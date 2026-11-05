//! Review decisions (§7): the `reviews` table, strikes, and acceptance, which
//! keeps a relation in `.kbx/` or, with `[write] frontmatter` on, writes it to
//! notes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use similar::TextDiff;
use tracing::warn;

use crate::graph::Graph;
use crate::index::{load_resolver, sha256, slash_path};
use crate::note::{Note, parse_note};
use crate::ontology::{NONE, Ontology, RelationKey, admits};
use crate::writer::{Staged, WriteError, add_relation, holds_link, unstage_entries, write_atomic};

mod accepted;
mod queue;
mod stage;

pub(crate) use accepted::held_keys;
pub use accepted::{AcceptedRelation, Applied, accepted_relations, apply, written_paths};
pub use queue::{Item, JUDGE_ERROR, Queue, excerpt, excerpt_of, section_excerpt};
pub use stage::{
    StageReport, Synced, hold_reviewed, locked_notes, mark_reviewed, stage, sync_stage, unstage,
};

/// What a wikilink can't hold in a file name.
const UNLINKABLE: [char; 5] = ['#', '|', '^', '[', ']'];

/// Rejections of a pair, across all relations, that suppress it from every
/// generator.
pub const STRIKES: i64 = 3;

/// A decision on one candidate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decision<'a> {
    /// The relation decided on. A relation's inverse is taken as that relation
    /// read the other way. Without one, the judge's latest verdict names it.
    pub rel: Option<&'a str>,
    /// Why, kept in `reviews`.
    pub reason: Option<&'a str>,
}

/// What [`accept`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// The note holding the relation, which the schema may have swapped with
    /// the candidate's `dst`.
    pub src: String,
    /// The note it points at.
    pub dst: String,
    /// The relation.
    pub rel: String,
    /// Files changed, relative to the vault. Empty if both already said so.
    pub written: Vec<PathBuf>,
}

/// What [`accept_unwritten`] would write, note by note. Each note starts from
/// what's on disk, and every candidate previewed goes on top of the ones before,
/// as accepting them in that order would.
#[derive(Debug, Default)]
pub struct Preview {
    /// By path from the vault: the file as it is, and as it would be.
    notes: BTreeMap<PathBuf, (String, String)>,
}

impl Preview {
    /// Whether no note would change.
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// A unified diff per note, by path. Lines keep their own endings, so a
    /// diff applied to the file gives exactly what accepting would write.
    pub fn diffs(&self) -> Vec<String> {
        let diff = |(path, (before, after)): (&PathBuf, &(String, String))| {
            let path = path.to_string_lossy();
            let lines = TextDiff::from_lines(before.as_str(), after.as_str());
            let (old, new) = (format!("a/{path}"), format!("b/{path}"));
            lines.unified_diff().header(&old, &new).to_string()
        };
        self.notes.iter().map(diff).collect()
    }

    /// `file` as the candidates before this one left it.
    fn over(&self, mut file: Checked) -> Result<Checked, ReviewError> {
        if let Some((_, after)) = self.notes.get(&file.path) {
            let mut note = parse_note(&file.path, after)
                .map_err(|error| ReviewError::Write(file.path.clone(), error.into()))?;
            if note.inferred.class {
                note.class = file.note.class;
            }
            file.note = note;
            file.text = after.clone();
        }
        Ok(file)
    }

    fn add(&mut self, file: &Checked, after: String) {
        let entry = (file.text.clone(), String::new());
        self.notes.entry(file.path.clone()).or_insert(entry).1 = after;
    }
}

/// What [`reject`] recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// The candidate's `src`, or its `dst` if `rel` reads the other way.
    pub src: String,
    /// The other note.
    pub dst: String,
    /// The relation rejected, or [`NONE`] if there was none to name.
    pub rel: String,
    /// Rejections of the pair so far, across all relations.
    pub strikes: i64,
}

impl Rejected {
    /// No generator will propose the pair again until [`unreject`].
    pub fn suppressed(&self) -> bool {
        self.strikes >= STRIKES
    }
}

/// What [`defer`] recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    /// The candidate's `src`, or its `dst` if `rel` reads the other way.
    pub src: String,
    /// The other note.
    pub dst: String,
    /// The relation deferred, or [`NONE`] if there was none to name.
    pub rel: String,
}

/// What [`unreject`] lifted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrejected {
    /// The pair, lower id first.
    pub pair: (String, String),
    /// The strikes cleared.
    pub strikes: i64,
}

/// Why a decision was refused. Nothing is written in any of these cases.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// The [`Queue`] has no current item to decide on.
    #[error("the queue is empty")]
    EmptyQueue,
    /// The cid isn't in `candidates`.
    #[error("no candidate {0}; run `kbx suggest`")]
    UnknownCandidate(String),
    /// [`unreject`] found no strikes under the cid.
    #[error("no rejected pair has cid {0}")]
    NotRejected(String),
    /// Nothing says which relation to write.
    #[error("candidate {0} has no judged relation; name one with --rel")]
    NoRelation(String),
    /// The relation isn't in the ontology.
    #[error("rel.unknown: `{0}` is not a relation in the ontology")]
    UnknownRelation(String),
    /// A person wrote the relation on a note the schema says it can't start
    /// from, so accepting it would write something else.
    #[error("rel.range: {path} holds `{rel}`, which the schema reads the other way round")]
    Reversed {
        /// The note, relative to the vault.
        path: PathBuf,
        /// The key as written.
        rel: String,
    },
    /// The notes' classes fit the relation in neither direction.
    #[error("rel.range: {rel} can't join a {src} and a {dst} in either direction")]
    OutOfSchema {
        /// The relation.
        rel: String,
        /// The class of the candidate's `src`.
        src: String,
        /// The class of the candidate's `dst`.
        dst: String,
    },
    /// An `exclusive` relation already has another target.
    #[error("rel.cardinality: {path} already has a `{rel}` target, and it is exclusive")]
    Exclusive {
        /// The note, relative to the vault.
        path: PathBuf,
        /// The relation.
        rel: String,
    },
    /// The source note can't be read any more.
    #[error("source.unavailable: note {0} is not on disk; run `kbx index` and `kbx suggest`")]
    SourceUnavailable(String),
    /// The target note can't be read any more (§7).
    #[error("target.unavailable: note {0} is not on disk; run `kbx index` and `kbx suggest`")]
    TargetUnavailable(String),
    /// A note differs from what the candidate was generated from.
    #[error(
        "{0} changed since this candidate was generated, so nothing was written; \
         run `kbx index` and `kbx suggest` again"
    )]
    Changed(PathBuf),
    /// Only an acceptance writes to a note with inferred keys.
    #[error("{0} has inferred keys, so nothing is staged in it; use `kbx review`")]
    Inferred(PathBuf),
    /// A note's frontmatter couldn't be edited.
    #[error("{0}: {1}")]
    Write(PathBuf, WriteError),
    /// A note could not be replaced.
    #[error("{0}: {1}")]
    Io(PathBuf, io::Error),
    /// The database failed.
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
}

/// The candidate id: the first 32 hex digits of the SHA-256 of the two note
/// ids, lower one first, joined by a NUL.
///
/// It depends on nothing but the pair, so a re-run lands on the same row and
/// the verdict cache stays valid. The order is fixed because a pair is one
/// question for the judge whichever way the generators found it; direction
/// comes from the schema (§3).
pub fn cid(src: &str, dst: &str) -> String {
    let (a, b) = ordered(src, dst);
    sha256(format!("{a}\0{b}").as_bytes())[..32].to_string()
}

/// The id of a candidate about two sections: [`cid`] when neither end has one,
/// and otherwise a digest that covers the section keys as well, so that a verdict
/// on these sections is never taken for one on others.
pub fn section_cid(
    src: &str,
    dst: &str,
    src_section: Option<&str>,
    dst_section: Option<&str>,
) -> String {
    if src_section.is_none() && dst_section.is_none() {
        return cid(src, dst);
    }
    let (a, b) = ordered(src, dst);
    let (first, second) = match a == src {
        true => (src_section, dst_section),
        false => (dst_section, src_section),
    };
    let (first, second) = (first.unwrap_or(""), second.unwrap_or(""));
    sha256(format!("{a}\0{b}\0{first}\0{second}").as_bytes())[..32].to_string()
}

/// A section's anchor as the index knows it: `^block-id`, or the slug path if it
/// has no block id, for the address shown in review. `None` if it has neither.
/// Whether a link can reach it is for [`Note::section_fragment`] to say: the
/// index keeps slugs, and different headings can share one.
pub fn section_anchor(connection: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let block: Option<String> = connection
        .query_row(
            "SELECT block_id FROM blocks WHERE section_key = ?
             ORDER BY start_line, block_id LIMIT 1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(block) = block {
        return Ok(Some(format!("^{block}")));
    }
    connection
        .query_row(
            "SELECT heading_path FROM sections WHERE key = ? AND heading_path <> ''",
            [key],
            |row| row.get(0),
        )
        .optional()
}

/// The pair with the lower id first.
pub fn ordered(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// The pairs with [`STRIKES`] or more, lower id first. Strikes count against
/// the pair whichever way each rejection read, and whatever its relation.
pub fn suppressed_pairs(connection: &Connection) -> rusqlite::Result<HashSet<(String, String)>> {
    let totals = strike_totals(connection)?;
    let suppressed = totals.into_iter().filter(|(_, total)| *total >= STRIKES);
    Ok(suppressed.map(|(pair, _)| pair).collect())
}

fn strike_totals(connection: &Connection) -> rusqlite::Result<HashMap<(String, String), i64>> {
    let mut statement =
        connection.prepare_cached("SELECT src, dst, strikes FROM reviews WHERE strikes > 0")?;
    let mut totals = HashMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (src, dst): (String, String) = (row.get(0)?, row.get(1)?);
        *totals.entry(ordered(&src, &dst)).or_default() += row.get::<_, i64>(2)?;
    }
    Ok(totals)
}

/// Writes the relation to the source note and its inverse to the target, unless
/// the ontology marks it `derived_only`, then records the decision and takes
/// the candidate out of the queue.
///
/// Both notes are checked and both new files prepared before either changes.
/// Candidates sharing a note stay acceptable: their recorded hash follows this
/// write, which is the only change to the note. The index doesn't, until
/// `kbx index` runs.
pub fn accept(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    cid: &str,
    decision: Decision<'_>,
) -> Result<Accepted, ReviewError> {
    accept_with(g, vault, ontology, cid, decision, None)
}

/// Records an acceptance with [`accept`]'s checks and adds what it would write
/// to `preview`, leaving every note alone. The relation goes in `accepted`, with
/// its edges, unless a note states it already. The candidate stays, marked as
/// accepted: [`accept`] or [`apply`] writes it later, under this relation unless
/// told otherwise, and [`reject`] and [`defer`] take the decision back.
pub fn accept_unwritten(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    cid: &str,
    decision: Decision<'_>,
    preview: &mut Preview,
) -> Result<Accepted, ReviewError> {
    accept_with(g, vault, ontology, cid, decision, Some(preview))
}

fn accept_with(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    cid: &str,
    decision: Decision<'_>,
    preview: Option<&mut Preview>,
) -> Result<Accepted, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let mut candidate = Candidate::load(&tx, cid)?;
    // Its pair is linked now, so no `kbx suggest` moves its hashes along. The
    // decision was made, and a note need only match the index.
    if accepted::holds(&tx, cid)? {
        candidate.follow_index(&tx, vault)?;
    }
    let rel = standing_rel(&tx, &candidate, decision)?
        .ok_or_else(|| ReviewError::NoRelation(cid.to_string()))?;
    let accepted = write_accepted(
        &tx,
        vault,
        ontology,
        cid,
        &candidate,
        Accepting {
            rel: &rel,
            reason: decision.reason,
            as_written: false,
            preview,
        },
    )?;
    tx.commit()?;
    Ok(accepted)
}

/// What [`write_accepted`] is asked to write.
struct Accepting<'a> {
    rel: &'a str,
    reason: Option<&'a str>,
    /// The relation already reads from the candidate's `src`, because a person
    /// put it there, so the schema can refuse that but not turn it around.
    as_written: bool,
    /// Where to put the changes instead of in the notes, if anywhere.
    preview: Option<&'a mut Preview>,
}

/// The writes and rows of an acceptance, up to the commit. The rename comes
/// last, so that a failure before it leaves the notes as they were.
fn write_accepted(
    tx: &Transaction<'_>,
    vault: &Path,
    ontology: &Ontology,
    cid: &str,
    candidate: &Candidate,
    accepting: Accepting<'_>,
) -> Result<Accepted, ReviewError> {
    let rel = accepting.rel;
    let key = ontology
        .lookup(rel)
        .ok_or_else(|| ReviewError::UnknownRelation(rel.to_string()))?;

    let first = OnDisk::read(tx, vault, &candidate.src)?
        .ok_or_else(|| ReviewError::SourceUnavailable(candidate.src.clone()))?;
    let second = OnDisk::read(tx, vault, &candidate.dst)?
        .ok_or_else(|| ReviewError::TargetUnavailable(candidate.dst.clone()))?;
    let first = first.checked(candidate.src_hash.as_deref())?;
    let second = second.checked(candidate.dst_hash.as_deref())?;
    let (first, second) = match &accepting.preview {
        Some(preview) => (preview.over(first)?, preview.over(second)?),
        None => (first, second),
    };

    let fit = runs_forward(&key, &first.note.class, &second.note.class);
    if accepting.as_written && fit == Some(key.inverted) {
        return Err(ReviewError::Reversed {
            path: first.path.clone(),
            rel: rel.to_string(),
        });
    }
    let (from, to) = match fit {
        Some(true) => (&first, &second),
        Some(false) => (&second, &first),
        None => {
            let (from, to) = match key.inverted {
                true => (&second, &first),
                false => (&first, &second),
            };
            return Err(ReviewError::OutOfSchema {
                rel: key.name.to_string(),
                src: from.note.class.clone(),
                dst: to.note.class.clone(),
            });
        }
    };

    let (resolver, ids) = load_resolver(tx)?;
    let names = |id: &str, target: &str| {
        resolver
            .resolve(target)
            .is_some_and(|(index, _)| ids[index] == id)
    };
    // An inverse is exclusive only if it's the same key, as in a symmetric one.
    let inverse_key = key.relation.inverse.as_str();
    let writes_inverse = !key.relation.derived_only;
    let inverse_exclusive = ontology.lookup(inverse_key).is_some_and(|k| k.exclusive());
    if key.relation.exclusive {
        from.refuse_second(key.name, |t| names(&to.note.id, t))?;
        from.refuse_held(tx, ontology, key.name, &to.note.id)?;
    }
    if writes_inverse && inverse_exclusive {
        to.refuse_second(inverse_key, |t| names(&from.note.id, t))?;
        to.refuse_held(tx, ontology, inverse_key, &from.note.id)?;
    }
    // Each note points at the section at the other end.
    let section_of = |file: &Checked| match file.note.id == candidate.src {
        true => candidate.src_section.as_deref(),
        false => candidate.dst_section.as_deref(),
    };
    let link_to =
        |file: &Checked| file.link_target(section_of(file), |target| names(&file.note.id, target));
    let forward = from.with_relation(key.name, &link_to(to), |t| names(&to.note.id, t))?;
    let inverse = match writes_inverse {
        true => to.with_relation(inverse_key, &link_to(from), |t| names(&from.note.id, t))?,
        false => None,
    };
    // Either note stating it is enough for the index to have the edge.
    let stated = forward.is_none() || (writes_inverse && inverse.is_none());

    // Writing what was already accepted is not a new decision, so its reason
    // and date stand unless this one gives a reason of its own.
    let decided = candidate.accepted_rel.as_deref() == Some(rel);
    if !decided {
        withdraw(tx, ontology, candidate)?;
    }
    if !decided || accepting.reason.is_some() {
        record(
            tx,
            (&from.note.id, &to.note.id, key.name),
            "accept",
            accepting.reason,
            0,
        )?;
    }
    let holder = stage::holder(tx, cid)?;
    let mut changes = Vec::new();
    for (file, text) in [(from, forward), (to, inverse)] {
        let text = match holder.as_deref() == Some(file.note.id.as_str()) {
            true => file.without_entry(text, cid)?,
            false => text,
        };
        if let Some(text) = text {
            changes.push((file, text));
        }
    }
    let mut written = Vec::new();
    if let Some(preview) = accepting.preview {
        tx.execute(
            "UPDATE candidates SET accepted_rel = ?1 WHERE cid = ?2",
            params![rel, cid],
        )?;
        if !stated {
            let relation = AcceptedRelation {
                cid: cid.to_string(),
                src: from.note.id.clone(),
                dst: to.note.id.clone(),
                rel: key.name.to_string(),
                src_section: section_of(from).map(String::from),
                dst_section: section_of(to).map(String::from),
            };
            accepted::hold(tx, ontology, &relation, now())?;
        }
        for (file, text) in changes {
            preview.add(file, text);
        }
    } else {
        tx.execute("DELETE FROM candidates WHERE cid = ?", [cid])?;
        accepted::release(tx, ontology, cid)?;
        for (file, text) in &changes {
            stage::follow(tx, &file.note.id, &file.hash, text)?;
        }
        tx.execute("DELETE FROM staged WHERE cid = ?", [cid])?;
        replace_all(vault, &changes)?;
        written.extend(changes.iter().map(|(f, _)| f.path.clone()));
    }
    Ok(Accepted {
        src: from.note.id.clone(),
        dst: to.note.id.clone(),
        rel: key.name.to_string(),
        written,
    })
}

/// Whether the relation runs from the candidate's `src` to its `dst`, given
/// their classes. `None` if it fits neither way.
///
/// §3: the schema decides direction, and a tie keeps the candidate's.
fn runs_forward(key: &RelationKey<'_>, src: &str, dst: &str) -> Option<bool> {
    let fits = |from, to| admits(&key.relation.domain, from) && admits(&key.relation.range, to);
    let (from, to) = match key.inverted {
        true => (dst, src),
        false => (src, dst),
    };
    match (fits(from, to), fits(to, from)) {
        (true, _) => Some(!key.inverted),
        (false, true) => Some(key.inverted),
        (false, false) => None,
    }
}

/// Records a rejection, adds a strike and takes the candidate out of the
/// queue. Below [`STRIKES`] a generator can propose the pair again, to be
/// judged afresh. The row stays in `reviews` for good.
pub fn reject(
    g: &Graph,
    ontology: &Ontology,
    cid: &str,
    decision: Decision<'_>,
) -> Result<Rejected, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let candidate = Candidate::load(&tx, cid)?;
    let (src, dst, rel) = decided_rel(&tx, ontology, &candidate, decision)?;
    withdraw(&tx, ontology, &candidate)?;
    record(&tx, (&src, &dst, &rel), "reject", decision.reason, 1)?;
    tx.execute("DELETE FROM candidates WHERE cid = ?", [cid])?;
    let strikes = strike_totals(&tx)?
        .get(&ordered(&src, &dst))
        .copied()
        .unwrap_or(0);
    tx.commit()?;
    Ok(Rejected {
        src,
        dst,
        rel,
        strikes,
    })
}

/// Records that the decision was put off. The candidate stays in the queue
/// and the pair's strikes stand.
pub fn defer(
    g: &Graph,
    ontology: &Ontology,
    cid: &str,
    decision: Decision<'_>,
) -> Result<Deferred, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let candidate = Candidate::load(&tx, cid)?;
    let (src, dst, rel) = decided_rel(&tx, ontology, &candidate, decision)?;
    withdraw(&tx, ontology, &candidate)?;
    record(&tx, (&src, &dst, &rel), "defer", decision.reason, 0)?;
    tx.commit()?;
    Ok(Deferred { src, dst, rel })
}

/// Clears a pair's strikes, so generators can propose it again. The rejections
/// stay in `reviews`, reasons and all: they are the evaluation set (§4).
pub fn unreject(g: &Graph, cid: &str) -> Result<Unrejected, ReviewError> {
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let totals = strike_totals(&tx)?;
    let found = totals
        .into_iter()
        .find(|((a, b), _)| self::cid(a, b) == cid);
    let Some((pair, strikes)) = found else {
        return Err(ReviewError::NotRejected(cid.to_string()));
    };
    tx.execute(
        "UPDATE reviews SET strikes = 0
         WHERE (src = ?1 AND dst = ?2) OR (src = ?2 AND dst = ?1)",
        params![pair.0, pair.1],
    )?;
    tx.commit()?;
    Ok(Unrejected { pair, strikes })
}

/// A `candidates` row, as far as a decision needs it.
struct Candidate {
    cid: String,
    src: String,
    dst: String,
    src_hash: Option<String>,
    dst_hash: Option<String>,
    src_section: Option<String>,
    dst_section: Option<String>,
    /// The relation [`accept_unwritten`] recorded, not yet in any note. It's
    /// as it was named, not as the ontology's key: where the schema fits both
    /// ways, an inverse is what says which note holds the relation.
    accepted_rel: Option<String>,
}

impl Candidate {
    fn load(connection: &Connection, cid: &str) -> Result<Candidate, ReviewError> {
        connection
            .query_row(
                "SELECT src, dst, src_hash, dst_hash, src_section, dst_section, accepted_rel
                 FROM candidates WHERE cid = ?",
                [cid],
                |row| {
                    Ok(Candidate {
                        cid: cid.to_string(),
                        src: row.get(0)?,
                        dst: row.get(1)?,
                        src_hash: row.get(2)?,
                        dst_hash: row.get(3)?,
                        src_section: row.get(4)?,
                        dst_section: row.get(5)?,
                        accepted_rel: row.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| ReviewError::UnknownCandidate(cid.to_string()))
    }

    /// Takes a note that matches the index as what was decided on. One that
    /// doesn't keeps its hash, which an acceptance sharing the note may have
    /// moved along ahead of the index.
    fn follow_index(&mut self, connection: &Connection, vault: &Path) -> rusqlite::Result<()> {
        for (id, hash) in [
            (&self.src, &mut self.src_hash),
            (&self.dst, &mut self.dst_hash),
        ] {
            let indexed = accepted::index_hash(connection, id)?;
            let file = OnDisk::read(connection, vault, id)?;
            if indexed.is_some() && file.map(|file| file.hash) == indexed {
                *hash = indexed;
            }
        }
        Ok(())
    }
}

/// The relation of the latest verdict that names one.
fn judged_rel(connection: &Connection, cid: &str) -> rusqlite::Result<Option<String>> {
    connection
        .query_row(
            "SELECT rel FROM verdicts WHERE cid = ?1 AND rel IS NOT NULL AND rel <> ?2
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![cid, NONE],
            |row| row.get(0),
        )
        .optional()
}

/// The relation a decision is about: the one it names, or else the one already
/// accepted without writing, or else the judge's.
fn standing_rel(
    connection: &Connection,
    candidate: &Candidate,
    decision: Decision<'_>,
) -> rusqlite::Result<Option<String>> {
    let named = decision.rel.map(String::from);
    match named.or_else(|| candidate.accepted_rel.clone()) {
        Some(rel) => Ok(Some(rel)),
        None => judged_rel(connection, &candidate.cid),
    }
}

/// Takes back an acceptance that was never written, with its edges, whose row
/// would otherwise stand in `reviews` for a link no note has. Only a rejection
/// adds strikes, so a row with some goes back to being one, and the rest go.
fn withdraw(
    tx: &Transaction<'_>,
    ontology: &Ontology,
    candidate: &Candidate,
) -> rusqlite::Result<()> {
    let Some(rel) = &candidate.accepted_rel else {
        return Ok(());
    };
    accepted::release(tx, ontology, &candidate.cid)?;
    let name = ontology.lookup(rel).map_or(rel.as_str(), |key| key.name);
    let row = "rel = ?1 AND decision = 'accept'
        AND ((src = ?2 AND dst = ?3) OR (src = ?3 AND dst = ?2))";
    let pair = params![name, candidate.src, candidate.dst];
    tx.execute(
        &format!("UPDATE reviews SET decision = 'reject' WHERE strikes > 0 AND {row}"),
        pair,
    )?;
    tx.execute(&format!("DELETE FROM reviews WHERE {row}"), pair)?;
    tx.execute(
        "UPDATE candidates SET accepted_rel = NULL WHERE cid = ?",
        [&candidate.cid],
    )?;
    Ok(())
}

/// The `(src, dst, rel)` of a rejection or deferral, which needn't name a
/// relation: with `--judge none` there may be nothing to say but "not this".
fn decided_rel(
    connection: &Connection,
    ontology: &Ontology,
    candidate: &Candidate,
    decision: Decision<'_>,
) -> Result<(String, String, String), ReviewError> {
    let rel = standing_rel(connection, candidate, decision)?;
    let (src, dst) = (candidate.src.clone(), candidate.dst.clone());
    let Some(rel) = rel.filter(|rel| rel != NONE) else {
        return Ok((src, dst, NONE.to_string()));
    };
    let key = ontology
        .lookup(&rel)
        .ok_or_else(|| ReviewError::UnknownRelation(rel.clone()))?;
    let name = key.name.to_string();
    Ok(match key.inverted {
        true => (dst, src, name),
        false => (src, dst, name),
    })
}

/// Sets the decision on a `(src, dst, rel)` row, adding to its strikes.
fn record(
    tx: &Transaction<'_>,
    (src, dst, rel): (&str, &str, &str),
    decision: &str,
    reason: Option<&str>,
    strikes: i64,
) -> rusqlite::Result<()> {
    let now = now();
    tx.execute(
        "INSERT INTO reviews (src, dst, rel, decision, reason, strikes, decided_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(src, dst, rel) DO UPDATE SET decision = excluded.decision,
             reason = excluded.reason, strikes = strikes + excluded.strikes,
             decided_at = excluded.decided_at",
        params![src, dst, rel, decision, reason, strikes, now],
    )?;
    Ok(())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn follow_hash(tx: &Transaction<'_>, id: &str, old: &str, new: &str) -> rusqlite::Result<()> {
    for (note, hash) in [("src", "src_hash"), ("dst", "dst_hash")] {
        let sql = format!("UPDATE candidates SET {hash} = ?1 WHERE {note} = ?2 AND {hash} = ?3");
        tx.execute(&sql, params![new, id, old])?;
    }
    Ok(())
}

/// A note's file as it is now, before anything says it can be trusted.
struct OnDisk {
    /// Relative to the vault.
    path: PathBuf,
    bytes: Vec<u8>,
    hash: String,
    /// The class the index gave the note.
    class: String,
}

impl OnDisk {
    /// `None` if the note left the index or its file can't be read, which to
    /// this user is the same thing (§1).
    fn read(connection: &Connection, vault: &Path, id: &str) -> rusqlite::Result<Option<OnDisk>> {
        let row: Option<(String, String)> = connection
            .query_row("SELECT path, type FROM files WHERE id = ?", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let Some((path, class)) = row else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        let Ok(bytes) = fs::read(vault.join(&path)) else {
            return Ok(None);
        };
        let hash = sha256(&bytes);
        Ok(Some(OnDisk {
            path,
            bytes,
            hash,
            class,
        }))
    }

    /// Refuses a file that differs from `recorded`, then parses it. A candidate
    /// from before the hashes were recorded has none, and can't be trusted.
    fn checked(self, recorded: Option<&str>) -> Result<Checked, ReviewError> {
        if recorded != Some(self.hash.as_str()) {
            return Err(ReviewError::Changed(self.path));
        }
        let text = String::from_utf8(self.bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            .map_err(|error| ReviewError::Io(self.path.clone(), error))?;
        let mut note = parse_note(&self.path, &text)
            .map_err(|error| ReviewError::Write(self.path.clone(), error.into()))?;
        // What a note doesn't state depends on `kbx.toml`, which the index read.
        if note.inferred.class {
            note.class = self.class;
        }
        Ok(Checked {
            path: self.path,
            hash: self.hash,
            text,
            note,
        })
    }
}

/// A note that is what the candidate was generated from.
struct Checked {
    path: PathBuf,
    hash: String,
    text: String,
    note: Note,
}

impl Checked {
    /// How a link to this note is written: by id, or by file name and anchor if
    /// it goes to `section` and a link can reach that. An anchor needs a link
    /// Obsidian can follow, and it looks notes up by file name. A note whose id
    /// is inferred is linked by its path, which `resolves` must take to this
    /// note: Obsidian can follow that too, and an inferred id means nothing to
    /// it.
    fn link_target(&self, section: Option<&str>, resolves: impl Fn(&str) -> bool) -> String {
        let by_path = self.path_target().filter(|target| resolves(target));
        let fragment = section.and_then(|key| self.note.section_fragment(key));
        let Some(fragment) = fragment else {
            return by_path.unwrap_or_else(|| self.note.id.clone());
        };
        let stem = self.path.file_stem().and_then(|stem| stem.to_str());
        let linkable = |stem: &&str| {
            stem.strip_prefix(self.note.id.as_str())
                .is_some_and(|rest| rest.starts_with('-'))
                && !stem.contains(UNLINKABLE)
        };
        let name = by_path
            .as_deref()
            .or(stem.filter(linkable))
            .unwrap_or(&self.note.id);
        format!("{name}#{fragment}")
    }

    /// The path without `.md`, for a note whose id is inferred, if a link can
    /// spell it.
    fn path_target(&self) -> Option<String> {
        if !self.note.inferred.id {
            return None;
        }
        let path = slash_path(&self.path);
        let target = path.strip_suffix(".md").unwrap_or(&path);
        let spellable = !target.contains(UNLINKABLE) && target.trim() == target;
        spellable.then(|| target.to_string())
    }

    fn with_relation(
        &self,
        key: &str,
        target: &str,
        is_target: impl Fn(&str) -> bool,
    ) -> Result<Option<String>, ReviewError> {
        add_relation(&self.text, key, target, is_target)
            .map_err(|error| ReviewError::Write(self.path.clone(), error))
    }

    /// `text`, or the file as it is if that's `None`, without a staged entry.
    fn without_entry(
        &self,
        text: Option<String>,
        cid: &str,
    ) -> Result<Option<String>, ReviewError> {
        let base = text.as_deref().unwrap_or(&self.text);
        let removed = unstage_entries(base, &[cid])
            .map_err(|error| ReviewError::Write(self.path.clone(), error))?;
        Ok(removed.or(text))
    }

    /// [`Checked::refuse_second`], for the targets kept in `.kbx/`.
    fn refuse_held(
        &self,
        tx: &Transaction<'_>,
        ontology: &Ontology,
        key: &str,
        other: &str,
    ) -> Result<(), ReviewError> {
        if accepted::has_other_target(tx, ontology, &self.note.id, key, other)? {
            return Err(ReviewError::Exclusive {
                path: self.path.clone(),
                rel: key.to_string(),
            });
        }
        Ok(())
    }

    /// Refuses a second target for an exclusive key.
    fn refuse_second(
        &self,
        key: &str,
        is_target: impl Fn(&str) -> bool,
    ) -> Result<(), ReviewError> {
        if self.note.relation(key).is_some() && !holds_link(&self.note, key, is_target) {
            return Err(ReviewError::Exclusive {
                path: self.path.clone(),
                rel: key.to_string(),
            });
        }
        Ok(())
    }
}

/// Replaces the files, preparing all of them first so that the likely failures
/// come before any file has changed. If a rename still fails, the files already
/// replaced get their old text back.
fn replace_all(vault: &Path, changes: &[(&Checked, String)]) -> Result<(), ReviewError> {
    let mut staged = Vec::new();
    for (file, text) in changes {
        let new = Staged::new(&vault.join(&file.path), text.as_bytes());
        staged.push(new.map_err(|error| ReviewError::Io(file.path.clone(), error))?);
    }
    let mut replaced: Vec<&Checked> = Vec::new();
    for ((file, _), new) in changes.iter().zip(staged) {
        if let Err(error) = new.commit() {
            for earlier in replaced {
                let path = vault.join(&earlier.path);
                if let Err(error) = write_atomic(&path, earlier.text.as_bytes()) {
                    warn!("{}: could not restore: {error}", path.display());
                }
            }
            return Err(ReviewError::Io(file.path.clone(), error));
        }
        replaced.push(file);
    }
    Ok(())
}
