//! The graph and the review decisions as other tools read them: rows for JSON
//! Lines, and Turtle. Field names and order here are a format people script
//! against, so they only ever gain fields. See `docs/export.md`.

use std::collections::BTreeSet;

use rusqlite::{Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::graph::Graph;
use crate::index::MENTIONS;
use crate::ontology::Ontology;

/// Where classes, and relations with no `iri` of their own, get their IRI.
pub const DEFAULT_NAMESPACE: &str = "https://github.com/hassaans/kbx/ns#";

/// Where notes get their IRI. An id means something only in its vault, so this
/// is a name, not a place.
pub const NOTE_NAMESPACE: &str = "urn:kbx:note:";

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

const DECISIONS: [&str; 3] = ["accept", "reject", "defer"];

/// A row of the `edges` table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeRow {
    /// The note the edge leaves.
    pub src: String,
    /// The note it reaches.
    pub dst: String,
    /// A frontmatter key, or [`MENTIONS`].
    pub rel: String,
    /// On a row reading a relation backwards, that relation.
    pub inverse_of: Option<String>,
    /// The relation's weight. Mentions and keys outside the ontology have none.
    pub weight: Option<f64>,
    /// `frontmatter`, `inline` or `derived`, or `kbx` for an accepted relation
    /// no note states.
    pub origin: String,
    /// The section holding the link.
    pub src_section: Option<String>,
    /// The line holding the link.
    pub line: Option<i64>,
}

/// A row of the `reviews` table (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// The note the relation was read from.
    pub src: String,
    /// The note it pointed at.
    pub dst: String,
    /// The relation, or [`NONE`](crate::NONE) if nothing named one.
    pub rel: String,
    /// `accept`, `reject` or `defer`.
    pub decision: String,
    /// Why.
    pub reason: Option<String>,
    /// Rejections that still count. `kbx unreject` clears them. Never NULL in
    /// the table: a NULL would swallow every later strike.
    #[serde(default)]
    pub strikes: i64,
    /// When, in seconds since the epoch.
    pub decided_at: Option<i64>,
}

/// Why JSON Lines couldn't be read as reviews. Nothing was imported.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The line isn't a review.
    #[error("line {0}: {1}")]
    Line(usize, serde_json::Error),
    /// The line's `decision` isn't one the table allows.
    #[error("line {0}: `{1}` is not a decision; expected accept, reject or defer")]
    Decision(usize, String),
}

/// Reads what `kbx export reviews --jsonl` printed.
pub fn parse_reviews(text: &str) -> Result<Vec<Review>, ImportError> {
    let mut reviews = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let review: Review =
            serde_json::from_str(line).map_err(|error| ImportError::Line(index + 1, error))?;
        if !DECISIONS.contains(&review.decision.as_str()) {
            return Err(ImportError::Decision(index + 1, review.decision));
        }
        reviews.push(review);
    }
    Ok(reviews)
}

impl Graph {
    /// Every edge, in key order.
    pub fn edges(&self) -> rusqlite::Result<Vec<EdgeRow>> {
        let mut statement = self.conn.prepare(
            "SELECT src, dst, rel, inverse_of, weight, origin, src_section, line
             FROM edges ORDER BY src, dst, rel, origin",
        )?;
        let rows = statement.query_map([], edge_row)?;
        rows.collect()
    }

    /// Every review, in key order.
    pub fn reviews(&self) -> rusqlite::Result<Vec<Review>> {
        let mut statement = self.conn.prepare(
            "SELECT src, dst, rel, decision, reason, strikes, decided_at
             FROM reviews ORDER BY src, dst, rel",
        )?;
        let rows = statement.query_map([], review_row)?;
        rows.collect()
    }

    /// Adds `reviews`, in one transaction. A row already here is replaced
    /// unless its decision is newer, so an old backup can't undo later work and
    /// importing twice changes nothing. Rows not mentioned stay, and notes are
    /// not touched: an imported `accept` writes no link.
    ///
    /// `kbx unreject` leaves `decided_at` alone, so one time on both sides is
    /// the same decision, perhaps with its strikes cleared since. The lower
    /// count stands.
    pub fn import_reviews(&self, reviews: &[Review]) -> rusqlite::Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut insert = tx.prepare(
            "INSERT INTO reviews (src, dst, rel, decision, reason, strikes, decided_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(src, dst, rel) DO UPDATE SET decision = excluded.decision,
                 reason = excluded.reason,
                 strikes = CASE WHEN ifnull(excluded.decided_at, 0) = ifnull(reviews.decided_at, 0)
                     THEN min(ifnull(reviews.strikes, 0), excluded.strikes)
                     ELSE excluded.strikes END,
                 decided_at = excluded.decided_at
             WHERE ifnull(excluded.decided_at, 0) >= ifnull(reviews.decided_at, 0)",
        )?;
        for r in reviews {
            insert.execute(params![
                r.src,
                r.dst,
                r.rel,
                r.decision,
                r.reason,
                r.strikes,
                r.decided_at
            ])?;
        }
        drop(insert);
        tx.commit()
    }
}

fn edge_row(row: &Row<'_>) -> rusqlite::Result<EdgeRow> {
    Ok(EdgeRow {
        src: row.get(0)?,
        dst: row.get(1)?,
        rel: row.get(2)?,
        inverse_of: row.get(3)?,
        weight: row.get(4)?,
        origin: row.get(5)?,
        src_section: row.get(6)?,
        line: row.get(7)?,
    })
}

fn review_row(row: &Row<'_>) -> rusqlite::Result<Review> {
    Ok(Review {
        src: row.get(0)?,
        dst: row.get(1)?,
        rel: row.get(2)?,
        decision: row.get(3)?,
        reason: row.get(4)?,
        strikes: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
        decided_at: row.get(6)?,
    })
}

/// The graph as Turtle: each note's class and title, then one triple per
/// relation and mention between notes, each IRI in full so that no id or class
/// name has to fit a prefixed name.
///
/// A relation is stated the way the ontology names it. Its inverse rows are
/// mirrors of those, and frontmatter keys outside the ontology have no meaning
/// to export.
pub fn turtle(g: &Graph, ontology: &Ontology) -> rusqlite::Result<String> {
    let mut text = String::new();
    let mut statement = g
        .conn
        .prepare("SELECT id, type, title FROM files ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let (id, class, title): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let note = note_iri(&id);
        let class = format!("{DEFAULT_NAMESPACE}{}", encoded(&class));
        text.push_str(&format!("<{note}> a <{class}> .\n"));
        text.push_str(&format!("<{note}> <{RDFS_LABEL}> {} .\n", literal(&title)));
    }

    let predicate = |rel: &str| match ontology.relations.get(rel) {
        Some(relation) => Some(match &relation.iri {
            Some(iri) => iri.clone(),
            None => format!("{DEFAULT_NAMESPACE}{}", encoded(rel)),
        }),
        None if rel == MENTIONS => Some(format!("{DEFAULT_NAMESPACE}{MENTIONS}")),
        None => None,
    };
    // One triple for an edge both notes state, which has a row per origin.
    let triples: BTreeSet<_> = g
        .edges()?
        .into_iter()
        .filter_map(|edge| Some((edge.src, predicate(&edge.rel)?, edge.dst)))
        .collect();
    for (src, predicate, dst) in triples {
        let (src, dst) = (note_iri(&src), note_iri(&dst));
        text.push_str(&format!("<{src}> <{predicate}> <{dst}> .\n"));
    }
    Ok(text)
}

fn note_iri(id: &str) -> String {
    format!("{NOTE_NAMESPACE}{}", encoded(id))
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
fn encoded(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A quoted Turtle string.
fn literal(text: &str) -> String {
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() => quoted.push_str(&format!("\\u{:04X}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}
