//! The built-in search engine: BM25 and vectors over the vault's sections, kept
//! in `.kbx/search-index.sqlite`. See `docs/search-engine.md`.
//!
//! Rows are keyed by `sections.text_hash`, so a section is embedded when its
//! text is new and never again, however often its note is edited elsewhere.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use kbx_core::Graph;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use tracing::warn;

use crate::embed::{EmbedError, Embedder};
use crate::search::{Hit, Query, Search, SearchError};

/// Bumped when a row's meaning changes. A file of any other version is rebuilt.
const VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS texts (id INTEGER PRIMARY KEY, text_hash TEXT NOT NULL UNIQUE,
    vector BLOB);
CREATE VIRTUAL TABLE IF NOT EXISTS terms USING fts5(text, tokenize = 'porter unicode61');
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Sections embedded per transaction, so that stopping loses only this many.
const EMBED_BATCH: usize = 64;

/// Rows taken from each ranking before they're fused.
const POOL: usize = 200;

/// Words of a query kept for BM25: rare ones say the most, and each costs time.
const QUERY_TERMS: usize = 64;

/// The usual constant of reciprocal rank fusion.
const FUSION_K: f64 = 60.0;

/// What [`SearchIndex::sync`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Synced {
    /// Texts added to the BM25 index.
    pub added: usize,
    /// Texts dropped, no section saying them any more.
    pub removed: usize,
    /// Texts embedded.
    pub embedded: usize,
    /// Texts still without a vector.
    pub unembedded: usize,
}

/// Where a section is.
struct Place {
    note: String,
    key: String,
    path: String,
    start: usize,
    end: usize,
    text_hash: String,
}

/// The index file, which like every index can be deleted at any time.
#[derive(Debug)]
pub struct SearchIndex {
    connection: Connection,
}

impl SearchIndex {
    /// Opens the index at `path`. A file that can't be read is replaced.
    pub fn open(path: &Path) -> rusqlite::Result<SearchIndex> {
        let connection = prepare(path).or_else(|error| {
            warn!("{}: {error}; starting again", path.display());
            let _ = fs::remove_file(path);
            prepare(path)
        })?;
        Ok(SearchIndex { connection })
    }

    /// Opens the index without changing it, if there's a usable one.
    pub fn open_read_only(path: &Path) -> Option<SearchIndex> {
        let connection =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let version = connection.query_row("PRAGMA user_version", [], |row| row.get(0));
        (version.ok() == Some(VERSION)).then_some(SearchIndex { connection })
    }

    /// Whether a section's text has no vector from the model `model`, so that
    /// [`sync`](SearchIndex::sync) would have embedding to do.
    pub fn needs_embedding(&self, g: &Graph, model: &str) -> rusqlite::Result<bool> {
        if self.stored_model()?.as_deref() != Some(model) {
            return has_sections(g);
        }
        let embedded = self.embedded()?;
        Ok(places(g)?
            .iter()
            .any(|place| !embedded.contains(&place.text_hash)))
    }

    /// Brings the index up to the graph. `embedder`'s vectors are kept next to
    /// the text, and those of another model are dropped first, so with none
    /// the keyword half is still up to date. `progress` hears `(done, total)`.
    pub fn sync(
        &self,
        g: &Graph,
        vault: &Path,
        embedder: Option<&dyn Embedder>,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Synced, SyncError> {
        let places = places(g)?;
        let (added, removed) = self.sync_texts(vault, &places)?;
        let mut synced = Synced {
            added,
            removed,
            ..Synced::default()
        };
        if let Some(embedder) = embedder {
            if self.stored_model()?.as_deref() != Some(embedder.id()) {
                self.connection
                    .execute("UPDATE texts SET vector = NULL", [])?;
                self.connection.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('model', ?)",
                    [embedder.id()],
                )?;
            }
            synced.embedded = self.embed_missing(embedder, progress)?;
        }
        let count = "SELECT count(*) FROM texts WHERE vector IS NULL";
        let unembedded: i64 = self.connection.query_row(count, [], |row| row.get(0))?;
        synced.unembedded = unembedded as usize;
        Ok(synced)
    }

    fn stored_model(&self) -> rusqlite::Result<Option<String>> {
        let query = "SELECT value FROM meta WHERE key = 'model'";
        self.connection
            .query_row(query, [], |row| row.get(0))
            .optional()
    }

    fn embedded(&self) -> rusqlite::Result<HashSet<String>> {
        let query = "SELECT text_hash FROM texts WHERE vector IS NOT NULL";
        let mut statement = self.connection.prepare(query)?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// Adds the texts of new sections and drops those nothing says any more.
    /// Each file is read once, however many of its sections changed.
    fn sync_texts(&self, vault: &Path, places: &[Place]) -> Result<(usize, usize), SyncError> {
        let mut statement = self.connection.prepare("SELECT text_hash, id FROM texts")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let stored: HashMap<String, i64> = rows.collect::<rusqlite::Result<_>>()?;
        let wanted: HashSet<&str> = places.iter().map(|p| p.text_hash.as_str()).collect();

        let transaction = self.connection.unchecked_transaction()?;
        let mut removed = 0;
        for (hash, id) in &stored {
            if !wanted.contains(hash.as_str()) {
                transaction.execute("DELETE FROM texts WHERE id = ?", [id])?;
                transaction.execute("DELETE FROM terms WHERE rowid = ?", [id])?;
                removed += 1;
            }
        }

        let mut by_file: HashMap<&str, Vec<&Place>> = HashMap::new();
        let mut seen = HashSet::new();
        for place in places {
            if !stored.contains_key(&place.text_hash) && seen.insert(&place.text_hash) {
                by_file.entry(&place.path).or_default().push(place);
            }
        }
        let mut added = 0;
        for (file, sections) in by_file {
            let path = vault.join(file);
            let src = match fs::read_to_string(&path) {
                Ok(src) => src,
                Err(error) => {
                    warn!("{}: {error}", path.display());
                    continue;
                }
            };
            let lines: Vec<&str> = src.lines().collect();
            for place in sections {
                let text = section_text(&lines, place.start, place.end);
                transaction.execute(
                    "INSERT INTO texts (text_hash) VALUES (?)",
                    [&place.text_hash],
                )?;
                let id = transaction.last_insert_rowid();
                transaction.execute(
                    "INSERT INTO terms (rowid, text) VALUES (?, ?)",
                    params![id, text],
                )?;
                added += 1;
            }
        }
        transaction.commit()?;
        Ok((added, removed))
    }

    /// Longest last, so that a batch's texts are about the same length, which
    /// is what padding costs. Each batch is committed: stopping keeps the work.
    fn embed_missing(
        &self,
        embedder: &dyn Embedder,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<usize, SyncError> {
        let mut statement = self.connection.prepare(
            "SELECT texts.id, terms.text FROM texts JOIN terms ON terms.rowid = texts.id
             WHERE texts.vector IS NULL ORDER BY length(terms.text), texts.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let missing: Vec<(i64, String)> = rows.collect::<rusqlite::Result<_>>()?;

        let mut done = 0;
        for batch in missing.chunks(EMBED_BATCH) {
            let texts: Vec<&str> = batch.iter().map(|(_, text)| text.as_str()).collect();
            let vectors = embedder.embed(&texts)?;
            let transaction = self.connection.unchecked_transaction()?;
            for ((id, _), vector) in batch.iter().zip(&vectors) {
                transaction.execute(
                    "UPDATE texts SET vector = ? WHERE id = ?",
                    params![bytes(vector), id],
                )?;
            }
            transaction.commit()?;
            done += batch.len();
            progress(done, missing.len());
        }
        Ok(done)
    }

    /// A [`Search`] over the index as it is. `embedder` is only for free text:
    /// a note's neighbours come from the vectors stored here.
    pub fn searcher(
        self,
        g: &Graph,
        embedder: Option<Box<dyn Embedder>>,
    ) -> rusqlite::Result<BuiltinSearch> {
        let mut ids = HashMap::new();
        let mut vectors = Vec::new();
        let mut rows_of: Vec<i64> = Vec::new();
        let mut width = 0;
        {
            let mut statement = self
                .connection
                .prepare("SELECT id, text_hash, vector FROM texts")?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            for row in rows {
                let (id, hash, vector): (i64, String, Option<Vec<u8>>) = row?;
                ids.insert(hash, id);
                if let Some(vector) = vector {
                    let vector = floats(&vector);
                    width = vector.len();
                    vectors.extend(vector);
                    rows_of.push(id);
                }
            }
        }

        let mut sections: HashMap<i64, Vec<(String, String)>> = HashMap::new();
        let mut notes: HashMap<String, Vec<i64>> = HashMap::new();
        for place in places(g)? {
            if let Some(&id) = ids.get(&place.text_hash) {
                sections
                    .entry(id)
                    .or_default()
                    .push((place.note.clone(), place.key));
                notes.entry(place.note).or_default().push(id);
            }
        }
        let row_of = rows_of
            .iter()
            .enumerate()
            .map(|(row, &id)| (id, row))
            .collect();
        Ok(BuiltinSearch {
            connection: self.connection,
            sections,
            notes,
            vectors,
            width,
            rows_of,
            row_of,
            embedder,
        })
    }
}

fn prepare(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != VERSION {
        connection.execute_batch(
            "DROP TABLE IF EXISTS texts; DROP TABLE IF EXISTS terms; DROP TABLE IF EXISTS meta;",
        )?;
    }
    connection.execute_batch(SCHEMA)?;
    connection.pragma_update(None, "user_version", VERSION)?;
    Ok(connection)
}

/// Why the index couldn't be brought up to date.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// A database failed.
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
    /// The model failed.
    #[error(transparent)]
    Embed(#[from] EmbedError),
}

fn has_sections(g: &Graph) -> rusqlite::Result<bool> {
    let query = "SELECT EXISTS (SELECT 1 FROM sections)";
    g.connection().query_row(query, [], |row| row.get(0))
}

fn places(g: &Graph) -> rusqlite::Result<Vec<Place>> {
    let mut statement = g.connection().prepare(
        "SELECT sections.id, sections.key, files.path, sections.start_line, sections.end_line,
                sections.text_hash
         FROM sections JOIN files ON files.id = sections.id
         ORDER BY sections.id, sections.start_line",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Place {
            note: row.get(0)?,
            key: row.get(1)?,
            path: row.get(2)?,
            start: row.get::<_, i64>(3)? as usize,
            end: row.get::<_, i64>(4)? as usize,
            text_hash: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// Lines `start` to `end`, 1-based and inclusive, as one line.
fn section_text(lines: &[&str], start: usize, end: usize) -> String {
    let end = end.min(lines.len());
    let section = lines.get(start.saturating_sub(1)..end).unwrap_or_default();
    let words = section.iter().flat_map(|line| line.split_whitespace());
    words.collect::<Vec<_>>().join(" ")
}

fn bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// A BM25 query matching any of `text`'s words, the rarest kept if there are
/// too many. Quoting keeps FTS5 from reading a word as its query syntax.
fn match_query(text: &str, frequency: &dyn Fn(&str) -> i64) -> Option<String> {
    let lower = text.to_lowercase();
    let words = lower.split(|c: char| !c.is_alphanumeric());
    let mut words: Vec<&str> = words.filter(|word| word.chars().count() > 1).collect();
    words.sort_unstable();
    words.dedup();
    words.sort_by_cached_key(|word| frequency(word));
    words.truncate(QUERY_TERMS);
    let quoted: Vec<String> = words.iter().map(|word| format!("\"{word}\"")).collect();
    (!quoted.is_empty()).then(|| quoted.join(" OR "))
}

/// A [`Search`] over a [`SearchIndex`], with the vectors in memory: a search
/// reads them all, which at this size takes milliseconds.
pub struct BuiltinSearch {
    connection: Connection,
    /// Each text's sections, as `(note, key)`.
    sections: HashMap<i64, Vec<(String, String)>>,
    /// Each note's texts.
    notes: HashMap<String, Vec<i64>>,
    /// Every vector, one after another.
    vectors: Vec<f32>,
    width: usize,
    /// The text of each vector.
    rows_of: Vec<i64>,
    /// The vector of each text.
    row_of: HashMap<i64, usize>,
    embedder: Option<Box<dyn Embedder>>,
}

impl BuiltinSearch {
    fn row(&self, row: usize) -> &[f32] {
        &self.vectors[row * self.width..(row + 1) * self.width]
    }

    /// The mean of a note's section vectors: what the note as a whole is about.
    fn note_vector(&self, note: &str) -> Option<Vec<f32>> {
        let rows = self
            .notes
            .get(note)?
            .iter()
            .filter_map(|id| self.row_of.get(id));
        let mut mean = vec![0.0; self.width];
        let mut any = false;
        for &row in rows {
            any = true;
            for (sum, x) in mean.iter_mut().zip(self.row(row)) {
                *sum += x;
            }
        }
        any.then_some(mean)
    }

    /// The texts nearest `vector`, best first.
    fn nearest(&self, vector: &[f32]) -> Vec<i64> {
        let mut scored: Vec<(f32, i64)> = (0..self.rows_of.len())
            .map(|row| {
                let dot = self.row(row).iter().zip(vector).map(|(a, b)| a * b).sum();
                (dot, self.rows_of[row])
            })
            .collect();
        let by_score = |a: &(f32, i64), b: &(f32, i64)| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1));
        if scored.len() > POOL {
            scored.select_nth_unstable_by(POOL - 1, by_score);
            scored.truncate(POOL);
        }
        scored.sort_unstable_by(by_score);
        scored.into_iter().map(|(_, id)| id).collect()
    }

    /// The texts BM25 ranks best for `text`, best first.
    fn keywords(&self, text: &str) -> rusqlite::Result<Vec<i64>> {
        let frequency = |_: &str| 0;
        let Some(query) = match_query(text, &frequency) else {
            return Ok(Vec::new());
        };
        let mut statement = self
            .connection
            .prepare_cached("SELECT rowid FROM terms WHERE terms MATCH ? ORDER BY rank LIMIT ?")?;
        let rows = statement.query_map(params![query, POOL as i64], |row| row.get(0))?;
        rows.collect()
    }

    /// Each note's first section in `ranking`, in order, without `skip`.
    fn by_note<'a>(&'a self, ranking: &[i64], skip: Option<&str>) -> Vec<(&'a str, &'a str)> {
        let mut seen = HashSet::new();
        let mut notes = Vec::new();
        for id in ranking {
            for (note, key) in self.sections.get(id).into_iter().flatten() {
                if Some(note.as_str()) != skip && seen.insert(note.as_str()) {
                    notes.push((note.as_str(), key.as_str()));
                }
            }
        }
        notes
    }
}

impl Search for BuiltinSearch {
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError> {
        let failed = |error: String| SearchError::Failed(format!("the search index: {error}"));
        let (text, skip, vector) = match query {
            Query::Note { note, text } => (*text, Some(*note), self.note_vector(note)),
            Query::Text(text) => {
                let embedded = self.embedder.as_ref().map(|model| model.embed_query(text));
                (
                    *text,
                    None,
                    embedded.transpose().map_err(|e| failed(e.to_string()))?,
                )
            }
        };
        let keywords = self.keywords(text).map_err(|e| failed(e.to_string()))?;
        let nearest = vector.map(|vector| self.nearest(&vector));

        let mut rankings = vec![self.by_note(&keywords, skip)];
        rankings.extend(nearest.map(|ids| self.by_note(&ids, skip)));
        let best = rankings.len() as f64 / (FUSION_K + 1.0);
        let mut fused: HashMap<&str, (f64, &str)> = HashMap::new();
        // Vectors last, so that a note both found gets the section it's nearest.
        for ranking in &rankings {
            for (rank, (note, section)) in ranking.iter().enumerate() {
                let share = 1.0 / (FUSION_K + rank as f64 + 1.0);
                let entry = fused.entry(note).or_insert((0.0, section));
                entry.0 += share;
                entry.1 = section;
            }
        }
        let mut hits: Vec<Hit> = fused
            .into_iter()
            .map(|(note, (score, section))| Hit {
                note: note.to_string(),
                section: section.to_string(),
                score: score / best,
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.note.cmp(&b.note)));
        hits.truncate(limit);
        Ok(hits)
    }
}
