//! Relations accepted and kept in `.kbx/`, which is where every acceptance goes
//! unless `[write] frontmatter` is on. They are edges of origin `kbx`, and
//! [`apply`] writes them to the notes later. See `docs/review.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::{Accepted, Accepting, Candidate, OnDisk, Preview, ReviewError, write_accepted};
use crate::graph::{Graph, has_accepted_table};
use crate::index::{link_accepted, mirror, unlink_accepted};
use crate::ontology::Ontology;

/// A relation accepted and in no note, as the frontmatter would state it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedRelation {
    /// The candidate it came from, which `kbx reject` takes to withdraw it.
    pub cid: String,
    /// The note that would hold the relation.
    pub src: String,
    /// The note it points at.
    pub dst: String,
    /// The relation.
    pub rel: String,
    /// The section key at the `src` end of a section-level pair.
    pub src_section: Option<String>,
    /// The section key at the `dst` end of a section-level pair.
    pub dst_section: Option<String>,
}

/// What [`apply`] did, or with a preview, would do.
#[derive(Debug, Default)]
pub struct Applied {
    /// The relations now in notes.
    pub applied: Vec<Accepted>,
    /// Those that stay in `.kbx/`, and why.
    pub refused: Vec<(AcceptedRelation, ReviewError)>,
}

/// The relations accepted and in no note. A database opened read-only can't be
/// upgraded, so one from an older kbx has none.
pub fn accepted_relations(connection: &Connection) -> rusqlite::Result<Vec<AcceptedRelation>> {
    if !has_accepted_table(connection)? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT cid, src, dst, rel, src_section, dst_section FROM accepted
         ORDER BY src, dst, rel",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AcceptedRelation {
            cid: row.get(0)?,
            src: row.get(1)?,
            dst: row.get(2)?,
            rel: row.get(3)?,
            src_section: row.get(4)?,
            dst_section: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// Keys each note counts as holding through [`accepted_relations`], as
/// `(id, key)`: the relation, and the inverse that writing it would add.
pub(crate) fn held_keys(
    relations: &[AcceptedRelation],
    ontology: &Ontology,
) -> HashSet<(String, String)> {
    let mut held = HashSet::new();
    for relation in relations {
        held.insert((relation.src.clone(), relation.rel.clone()));
        let key = ontology.lookup(&relation.rel);
        if key.is_some_and(|key| !key.relation.derived_only)
            && let Some(inverse) = mirror(ontology, &relation.rel)
        {
            held.insert((relation.dst.clone(), inverse.to_string()));
        }
    }
    held
}

/// Whether a candidate's acceptance is the one kept here.
pub(super) fn holds(connection: &Connection, cid: &str) -> rusqlite::Result<bool> {
    let found = connection
        .query_row("SELECT 1 FROM accepted WHERE cid = ?", [cid], |_| Ok(()))
        .optional()?;
    Ok(found.is_some())
}

/// Keeps an acceptance here and adds its edges.
pub(super) fn hold(
    tx: &Transaction<'_>,
    ontology: &Ontology,
    relation: &AcceptedRelation,
    decided_at: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO accepted (cid, src, dst, rel, src_section, dst_section, decided_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(src, dst, rel) DO UPDATE SET cid = excluded.cid,
             src_section = excluded.src_section, dst_section = excluded.dst_section",
        params![
            relation.cid,
            relation.src,
            relation.dst,
            relation.rel,
            relation.src_section,
            relation.dst_section,
            decided_at
        ],
    )?;
    link_accepted(tx, ontology, &relation.src, &relation.dst, &relation.rel)
}

/// Lets go of a candidate's acceptance and its edges: it was withdrawn, or a
/// note holds it now.
pub(super) fn release(
    tx: &Transaction<'_>,
    ontology: &Ontology,
    cid: &str,
) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("SELECT src, dst, rel FROM accepted WHERE cid = ?")?;
    let rows = statement.query_map([cid], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let rows = rows.collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    drop(statement);
    for (src, dst, rel) in rows {
        unlink_accepted(tx, ontology, &src, &dst, &rel)?;
    }
    tx.execute("DELETE FROM accepted WHERE cid = ?", [cid])?;
    Ok(())
}

/// Whether `note` already has a target other than `other` under `key`, kept
/// here. An exclusive relation counts these as it counts the frontmatter's.
pub(super) fn has_other_target(
    tx: &Transaction<'_>,
    ontology: &Ontology,
    note: &str,
    key: &str,
    other: &str,
) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM accepted WHERE (src = ?1 AND rel = ?2 AND dst <> ?3)
             OR (dst = ?1 AND rel = ?4 AND src <> ?3))",
        params![note, key, other, mirror(ontology, key)],
        |row| row.get(0),
    )
}

/// Writes the relations kept here into the notes, as accepting them with
/// `[write] frontmatter` on would have: the inverse, the mirror, links by path to
/// notes with inferred keys. With `preview` it adds that to the preview and
/// nothing changes, here or on disk.
///
/// The decision is taken, so no hash from `kbx suggest` guards it. A note must
/// only match the index. One relation refused doesn't stop the rest.
pub fn apply(
    g: &Graph,
    vault: &Path,
    ontology: &Ontology,
    mut preview: Option<&mut Preview>,
) -> rusqlite::Result<Applied> {
    let mut outcome = Applied::default();
    // The index doesn't follow until the run ends, so what this run wrote would
    // look like someone else's edit to the next relation on that note.
    let mut ours: HashMap<String, String> = HashMap::new();
    for relation in accepted_relations(g.connection())? {
        let tx = Transaction::new_unchecked(&g.conn, TransactionBehavior::Immediate)?;
        let candidate = Candidate {
            cid: relation.cid.clone(),
            src_hash: trusted_hash(&tx, &ours, &relation.src)?,
            dst_hash: trusted_hash(&tx, &ours, &relation.dst)?,
            src: relation.src.clone(),
            dst: relation.dst.clone(),
            src_section: relation.src_section.clone(),
            dst_section: relation.dst_section.clone(),
            accepted_rel: Some(relation.rel.clone()),
        };
        let accepting = Accepting {
            rel: &relation.rel,
            reason: None,
            as_written: true,
            preview: preview.as_deref_mut(),
        };
        match write_accepted(&tx, vault, ontology, &relation.cid, &candidate, accepting) {
            Ok(accepted) => {
                // A preview's rows go with the transaction.
                if preview.is_none() {
                    tx.commit()?;
                    for id in [&relation.src, &relation.dst] {
                        if let Some(file) = OnDisk::read(g.connection(), vault, id)? {
                            ours.insert(id.clone(), file.hash);
                        }
                    }
                }
                outcome.applied.push(accepted);
            }
            Err(error) => outcome.refused.push((relation, error)),
        }
    }
    Ok(outcome)
}

/// The hash a note has to have: what this run left it with, or else the index's.
fn trusted_hash(
    connection: &Connection,
    ours: &HashMap<String, String>,
    id: &str,
) -> rusqlite::Result<Option<String>> {
    if let Some(hash) = ours.get(id) {
        return Ok(Some(hash.clone()));
    }
    index_hash(connection, id)
}

/// A note's hash as of the last `kbx index`.
pub(super) fn index_hash(connection: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    connection
        .query_row("SELECT content_hash FROM files WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .optional()
}

/// The paths of what [`apply`] wrote are in [`Accepted::written`].
pub fn written_paths(applied: &Applied) -> Vec<PathBuf> {
    let written = applied.applied.iter().flat_map(|a| a.written.iter());
    written.cloned().collect()
}
