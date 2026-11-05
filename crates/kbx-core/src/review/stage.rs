//! Staged proposals (§7): candidates written under `kbx.proposed` so that an
//! editor shows them, and the edits made there taken back as decisions.
//!
//! The candidates stay in their table, so the queue is the same either way.
//! Staging adds a copy in the note and a row in `staged` saying so. A decision
//! made in the editor is one of [`accept`](super::accept)'s or [`reject`]'s,
//! with the same guards and strikes. See `docs/staging.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tracing::warn;

use super::{
    Accepted, Accepting, Candidate, Checked, Decision, OnDisk, Queue, Rejected, ReviewError,
    follow_hash, reject, write_accepted,
};
use crate::graph::Graph;
use crate::index::{load_resolver, sha256};
use crate::ontology::{NONE, Ontology};
use crate::vault::Resolver;
use crate::writer::{
    Staged, StagedEntry, WriteError, proposed_ids, set_reviewed, stage_entries, unstage_entries,
    user_content,
};

/// The verdict that makes `judge` one of an entry's `by`.
const ACCEPT: &str = "accept";

/// What `by` calls a judge that agreed (§3).
const JUDGE: &str = "judge";

/// Why a removed line was rejected, kept in `reviews`.
const REMOVED: &str = "removed from kbx.proposed";

/// Why a moved line was accepted, kept in `reviews`.
const MOVED: &str = "moved out of kbx.proposed";

/// What [`stage`] did.
#[derive(Debug, Default)]
pub struct StageReport {
    /// Entries now in notes that weren't before.
    pub staged: usize,
    /// Files changed, relative to the vault.
    pub written: Vec<PathBuf>,
    /// Notes that took no entries, by id, and why. The candidates stand.
    pub refused: Vec<(String, ReviewError)>,
    /// Notes left alone because they have inferred keys. Their candidates stay
    /// in the queue, for `kbx review`.
    pub inferred: usize,
}

/// What [`sync_stage`] did.
#[derive(Debug, Default)]
pub struct Synced {
    /// Lines moved into a relation key, by cid.
    pub accepted: Vec<(String, Accepted)>,
    /// Lines removed, by cid.
    pub rejected: Vec<(String, Rejected)>,
    /// Lines whose candidate was decided or dropped elsewhere.
    pub cleared: usize,
    /// Lines still waiting for a decision, or on a note that can't be read.
    pub pending: usize,
    /// Lines that couldn't be taken as decided, by cid, and why. They stay in
    /// `staged`, so the next run tries again.
    pub refused: Vec<(String, ReviewError)>,
    /// Files changed, relative to the vault.
    pub written: Vec<PathBuf>,
    /// Notes this run took a decision from and left with no line to decide.
    pub finished: Vec<String>,
}

/// Writes these candidates under `kbx.proposed`, in the note a relation would
/// be written to, leaving out what the queue leaves out and what's there.
///
/// A note with inferred keys takes no entries: staging isn't an acceptance, and
/// only an acceptance writes to such a note. One note refusing doesn't stop the
/// rest. Candidates sharing a staged note
/// stay acceptable: their recorded hash follows the write. The index doesn't,
/// until `kbx index` runs.
pub fn stage(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    cids: &[String],
) -> rusqlite::Result<StageReport> {
    let wanted: HashSet<&str> = cids.iter().map(String::as_str).collect();
    let held = rows(g.connection())?;
    let held: HashSet<&str> = held.iter().map(|row| row.cid.as_str()).collect();

    let queue = Queue::load(g, ontology)?;
    let mut by_note: BTreeMap<String, Vec<Line>> = BTreeMap::new();
    for item in queue.items() {
        let cid = item.cid.as_str();
        let decided = held.contains(cid) || item.unwritten;
        if !wanted.contains(cid) || decided || item.src_class.is_none() {
            continue;
        }
        let (note, target, section) = match item.swapped(ontology) {
            true => (&item.dst, &item.src, &item.src_section),
            false => (&item.src, &item.dst, &item.dst_section),
        };
        let rel = item.rel.as_deref().unwrap_or(NONE);
        let rel = ontology.lookup(rel).map_or(rel, |key| key.name);
        let mut by = item.generators.clone();
        if item.verdict.as_deref() == Some(ACCEPT) {
            by.push(JUDGE.to_string());
        }
        by_note.entry(note.clone()).or_default().push(Line {
            target: target.clone(),
            section: section.clone(),
            entry: StagedEntry {
                rel: rel.to_string(),
                to: format!("[[{target}]]"),
                by,
                score: (item.score * 100.0).round() / 100.0,
                id: item.cid.clone(),
            },
        });
    }

    let mut report = StageReport::default();
    let links = load_resolver(g.connection())?;
    for (note, lines) in by_note {
        match stage_note(g, vault, &note, &lines, &links) {
            Ok(written) => {
                report.staged += lines.len();
                report.written.extend(written);
            }
            Err(ReviewError::Inferred(_)) => report.inferred += 1,
            Err(error) => report.refused.push((note, error)),
        }
    }
    Ok(report)
}

/// An entry on its way into a note.
struct Line {
    target: String,
    /// The section of the target it's about, if any.
    section: Option<String>,
    entry: StagedEntry,
}

impl Line {
    /// The entry as it goes in the note. `to` is the link a relation would hold,
    /// so that moving the line under a relation key keeps the anchor.
    fn linked(
        &self,
        connection: &Connection,
        vault: &Path,
        (resolver, ids): &(Resolver, Vec<String>),
    ) -> Result<StagedEntry, ReviewError> {
        let mut entry = self.entry.clone();
        if let Some(file) = OnDisk::read(connection, vault, &self.target)? {
            let hash = file.hash.clone();
            let names = |target: &str| {
                let found = resolver.resolve(target);
                found.is_some_and(|(index, _)| ids[index] == self.target)
            };
            let file = file.checked(Some(&hash))?;
            let link = file.link_target(self.section.as_deref(), names);
            entry.to = format!("[[{link}]]");
        }
        Ok(entry)
    }
}

fn stage_note(
    g: &Graph,
    vault: &Path,
    note: &str,
    lines: &[Line],
    links: &(Resolver, Vec<String>),
) -> Result<Option<PathBuf>, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let file = OnDisk::read(&tx, vault, note)?
        .ok_or_else(|| ReviewError::SourceUnavailable(note.to_string()))?;
    for line in lines {
        let candidate = Candidate::load(&tx, &line.entry.id)?;
        let recorded = match candidate.src == note {
            true => candidate.src_hash,
            false => candidate.dst_hash,
        };
        if recorded.as_deref() != Some(file.hash.as_str()) {
            return Err(ReviewError::Changed(file.path));
        }
    }
    let hash = file.hash.clone();
    let file = file.checked(Some(&hash))?;
    if file.note.inferred.any() {
        return Err(ReviewError::Inferred(file.path));
    }

    let entries = lines
        .iter()
        .map(|line| line.linked(&tx, vault, links))
        .collect::<Result<Vec<StagedEntry>, ReviewError>>()?;
    let new = stage_entries(&file.text, &entries)
        .map_err(|error| ReviewError::Write(file.path.clone(), error))?;
    let text = new.as_deref().unwrap_or(&file.text);
    follow(&tx, note, &hash, text)?;
    // The file is what every other line on it was staged into, by the check
    // above, even if a plain `kbx suggest` has moved their hashes along.
    tx.execute(
        "UPDATE staged SET hash = ?, content = ? WHERE note = ?",
        params![sha256(text.as_bytes()), user_content(text), note],
    )?;
    for line in lines {
        tx.execute(
            "INSERT INTO staged (cid, note, target, rel, hash, content)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                line.entry.id,
                note,
                line.target,
                line.entry.rel,
                sha256(text.as_bytes()),
                user_content(text)
            ],
        )?;
    }
    let written = commit_write(tx, vault, &file, new)?;
    Ok(written)
}

/// Renames `text` over the file if there is one, then commits.
fn commit_write(
    tx: Transaction<'_>,
    vault: &Path,
    file: &Checked,
    text: Option<String>,
) -> Result<Option<PathBuf>, ReviewError> {
    let io = |error| ReviewError::Io(file.path.clone(), error);
    if let Some(text) = &text {
        let staged = Staged::new(&vault.join(&file.path), text.as_bytes()).map_err(io)?;
        staged.commit().map_err(io)?;
    }
    tx.commit()?;
    Ok(text.map(|_| file.path.clone()))
}

/// Takes a candidate's line out of its note and out of `staged`, recording no
/// decision: for a candidate decided elsewhere. Returns the file it changed.
///
/// A note that's gone has no line left to take out. One that doesn't parse
/// does, so its row stays for [`sync_stage`] to come back to.
pub fn unstage(g: &Graph, vault: &Path, cid: &str) -> Result<Option<PathBuf>, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let Some(note) = holder(&tx, cid)? else {
        return Ok(None);
    };
    let Some(file) = OnDisk::read(&tx, vault, &note)? else {
        tx.execute("DELETE FROM staged WHERE cid = ?", [cid])?;
        tx.commit()?;
        return Ok(None);
    };
    let hash = file.hash.clone();
    let Ok(file) = file.checked(Some(&hash)) else {
        return Ok(None);
    };
    let new = unstage_entries(&file.text, &[cid])
        .map_err(|error| ReviewError::Write(file.path.clone(), error))?;
    tx.execute("DELETE FROM staged WHERE cid = ?", [cid])?;
    if let Some(text) = &new {
        follow(&tx, &note, &hash, text)?;
    }
    commit_write(tx, vault, &file, new)
}

/// Takes the edits made under `kbx.proposed` as decisions.
///
/// `staged` says what kbx wrote, and for each line:
///
/// - A link between the two notes under a relation key is an acceptance of that
///   relation, read the way it's written, whether or not the line is still
///   there. The links are the person's, so the hash guard doesn't apply to it;
///   the file must still match the index, which should be current.
/// - A line that's gone is a [`reject`] of the relation it proposed.
/// - A line whose candidate is gone was decided elsewhere, and is removed.
///
/// A note that's gone or doesn't parse says nothing, and neither does a list
/// that no longer reads as proposals: those lines wait.
pub fn sync_stage(g: &Graph, vault: &Path, ontology: &Ontology) -> rusqlite::Result<Synced> {
    let (resolver, ids) = {
        let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Deferred)?;
        load_resolver(&tx)?
    };
    let names = Names { resolver, ids };

    // First, while every row is there: a decision takes its row with it, and a
    // note's last row is what says the candidates at that note can follow it.
    follow_removals(g, vault)?;

    let mut synced = Synced::default();
    let mut decided: HashSet<String> = HashSet::new();
    let mut ours = HashMap::new();
    for row in rows(g.connection())? {
        let outcome = sync_row(g, vault, ontology, &names, &ours, &row);
        // The index doesn't follow until the run ends, so what this run wrote
        // would look like someone else's edit to the next line on that note.
        if outcome.as_ref().is_ok_and(Outcome::wrote) {
            for id in [&row.note, &row.target] {
                if let Some(file) = OnDisk::read(g.connection(), vault, id)? {
                    ours.insert(id.clone(), file.hash);
                }
            }
        }
        match outcome {
            Ok(Outcome::Accepted(accepted)) => {
                synced.written.extend(accepted.written.iter().cloned());
                synced.accepted.push((row.cid, accepted));
                decided.insert(row.note);
            }
            Ok(Outcome::Rejected(rejected, written)) => {
                synced.written.extend(written);
                synced.rejected.push((row.cid, rejected));
                decided.insert(row.note);
            }
            Ok(Outcome::Cleared(written)) => {
                synced.written.extend(written);
                synced.cleared += 1;
            }
            Ok(Outcome::Pending) => synced.pending += 1,
            Err(error) => synced.refused.push((row.cid, error)),
        }
    }

    let left = rows(g.connection())?;
    let left: HashSet<&str> = left.iter().map(|row| row.note.as_str()).collect();
    synced.finished = decided
        .into_iter()
        .filter(|note| !left.contains(note.as_str()))
        .collect();
    synced.finished.sort();
    Ok(synced)
}

/// What a link target resolves to, as of the start of a sync.
struct Names {
    resolver: Resolver,
    ids: Vec<String>,
}

impl Names {
    /// The first relation key on `from` that links to `to`, known or not.
    fn key<'a>(&self, from: &'a Checked, to: &Checked, ontology: &Ontology) -> Option<&'a str> {
        let links_to = |target: &str| {
            let found = self.resolver.resolve(target);
            found.is_some_and(|(index, _)| self.ids[index] == to.note.id)
        };
        let mut keys = from
            .note
            .relations
            .iter()
            .filter(|entry| entry.links.iter().any(|link| links_to(&link.target)));
        let first = keys.clone().next();
        let known = keys.find(|entry| ontology.lookup(&entry.key).is_some());
        known.or(first).map(|entry| entry.key.as_str())
    }
}

enum Outcome {
    Accepted(Accepted),
    Rejected(Rejected, Option<PathBuf>),
    Cleared(Option<PathBuf>),
    Pending,
}

impl Outcome {
    fn wrote(&self) -> bool {
        match self {
            Outcome::Accepted(accepted) => !accepted.written.is_empty(),
            Outcome::Rejected(_, written) | Outcome::Cleared(written) => written.is_some(),
            Outcome::Pending => false,
        }
    }
}

fn sync_row(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    names: &Names,
    ours: &HashMap<String, String>,
    row: &Row,
) -> Result<Outcome, ReviewError> {
    let connection = g.connection();
    let exists = match Candidate::load(connection, &row.cid) {
        Ok(_) => true,
        Err(ReviewError::UnknownCandidate(_)) => false,
        Err(error) => return Err(error),
    };
    let note = read(connection, vault, &row.note)?;
    let target = read(connection, vault, &row.target)?;
    let (Some(note), Some(target)) = (note, target) else {
        return match exists {
            true => Ok(Outcome::Pending),
            false => Ok(Outcome::Cleared(unstage(g, vault, &row.cid)?)),
        };
    };
    let Some(held) = proposed_ids(&note.note) else {
        warn!("{}: kbx.proposed can't be read", note.path.display());
        return Ok(Outcome::Pending);
    };

    let written = [(&note, &target), (&target, &note)]
        .into_iter()
        .find_map(|(from, to)| Some((from, to, names.key(from, to, ontology)?)));
    if let Some((from, to, key)) = written {
        let pair = (from, to);
        return accept_as_written(g, vault, ontology, ours, &row.cid, pair, key);
    }
    match (held.contains(&row.cid), exists) {
        (true, true) => Ok(Outcome::Pending),
        (false, true) => {
            let decision = Decision {
                rel: Some(&row.rel),
                reason: Some(REMOVED),
            };
            let rejected = reject(g, ontology, &row.cid, decision)?;
            Ok(Outcome::Rejected(rejected, unstage(g, vault, &row.cid)?))
        }
        (_, false) => Ok(Outcome::Cleared(unstage(g, vault, &row.cid)?)),
    }
}

fn accept_as_written(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    ours: &HashMap<String, String>,
    cid: &str,
    (from, to): (&Checked, &Checked),
    key: &str,
) -> Result<Outcome, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let trusted = |id: &str| -> rusqlite::Result<Option<String>> {
        if let Some(hash) = ours.get(id) {
            return Ok(Some(hash.clone()));
        }
        tx.query_row("SELECT content_hash FROM files WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .optional()
    };
    // The candidate may be gone, and it may read the other way round.
    let stored = match Candidate::load(&tx, cid) {
        Ok(stored) => Some(stored),
        Err(ReviewError::UnknownCandidate(_)) => None,
        Err(error) => return Err(error),
    };
    let section = |id: &str| {
        stored.as_ref().and_then(|stored| match stored.src == id {
            true => stored.src_section.clone(),
            false => stored.dst_section.clone(),
        })
    };
    let candidate = Candidate {
        cid: cid.to_string(),
        src: from.note.id.clone(),
        dst: to.note.id.clone(),
        src_hash: trusted(&from.note.id)?,
        dst_hash: trusted(&to.note.id)?,
        src_section: section(&from.note.id),
        dst_section: section(&to.note.id),
        accepted_rel: stored.as_ref().and_then(|s| s.accepted_rel.clone()),
    };
    let accepting = Accepting {
        rel: key,
        reason: Some(MOVED),
        as_written: true,
        preview: None,
    };
    let accepted = write_accepted(&tx, vault, ontology, cid, &candidate, accepting)?;
    tx.commit()?;
    Ok(Outcome::Accepted(accepted))
}

/// Lets the remaining candidates of a note past an edit that only touched the
/// tool namespaces, as removing a line does, so that they can still be
/// accepted. An edit to anything else trips the hash guard as it always does.
fn follow_removals(g: &Graph, vault: &Path) -> rusqlite::Result<()> {
    let mut seen: HashMap<String, String> = HashMap::new();
    for row in rows(g.connection())? {
        if seen.insert(row.note.clone(), row.hash.clone()).as_ref() == Some(&row.hash) {
            continue;
        }
        let Some(file) = OnDisk::read(g.connection(), vault, &row.note)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(file.bytes) else {
            continue;
        };
        if file.hash != row.hash && user_content(&text) == row.content {
            let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
            follow(&tx, &row.note, &row.hash, &text)?;
            tx.commit()?;
        }
    }
    Ok(())
}

/// Sets `kbx.reviewed` on a note, which locks it from new proposals until its
/// content changes. Returns the file, if that wrote to it.
pub fn mark_reviewed(
    g: &Graph,
    vault: &Path,
    note: &str,
    date: &str,
) -> Result<Option<PathBuf>, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let file =
        read(&tx, vault, note)?.ok_or_else(|| ReviewError::SourceUnavailable(note.to_string()))?;
    let new = set_reviewed(&file.text, date)
        .map_err(|error: WriteError| ReviewError::Write(file.path.clone(), error))?;
    let text = new.as_deref().unwrap_or(&file.text);
    follow(&tx, note, &file.hash, text)?;
    // Here and not left to the index: the same date, marked again after an
    // edit, is no news to it.
    tx.execute(
        "INSERT INTO locks (id, reviewed, baseline, current) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(id) DO UPDATE SET reviewed = excluded.reviewed,
             baseline = excluded.baseline, current = excluded.current, held = 0",
        params![note, date, user_content(text)],
    )?;
    commit_write(tx, vault, &file, new)
}

/// [`mark_reviewed`], with the lock kept in `.kbx/` and the note left alone. It
/// works the same: the note's content lifts it.
pub fn hold_reviewed(g: &Graph, vault: &Path, note: &str, date: &str) -> Result<(), ReviewError> {
    let connection = g.connection();
    let file = read(connection, vault, note)?
        .ok_or_else(|| ReviewError::SourceUnavailable(note.to_string()))?;
    connection.execute(
        "INSERT INTO locks (id, reviewed, baseline, current, held) VALUES (?1, ?2, ?3, ?3, 1)
         ON CONFLICT(id) DO UPDATE SET reviewed = excluded.reviewed,
             baseline = excluded.baseline, current = excluded.current, held = 1",
        params![note, date, user_content(&file.text)],
    )?;
    Ok(())
}

/// The notes under a `kbx.reviewed` lock: reviewed, and unchanged since.
pub fn locked_notes(connection: &Connection) -> rusqlite::Result<HashSet<String>> {
    // `--dry-run` opens an older database as it is, without the table.
    let tables = "SELECT COUNT(*) FROM sqlite_master WHERE name = 'locks'";
    if connection.query_row(tables, [], |row| row.get::<_, i64>(0))? == 0 {
        return Ok(HashSet::new());
    }
    let mut statement = connection.prepare("SELECT id FROM locks WHERE baseline = current")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect()
}

/// The note holding a candidate's line, if it has one.
pub(super) fn holder(connection: &Connection, cid: &str) -> rusqlite::Result<Option<String>> {
    connection
        .query_row("SELECT note FROM staged WHERE cid = ?", [cid], |row| {
            row.get(0)
        })
        .optional()
}

/// Moves what's recorded about a note along with a write of `text` over the
/// file hashed `old`, which is the only change to it.
pub(super) fn follow(
    tx: &Transaction<'_>,
    note: &str,
    old: &str,
    text: &str,
) -> rusqlite::Result<()> {
    let new = sha256(text.as_bytes());
    follow_hash(tx, note, old, &new)?;
    tx.execute(
        "UPDATE staged SET hash = ?, content = ? WHERE note = ? AND hash = ?",
        params![new, user_content(text), note, old],
    )?;
    Ok(())
}

/// A note as it is now, or `None` if it's gone or doesn't parse.
fn read(connection: &Connection, vault: &Path, id: &str) -> rusqlite::Result<Option<Checked>> {
    let Some(file) = OnDisk::read(connection, vault, id)? else {
        return Ok(None);
    };
    let hash = file.hash.clone();
    Ok(file.checked(Some(&hash)).ok())
}

/// A `staged` row.
struct Row {
    cid: String,
    note: String,
    target: String,
    rel: String,
    hash: String,
    content: String,
}

fn rows(connection: &Connection) -> rusqlite::Result<Vec<Row>> {
    let mut statement = connection
        .prepare("SELECT cid, note, target, rel, hash, content FROM staged ORDER BY note, cid")?;
    let rows = statement.query_map([], |row| {
        Ok(Row {
            cid: row.get(0)?,
            note: row.get(1)?,
            target: row.get(2)?,
            rel: row.get(3)?,
            hash: row.get(4)?,
            content: row.get(5)?,
        })
    })?;
    rows.collect()
}
