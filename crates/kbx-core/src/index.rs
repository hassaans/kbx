use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::graph::Graph;
use crate::note::{Note, ParseError, parse_note, split_frontmatter};
use crate::ontology::{Ontology, OntologyError};
use crate::vault::Resolver;

/// The `rel` of an edge from an inline `[[link]]`.
pub const MENTIONS: &str = "mentions";

/// Bump when the same file should compile to different rows, which makes the
/// next run rebuild.
const PIPELINE_VERSION: &str = "1";

const FRONTMATTER: &str = "frontmatter";
const INLINE: &str = "inline";
const DERIVED: &str = "derived";

/// What to index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexOptions {
    /// Ignore the stored hashes. With no `paths`, rebuilds every table that
    /// comes from the files; `candidates`, `verdicts` and `reviews` don't.
    pub full: bool,
    /// Limit the run to these files and folders, relative to the working
    /// directory. Empty means the whole vault.
    pub paths: Vec<PathBuf>,
}

/// What a run did. A file is counted once among `indexed`, `unchanged`,
/// `skipped` and `failed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexReport {
    /// Files compiled, because they are new or changed.
    pub indexed: usize,
    /// Files whose `content_hash` matched, which were left alone.
    pub unchanged: usize,
    /// Files whose rows were dropped: deleted, or no longer notes.
    pub removed: usize,
    /// Files that aren't notes, or claim an id an earlier path holds.
    pub skipped: usize,
    /// Files that could not be read or parsed.
    pub failed: usize,
    /// Links in the whole index that resolve to nothing (§8).
    pub hidden: usize,
}

/// Why a run stopped. One bad note never does this: it counts as `failed`.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The ontology is not valid.
    #[error(transparent)]
    Ontology(#[from] OntologyError),
    /// The database failed.
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
    /// A given path is not inside the vault.
    #[error("{0} is outside the vault")]
    OutsideVault(PathBuf),
    /// A given path is not on disk and nothing was indexed from it.
    #[error("{0} does not exist")]
    NotFound(PathBuf),
    /// The working directory could not be read.
    #[error("{0}: {1}")]
    Io(PathBuf, io::Error),
}

/// Compiles the markdown files under `vault` into `graph`, in one transaction.
///
/// A file whose `content_hash` matches is not parsed or written. Links are
/// stored as written, and `edges` is compared with what they now resolve to, so
/// a run only writes the rows that differ.
pub fn index_vault(
    graph: &mut Graph,
    vault: &Path,
    ontology_yaml: &str,
    options: &IndexOptions,
) -> Result<IndexReport, IndexError> {
    let ontology = Ontology::parse(ontology_yaml)?;
    let ontology_hash = sha256(ontology_yaml.as_bytes());
    let tx = graph
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;

    let stale = meta(&tx, "pipeline_version")?.is_some_and(|v| v != PIPELINE_VERSION);
    let selection = if stale {
        vec![Selected::whole_vault(vault)]
    } else {
        select(vault, &options.paths)?
    };
    let whole_vault = selection
        .iter()
        .any(|selected| selected.relative.is_empty());
    let rebuild = stale || (options.full && whole_vault);
    if rebuild {
        tx.execute_batch(
            "DELETE FROM files; DELETE FROM aliases; DELETE FROM sections;
             DELETE FROM blocks; DELETE FROM links; DELETE FROM edges;",
        )?;
    }

    let mut run = Run::new(&tx, options.full)?;
    let mut on_disk = BTreeSet::new();
    for selected in &selection {
        match fs::metadata(&selected.absolute) {
            Ok(metadata) if metadata.is_dir() => {
                walk(vault, &selected.relative, &mut on_disk);
            }
            Ok(_) if is_note_path(&selected.relative) => {
                on_disk.insert(selected.relative.clone());
            }
            Ok(_) => {}
            Err(_) if run.knows_under(&selected.relative) => {}
            Err(_) => return Err(IndexError::NotFound(selected.absolute.clone())),
        }
    }

    // Before anything is written, so a moved file's id is free for its new path.
    let selects = |path: &str| selection.iter().any(|s| s.contains(path));
    let gone: Vec<String> = run
        .by_path
        .keys()
        .filter(|path| selects(path) && !on_disk.contains(*path))
        .cloned()
        .collect();
    for path in gone {
        run.forget(&path)?;
    }
    for path in &on_disk {
        run.index_file(vault, path)?;
    }

    let mut report = run.report;
    if run.dirty || rebuild || meta(&tx, "ontology_hash")?.as_deref() != Some(&ontology_hash) {
        relink(&tx, &ontology)?;
    }
    set_meta(&tx, "ontology_hash", &ontology_hash)?;
    set_meta(&tx, "pipeline_version", PIPELINE_VERSION)?;
    let hidden: i64 = tx.query_row("SELECT COUNT(*) FROM links WHERE dst IS NULL", [], |row| {
        row.get(0)
    })?;
    report.hidden = hidden as usize;
    tx.commit()?;
    Ok(report)
}

/// A file or folder the run covers.
struct Selected {
    absolute: PathBuf,
    /// From the vault root with `/` separators, empty for the root itself.
    relative: String,
}

impl Selected {
    fn whole_vault(vault: &Path) -> Selected {
        Selected {
            absolute: vault.to_path_buf(),
            relative: String::new(),
        }
    }

    fn contains(&self, path: &str) -> bool {
        self.relative.is_empty()
            || path
                .strip_prefix(&self.relative)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

fn select(vault: &Path, paths: &[PathBuf]) -> Result<Vec<Selected>, IndexError> {
    if paths.is_empty() {
        return Ok(vec![Selected::whole_vault(vault)]);
    }
    let cwd = env::current_dir().map_err(|e| IndexError::Io(".".into(), e))?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let mut selection = Vec::new();
    for path in paths {
        // A deleted file can't be canonicalized, and is still a valid argument.
        let absolute = path
            .canonicalize()
            .unwrap_or_else(|_| normalize(&cwd.join(path)));
        let relative = absolute
            .strip_prefix(vault)
            .map_err(|_| IndexError::OutsideVault(path.clone()))?;
        selection.push(Selected {
            relative: slash_path(relative),
            absolute,
        });
    }
    Ok(selection)
}

/// Removes `.` and `..` without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean
}

fn slash_path(path: &Path) -> String {
    let parts: Vec<_> = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}

/// Collects the notes under `folder`. Hidden folders such as `.kbx` and
/// `.obsidian` are left out, and symlinks aren't followed, so a link back up
/// the tree can't loop. A folder that can't be read is as good as absent,
/// which is how §1 enforces ACLs.
fn walk(vault: &Path, folder: &str, found: &mut BTreeSet<String>) {
    let entries = match fs::read_dir(vault.join(folder)) {
        Ok(entries) => entries,
        Err(error) => {
            warn!("{folder}: {error}");
            return;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = match folder {
            "" => name.clone(),
            _ => format!("{folder}/{name}"),
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && !name.starts_with('.') => walk(vault, &path, found),
            Ok(kind) if kind.is_file() && is_note_path(&path) => {
                found.insert(path);
            }
            _ => {}
        }
    }
}

fn is_note_path(path: &str) -> bool {
    let visible = path.split('/').all(|part| !part.starts_with('.'));
    let extension = path.rsplit_once('.').map(|(_, extension)| extension);
    visible && extension.is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// A `files` row, as far as deciding what to do with a file needs it.
struct Indexed {
    id: String,
    content_hash: String,
    fm_hash: String,
}

/// The state of a run. `by_path` and `by_id` mirror `files` as it changes.
struct Run<'a> {
    tx: &'a Transaction<'a>,
    force: bool,
    now: i64,
    by_path: BTreeMap<String, Indexed>,
    by_id: HashMap<String, String>,
    report: IndexReport,
    /// Something links resolve against, or a link itself, changed.
    dirty: bool,
}

impl<'a> Run<'a> {
    fn new(tx: &'a Transaction<'a>, force: bool) -> rusqlite::Result<Run<'a>> {
        let mut statement = tx.prepare("SELECT path, id, content_hash, fm_hash FROM files")?;
        let rows = statement.query_map([], |row| {
            let file = Indexed {
                id: row.get(1)?,
                content_hash: row.get(2)?,
                fm_hash: row.get(3)?,
            };
            Ok((row.get::<_, String>(0)?, file))
        })?;
        let by_path = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        let by_id = by_path
            .iter()
            .map(|(path, file)| (file.id.clone(), path.clone()))
            .collect();
        drop(statement);
        Ok(Run {
            tx,
            force,
            now: unix_seconds(SystemTime::now()),
            by_path,
            by_id,
            report: IndexReport::default(),
            dirty: false,
        })
    }

    fn knows_under(&self, folder: &str) -> bool {
        let prefix = format!("{folder}/");
        self.by_path
            .keys()
            .any(|path| path == folder || path.starts_with(&prefix))
    }

    fn index_file(&mut self, vault: &Path, path: &str) -> rusqlite::Result<()> {
        let full = vault.join(path);
        let bytes = match fs::read(&full) {
            Ok(bytes) => bytes,
            Err(error) => return self.fail(path, error),
        };
        let content_hash = sha256(&bytes);
        let known = self.by_path.get(path);
        if !self.force && known.is_some_and(|file| file.content_hash == content_hash) {
            self.report.unchanged += 1;
            return Ok(());
        }
        let src = match std::str::from_utf8(&bytes) {
            Ok(src) => src,
            Err(error) => return self.fail(path, error),
        };
        match parse_note(path, src) {
            Ok(note) => {
                let mtime = fs::metadata(&full).and_then(|metadata| metadata.modified());
                let mtime = mtime.map_or(0, unix_seconds);
                self.write_note(&note, src, &content_hash, mtime)
            }
            Err(ParseError::NoFrontmatter) => {
                debug!("{path}: no frontmatter, skipping");
                self.report.skipped += 1;
                self.forget(path)
            }
            Err(error) => self.fail(path, error),
        }
    }

    /// A note that can't be read has no rows, as if its file weren't there.
    fn fail(&mut self, path: &str, error: impl Display) -> rusqlite::Result<()> {
        warn!("{path}: {error}");
        self.report.failed += 1;
        self.forget(path)
    }

    /// Drops a file's rows. Its edges go when [`relink`] runs.
    fn forget(&mut self, path: &str) -> rusqlite::Result<()> {
        let Some(file) = self.by_path.remove(path) else {
            return Ok(());
        };
        self.by_id.remove(&file.id);
        self.tx
            .execute("DELETE FROM files WHERE path = ?", [path])?;
        for table in ["aliases", "sections", "blocks"] {
            let sql = format!("DELETE FROM {table} WHERE id = ?");
            self.tx.execute(&sql, [&file.id])?;
        }
        self.tx
            .execute("DELETE FROM links WHERE src = ?", [&file.id])?;
        self.report.removed += 1;
        self.dirty = true;
        Ok(())
    }

    fn write_note(
        &mut self,
        note: &Note,
        src: &str,
        content_hash: &str,
        mtime: i64,
    ) -> rusqlite::Result<()> {
        let path = note.path.to_string_lossy().into_owned();
        if self.by_path.get(&path).is_some_and(|f| f.id != note.id) {
            self.forget(&path)?;
        }
        // `files.id` is unique. The lowest path keeps an id, wherever a run
        // starts from, so an incremental run agrees with a rebuild.
        if let Some(holder) = self.by_id.get(&note.id).filter(|h| **h != path).cloned() {
            if holder < path {
                warn!(
                    "{path}: id {} is already used by {holder}, skipping",
                    note.id
                );
                self.report.skipped += 1;
                return Ok(());
            }
            warn!("{holder}: id {} is also used by {path}, skipping", note.id);
            self.forget(&holder)?;
        }

        let fm_hash = match split_frontmatter(src) {
            Ok(Some(frontmatter)) => sha256(frontmatter.yaml.as_bytes()),
            _ => String::new(),
        };
        let known = self.by_path.get(&path);
        let fm_changed = self.force || known.is_none_or(|file| file.fm_hash != fm_hash);
        if known.is_none() {
            self.dirty = true;
        }

        let scope = path.split_once('/').map(|(scope, _)| scope);
        self.tx.execute(
            "INSERT INTO files (path, id, type, title, content_hash, fm_hash, mtime, scope, indexed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(path) DO UPDATE SET type = excluded.type, title = excluded.title,
                 content_hash = excluded.content_hash, fm_hash = excluded.fm_hash,
                 mtime = excluded.mtime, scope = excluded.scope, indexed_at = excluded.indexed_at",
            params![path, note.id, note.class, note.title, content_hash, fm_hash, mtime, scope, self.now],
        )?;
        if fm_changed {
            self.sync_aliases(note)?;
            let links = frontmatter_links(note);
            self.sync_links(&note.id, FRONTMATTER, &links)?;
        }
        self.sync_sections(note, src)?;
        self.sync_blocks(note)?;
        self.sync_links(&note.id, INLINE, &inline_links(note))?;

        self.by_id.insert(note.id.clone(), path.clone());
        self.by_path.insert(
            path,
            Indexed {
                id: note.id.clone(),
                content_hash: content_hash.to_string(),
                fm_hash,
            },
        );
        self.report.indexed += 1;
        Ok(())
    }

    fn sync_aliases(&mut self, note: &Note) -> rusqlite::Result<()> {
        let wanted: BTreeSet<&String> = note.aliases.iter().collect();
        let mut statement = self.tx.prepare("SELECT alias FROM aliases WHERE id = ?")?;
        let rows = statement.query_map([&note.id], |row| row.get::<_, String>(0))?;
        let stored = rows.collect::<rusqlite::Result<BTreeSet<_>>>()?;
        if wanted.iter().copied().eq(stored.iter()) {
            return Ok(());
        }
        self.tx
            .execute("DELETE FROM aliases WHERE id = ?", [&note.id])?;
        for alias in wanted {
            self.tx.execute(
                "INSERT INTO aliases (alias, id) VALUES (?, ?)",
                [alias, &note.id],
            )?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Writes only the sections that differ, so an edit to one section leaves
    /// the rows of the others alone.
    fn sync_sections(&self, note: &Note, src: &str) -> rusqlite::Result<()> {
        let mut statement = self.tx.prepare(
            "SELECT key, heading_path, block_id, start_line, end_line, text_hash, tokens
             FROM sections WHERE id = ?",
        )?;
        let rows = statement.query_map([&note.id], |row| {
            let section = SectionRow {
                heading_path: row.get(1)?,
                block_id: row.get(2)?,
                start_line: row.get(3)?,
                end_line: row.get(4)?,
                text_hash: row.get(5)?,
                tokens: row.get(6)?,
            };
            Ok((row.get::<_, String>(0)?, section))
        })?;
        let mut stored = rows.collect::<rusqlite::Result<HashMap<_, _>>>()?;

        for section in &note.sections {
            let key = section.key.to_string();
            let text = &src[section.byte_range.clone()];
            let row = SectionRow {
                heading_path: section.slug_path.clone(),
                block_id: section.heading_block().map(|block| block.id.clone()),
                start_line: section.start_line as i64,
                end_line: section.end_line as i64,
                text_hash: sha256(text.as_bytes()),
                tokens: estimate_tokens(text),
            };
            if stored.remove(&key).as_ref() == Some(&row) {
                continue;
            }
            self.tx.execute(
                "INSERT INTO sections (key, id, heading_path, block_id, start_line, end_line, text_hash, tokens)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(key) DO UPDATE SET heading_path = excluded.heading_path,
                     block_id = excluded.block_id, start_line = excluded.start_line,
                     end_line = excluded.end_line, text_hash = excluded.text_hash,
                     tokens = excluded.tokens",
                params![
                    key, note.id, row.heading_path, row.block_id, row.start_line, row.end_line,
                    row.text_hash, row.tokens
                ],
            )?;
        }
        for key in stored.keys() {
            self.tx
                .execute("DELETE FROM sections WHERE key = ?", [key])?;
        }
        Ok(())
    }

    fn sync_blocks(&self, note: &Note) -> rusqlite::Result<()> {
        // An anchor can only mean one block, so the first with an id keeps it.
        let mut wanted: BTreeMap<String, (String, i64, i64)> = BTreeMap::new();
        for section in &note.sections {
            for block in &section.blocks {
                wanted.entry(block.id.clone()).or_insert((
                    section.key.to_string(),
                    block.start_line as i64,
                    block.end_line as i64,
                ));
            }
        }
        let mut statement = self.tx.prepare(
            "SELECT block_id, section_key, start_line, end_line FROM blocks WHERE id = ?",
        )?;
        let rows = statement.query_map([&note.id], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
        })?;
        let stored = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        if stored == wanted {
            return Ok(());
        }
        self.tx
            .execute("DELETE FROM blocks WHERE id = ?", [&note.id])?;
        for (block_id, (section_key, start_line, end_line)) in &wanted {
            self.tx.execute(
                "INSERT INTO blocks (id, block_id, section_key, start_line, end_line)
                 VALUES (?, ?, ?, ?, ?)",
                params![note.id, block_id, section_key, start_line, end_line],
            )?;
        }
        Ok(())
    }

    fn sync_links(&mut self, src: &str, origin: &str, wanted: &[LinkRow]) -> rusqlite::Result<()> {
        let mut statement = self.tx.prepare(
            "SELECT key, target, fragment, src_section, line FROM links
             WHERE src = ? AND origin = ? ORDER BY rowid",
        )?;
        let rows = statement.query_map([src, origin], |row| {
            Ok(LinkRow {
                key: row.get(0)?,
                target: row.get(1)?,
                fragment: row.get(2)?,
                src_section: row.get(3)?,
                line: row.get(4)?,
            })
        })?;
        let stored = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if stored == wanted {
            return Ok(());
        }
        self.tx.execute(
            "DELETE FROM links WHERE src = ? AND origin = ?",
            [src, origin],
        )?;
        for link in wanted {
            self.tx.execute(
                "INSERT INTO links (src, key, target, fragment, origin, src_section, line)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    src,
                    link.key,
                    link.target,
                    link.fragment,
                    origin,
                    link.src_section,
                    link.line
                ],
            )?;
        }
        self.dirty = true;
        Ok(())
    }
}

#[derive(PartialEq)]
struct SectionRow {
    heading_path: String,
    block_id: Option<String>,
    start_line: i64,
    end_line: i64,
    text_hash: String,
    tokens: i64,
}

#[derive(PartialEq)]
struct LinkRow {
    key: String,
    target: String,
    fragment: Option<String>,
    src_section: Option<String>,
    line: i64,
}

fn frontmatter_links(note: &Note) -> Vec<LinkRow> {
    let links = note.relations.iter().flat_map(|entry| {
        entry.links.iter().map(|link| LinkRow {
            key: entry.key.clone(),
            target: link.target.clone(),
            fragment: link.fragment.clone(),
            src_section: None,
            line: link.line as i64,
        })
    });
    links.collect()
}

fn inline_links(note: &Note) -> Vec<LinkRow> {
    let links = note.sections.iter().flat_map(|section| {
        section.mentions.iter().map(|link| LinkRow {
            key: MENTIONS.to_string(),
            target: link.target.clone(),
            fragment: link.fragment.clone(),
            src_section: Some(section.key.to_string()),
            line: link.line as i64,
        })
    });
    links.collect()
}

type EdgeKey = (String, String, String, &'static str);

#[derive(PartialEq)]
struct EdgeRow {
    inverse_of: Option<String>,
    weight: Option<f64>,
    src_section: Option<String>,
    line: Option<i64>,
}

/// Resolves every stored link again and brings `edges` in line with the
/// result. Resolution depends on the whole vault, so this can't be limited to
/// the files that changed.
fn relink(tx: &Transaction<'_>, ontology: &Ontology) -> rusqlite::Result<()> {
    let (resolver, ids) = load_resolver(tx)?;
    let mut wanted: BTreeMap<EdgeKey, EdgeRow> = BTreeMap::new();
    let mut moved = Vec::new();

    let mut statement = tx.prepare(
        "SELECT rowid, src, key, target, origin, src_section, line, dst FROM links
         ORDER BY src, line, rowid",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (rowid, src, key): (i64, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let target: String = row.get(3)?;
        let origin: String = row.get(4)?;
        let src_section: Option<String> = row.get(5)?;
        let line: i64 = row.get(6)?;
        let stored: Option<String> = row.get(7)?;

        let dst = resolver.resolve(&target).map(|(index, _)| &ids[index]);
        if dst != stored.as_ref() {
            moved.push((rowid, dst.cloned()));
        }
        let Some(dst) = dst else { continue };

        let (origin, inverse_of, weight) = if origin == INLINE {
            (INLINE, None, None)
        } else {
            let relation = ontology.lookup(&key);
            let inverse_of = relation.filter(|r| r.inverted).map(|r| r.name.to_string());
            (FRONTMATTER, inverse_of, relation.map(|r| r.relation.weight))
        };
        // Several links to one note are one edge; the first says where.
        wanted
            .entry((src, dst.clone(), key, origin))
            .or_insert(EdgeRow {
                inverse_of,
                weight,
                src_section,
                line: Some(line),
            });
    }
    drop(rows);
    drop(statement);
    for (rowid, dst) in moved {
        tx.execute(
            "UPDATE links SET dst = ? WHERE rowid = ?",
            params![dst, rowid],
        )?;
    }

    derive_inverses(&mut wanted, ontology);
    write_edges(tx, &wanted)
}

fn load_resolver(tx: &Transaction<'_>) -> rusqlite::Result<(Resolver, Vec<String>)> {
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    let mut statement = tx.prepare("SELECT id, alias FROM aliases")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        aliases.entry(row.get(0)?).or_default().push(row.get(1)?);
    }
    drop(rows);

    let mut resolver = Resolver::default();
    let mut ids = Vec::new();
    let mut statement = tx.prepare("SELECT id, path FROM files")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (id, path): (String, String) = (row.get(0)?, row.get(1)?);
        let names = aliases.get(&id).map_or(&[][..], Vec::as_slice);
        resolver.add(ids.len(), &id, Path::new(&path), names);
        ids.push(id);
    }
    Ok((resolver, ids))
}

/// Adds the mirror of each frontmatter edge, unless the target note states it
/// itself. `inverse_of` is set on a row that reads a relation backwards.
fn derive_inverses(wanted: &mut BTreeMap<EdgeKey, EdgeRow>, ontology: &Ontology) {
    let mut derived = Vec::new();
    for ((src, dst, rel, origin), row) in wanted.iter() {
        let Some(relation) = ontology.lookup(rel).filter(|_| *origin == FRONTMATTER) else {
            continue;
        };
        let (mirror, inverse_of) = if relation.inverted {
            (relation.name, None)
        } else {
            (relation.relation.inverse.as_str(), Some(relation.name))
        };
        let stated = (dst.clone(), src.clone(), mirror.to_string(), FRONTMATTER);
        if !wanted.contains_key(&stated) {
            let edge = EdgeRow {
                inverse_of: inverse_of.map(str::to_string),
                weight: row.weight,
                src_section: None,
                line: None,
            };
            derived.push((
                (dst.clone(), src.clone(), mirror.to_string(), DERIVED),
                edge,
            ));
        }
    }
    wanted.extend(derived);
}

/// Writes the difference between `wanted` and `edges`.
fn write_edges(tx: &Transaction<'_>, wanted: &BTreeMap<EdgeKey, EdgeRow>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare(
        "SELECT src, dst, rel, origin, inverse_of, weight, src_section, line FROM edges",
    )?;
    let rows = statement.query_map([], |row| {
        let edge = EdgeRow {
            inverse_of: row.get(4)?,
            weight: row.get(5)?,
            src_section: row.get(6)?,
            line: row.get(7)?,
        };
        let key: (String, String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        Ok((key, edge))
    })?;
    let mut stored = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;

    for ((src, dst, rel, origin), edge) in wanted {
        let key = (src.clone(), dst.clone(), rel.clone(), origin.to_string());
        if stored.remove(&key).as_ref() == Some(edge) {
            continue;
        }
        tx.execute(
            "INSERT INTO edges (src, dst, rel, inverse_of, weight, origin, src_section, line)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(src, dst, rel, origin) DO UPDATE SET inverse_of = excluded.inverse_of,
                 weight = excluded.weight, src_section = excluded.src_section, line = excluded.line",
            params![src, dst, rel, edge.inverse_of, edge.weight, origin, edge.src_section, edge.line],
        )?;
    }
    for (src, dst, rel, origin) in stored.keys() {
        tx.execute(
            "DELETE FROM edges WHERE src = ? AND dst = ? AND rel = ? AND origin = ?",
            [src, dst, rel, origin],
        )?;
    }
    Ok(())
}

fn meta(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row("SELECT v FROM meta WHERE k = ?", [key], |row| row.get(0))
        .optional()
}

fn set_meta(tx: &Transaction<'_>, key: &str, value: &str) -> rusqlite::Result<()> {
    if meta(tx, key)?.as_deref() != Some(value) {
        tx.execute(
            "INSERT INTO meta (k, v) VALUES (?, ?)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            [key, value],
        )?;
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// About four characters to a token, which is close enough for budgets.
fn estimate_tokens(text: &str) -> i64 {
    text.chars().count().div_ceil(4) as i64
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}
