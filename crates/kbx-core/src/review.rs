//! Review decisions (§7): the `reviews` table, strikes, and acceptance, which
//! is what writes relations to notes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tracing::warn;

use crate::graph::Graph;
use crate::index::{load_resolver, sha256};
use crate::note::{Note, parse_note};
use crate::ontology::{NONE, Ontology, admits};
use crate::writer::{Staged, WriteError, add_relation, holds_link, write_atomic};

mod queue;

pub use queue::{Item, Queue, excerpt};

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
    let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
    let candidate = Candidate::load(&tx, cid)?;
    let rel = match decision.rel {
        Some(rel) => rel.to_string(),
        None => judged_rel(&tx, cid)?.ok_or_else(|| ReviewError::NoRelation(cid.to_string()))?,
    };
    let key = ontology
        .lookup(&rel)
        .ok_or_else(|| ReviewError::UnknownRelation(rel.clone()))?;

    let first = OnDisk::read(&tx, vault, &candidate.src)?
        .ok_or_else(|| ReviewError::SourceUnavailable(candidate.src.clone()))?;
    let second = OnDisk::read(&tx, vault, &candidate.dst)?
        .ok_or_else(|| ReviewError::TargetUnavailable(candidate.dst.clone()))?;
    let first = first.checked(candidate.src_hash.as_deref())?;
    let second = second.checked(candidate.dst_hash.as_deref())?;

    // §3: the schema decides direction, and a tie keeps the candidate's.
    let fits = |from: &Checked, to: &Checked| {
        admits(&key.relation.domain, &from.note.class)
            && admits(&key.relation.range, &to.note.class)
    };
    let (from, to) = if key.inverted {
        (&second, &first)
    } else {
        (&first, &second)
    };
    let (from, to) = match (fits(from, to), fits(to, from)) {
        (true, _) => (from, to),
        (false, true) => (to, from),
        (false, false) => {
            return Err(ReviewError::OutOfSchema {
                rel: key.name.to_string(),
                src: from.note.class.clone(),
                dst: to.note.class.clone(),
            });
        }
    };

    let (resolver, ids) = load_resolver(&tx)?;
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
    }
    if writes_inverse && inverse_exclusive {
        to.refuse_second(inverse_key, |t| names(&from.note.id, t))?;
    }
    let forward = from.with_relation(key.name, &to.note.id, |t| names(&to.note.id, t))?;
    let inverse = match writes_inverse {
        true => to.with_relation(inverse_key, &from.note.id, |t| names(&from.note.id, t))?,
        false => None,
    };

    record(
        &tx,
        (&from.note.id, &to.note.id, key.name),
        "accept",
        decision.reason,
        0,
    )?;
    tx.execute("DELETE FROM candidates WHERE cid = ?", [cid])?;
    let mut changes = Vec::new();
    for (file, text) in [(from, forward), (to, inverse)] {
        if let Some(text) = text {
            follow_hash(&tx, &file.note.id, &file.hash, &sha256(text.as_bytes()))?;
            changes.push((file, text));
        }
    }
    replace_all(vault, &changes)?;
    tx.commit()?;
    Ok(Accepted {
        src: from.note.id.clone(),
        dst: to.note.id.clone(),
        rel: key.name.to_string(),
        written: changes.iter().map(|(f, _)| f.path.clone()).collect(),
    })
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
    src: String,
    dst: String,
    src_hash: Option<String>,
    dst_hash: Option<String>,
}

impl Candidate {
    fn load(connection: &Connection, cid: &str) -> Result<Candidate, ReviewError> {
        connection
            .query_row(
                "SELECT src, dst, src_hash, dst_hash FROM candidates WHERE cid = ?",
                [cid],
                |row| {
                    Ok(Candidate {
                        src: row.get(0)?,
                        dst: row.get(1)?,
                        src_hash: row.get(2)?,
                        dst_hash: row.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| ReviewError::UnknownCandidate(cid.to_string()))
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

/// The `(src, dst, rel)` of a rejection or deferral, which needn't name a
/// relation: with `--judge none` there may be nothing to say but "not this".
fn decided_rel(
    connection: &Connection,
    ontology: &Ontology,
    candidate: &Candidate,
    decision: Decision<'_>,
) -> Result<(String, String, String), ReviewError> {
    let cid = cid(&candidate.src, &candidate.dst);
    let rel = match decision.rel {
        Some(rel) => Some(rel.to_string()),
        None => judged_rel(connection, &cid)?,
    };
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
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
}

impl OnDisk {
    /// `None` if the note left the index or its file can't be read, which to
    /// this user is the same thing (§1).
    fn read(connection: &Connection, vault: &Path, id: &str) -> rusqlite::Result<Option<OnDisk>> {
        let path: Option<String> = connection
            .query_row("SELECT path FROM files WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(path) = path.map(PathBuf::from) else {
            return Ok(None);
        };
        let Ok(bytes) = fs::read(vault.join(&path)) else {
            return Ok(None);
        };
        let hash = sha256(&bytes);
        Ok(Some(OnDisk { path, bytes, hash }))
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
        let note = parse_note(&self.path, &text)
            .map_err(|error| ReviewError::Write(self.path.clone(), error.into()))?;
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
    fn with_relation(
        &self,
        key: &str,
        target: &str,
        is_target: impl Fn(&str) -> bool,
    ) -> Result<Option<String>, ReviewError> {
        add_relation(&self.text, key, target, is_target)
            .map_err(|error| ReviewError::Write(self.path.clone(), error))
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
