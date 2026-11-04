//! The review queue (§7): the candidates in the order a person should see
//! them. It holds no terminal and no rules of its own: every decision here is
//! [`accept`], [`reject`] or [`defer`], so the guards and strikes apply.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Map, Value};

use super::{
    Accepted, Decision, Deferred, OnDisk, Rejected, ReviewError, accept, defer, judged_rel,
    ordered, reject,
};
use crate::graph::Graph;
use crate::note::split_frontmatter;
use crate::ontology::{NONE, Ontology};

/// The verdict that puts a candidate at the end of the queue, unranked (§6.3).
const UNSURE: &str = "unsure";

/// The most of a note an excerpt holds: the judge's 600 tokens (§6.3), at about
/// four characters each.
const EXCERPT_CHARS: usize = 2400;

/// A candidate awaiting a decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The candidate.
    pub cid: String,
    /// The note a relation would be written to.
    pub src: String,
    /// The note it would point at.
    pub dst: String,
    /// `src`'s title. `None` if the note has left the index.
    pub src_title: Option<String>,
    /// `dst`'s title. `None` if the note has left the index.
    pub dst_title: Option<String>,
    /// The relation a decision would be about: the one [`accept`] would take
    /// from the verdicts, until [`Queue::edit`] changes it.
    pub rel: Option<String>,
    /// The signals combined, from 0 to 1.
    pub score: f64,
    /// Every generator that found the pair, sorted.
    pub generators: Vec<String>,
    /// Each generator's evidence in the §6.3 form: `adamic_adar:0.42`,
    /// `schema_gap:depends_on`, `alias_cooccur:'token refresh'`.
    pub why_proposed: Vec<String>,
    /// The latest verdict: `accept`, `reject` or `unsure`.
    pub verdict: Option<String>,
    /// Why the latest verdict says so.
    pub rationale: Option<String>,
}

impl Item {
    fn unranked(&self) -> bool {
        self.verdict.as_deref() == Some(UNSURE)
    }

    /// Best score first, then every `unsure` candidate in no ranking at all.
    fn order(&self, other: &Item) -> Ordering {
        let score = match self.unranked() {
            true => Ordering::Equal,
            false => other.score.total_cmp(&self.score),
        };
        self.unranked()
            .cmp(&other.unranked())
            .then(score)
            .then_with(|| self.cid.cmp(&other.cid))
    }

    fn decision<'a>(&'a self, reason: Option<&'a str>) -> Decision<'a> {
        Decision {
            rel: self.rel.as_deref(),
            reason,
        }
    }
}

/// The candidates of one review session, and how far it has got.
///
/// Accepting or rejecting takes an item out. Deferring and skipping move past
/// it, and it's there again when the queue is next loaded. A refused decision
/// changes nothing, so the item stays current.
#[derive(Debug, Clone, PartialEq)]
pub struct Queue {
    items: Vec<Item>,
    at: usize,
    hidden: usize,
}

impl Queue {
    /// Reads the queue. Nothing here touches the notes.
    ///
    /// A pair below three strikes comes back on the next `kbx suggest`, often
    /// with the cached verdict that was just rejected. Asking again would be
    /// noise, so a candidate proposing a relation already rejected for its pair
    /// is left out and counted in [`Queue::hidden`]. One that names no relation
    /// is a new question each time.
    pub fn load(g: &Graph, ontology: &Ontology) -> rusqlite::Result<Queue> {
        let connection = g.connection();
        let rejected = rejected_relations(connection)?;
        let mut statement = connection.prepare(
            "SELECT c.cid, c.src, c.dst, s.title, d.title, c.score, c.generators, c.features
             FROM candidates c
             LEFT JOIN files s ON s.id = c.src
             LEFT JOIN files d ON d.id = c.dst",
        )?;
        let mut items = Vec::new();
        let mut hidden = 0;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let item = item(connection, row)?;
            let repeat = item.rel.as_deref().and_then(|rel| ontology.lookup(rel));
            let pair = ordered(&item.src, &item.dst);
            if repeat.is_some_and(|key| rejected.contains(&(pair, key.name.to_string()))) {
                hidden += 1;
            } else {
                items.push(item);
            }
        }
        items.sort_by(Item::order);
        Ok(Queue {
            items,
            at: 0,
            hidden,
        })
    }

    /// The relations [`Queue::edit`] can pick from.
    pub fn relations(ontology: &Ontology) -> Vec<&str> {
        let words = ontology.judge.vocabulary.iter().map(String::as_str);
        words.filter(|word| *word != NONE).collect()
    }

    /// Every candidate still undecided, those moved past included.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The candidate up for a decision. `None` at the end of the queue.
    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.at)
    }

    /// How many items came before the current one and are still undecided.
    pub fn passed(&self) -> usize {
        self.at
    }

    /// Candidates left out for proposing a relation already rejected.
    pub fn hidden(&self) -> usize {
        self.hidden
    }

    /// Changes the relation of the current item, for this session. Nothing is
    /// recorded until a decision is.
    pub fn edit(&mut self, ontology: &Ontology, rel: &str) -> Result<(), ReviewError> {
        if !Queue::relations(ontology).contains(&rel) {
            return Err(ReviewError::UnknownRelation(rel.to_string()));
        }
        let item = self.items.get_mut(self.at);
        item.ok_or(ReviewError::EmptyQueue)?.rel = Some(rel.to_string());
        Ok(())
    }

    /// [`accept`]s the current item, under the relation it shows.
    pub fn accept(
        &mut self,
        g: &Graph,
        vault: &Path,
        ontology: &Ontology,
        reason: Option<&str>,
    ) -> Result<Accepted, ReviewError> {
        let item = self.current().ok_or(ReviewError::EmptyQueue)?;
        let accepted = accept(g, vault, ontology, &item.cid, item.decision(reason))?;
        self.items.remove(self.at);
        Ok(accepted)
    }

    /// [`reject`]s the current item, under the relation it shows.
    pub fn reject(
        &mut self,
        g: &Graph,
        ontology: &Ontology,
        reason: Option<&str>,
    ) -> Result<Rejected, ReviewError> {
        let item = self.current().ok_or(ReviewError::EmptyQueue)?;
        let rejected = reject(g, ontology, &item.cid, item.decision(reason))?;
        self.items.remove(self.at);
        Ok(rejected)
    }

    /// [`defer`]s the current item and moves past it.
    pub fn defer(
        &mut self,
        g: &Graph,
        ontology: &Ontology,
        reason: Option<&str>,
    ) -> Result<Deferred, ReviewError> {
        let item = self.current().ok_or(ReviewError::EmptyQueue)?;
        let deferred = defer(g, ontology, &item.cid, item.decision(reason))?;
        self.skip();
        Ok(deferred)
    }

    /// Moves past the current item, recording nothing.
    pub fn skip(&mut self) {
        self.at = self.items.len().min(self.at + 1);
    }
}

/// The start of a note's body, as it is on disk now. `None` if the note can't
/// be read, which a reviewer should see rather than an error.
pub fn excerpt(g: &Graph, vault: &Path, id: &str) -> rusqlite::Result<Option<String>> {
    let Some(file) = OnDisk::read(g.connection(), vault, id)? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&file.bytes);
    let body = match split_frontmatter(&text) {
        Ok(Some(frontmatter)) => &text[frontmatter.body_start..],
        _ => &text,
    };
    Ok(Some(body.trim().chars().take(EXCERPT_CHARS).collect()))
}

fn item(connection: &Connection, row: &Row<'_>) -> rusqlite::Result<Item> {
    let cid: String = row.get(0)?;
    let generators: Option<String> = row.get(6)?;
    let features: Option<String> = row.get(7)?;
    let (verdict, rationale) = latest_verdict(connection, &cid)?.unwrap_or_default();
    Ok(Item {
        rel: judged_rel(connection, &cid)?,
        src: row.get(1)?,
        dst: row.get(2)?,
        src_title: row.get(3)?,
        dst_title: row.get(4)?,
        score: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
        generators: generators
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default(),
        why_proposed: features
            .and_then(|text| serde_json::from_str(&text).ok())
            .map(|features| why_proposed(&features))
            .unwrap_or_default(),
        verdict,
        rationale,
        cid,
    })
}

/// The `verdict` and `rationale` of the row [`judged_rel`] would look at first.
fn latest_verdict(
    connection: &Connection,
    cid: &str,
) -> rusqlite::Result<Option<(Option<String>, Option<String>)>> {
    connection
        .query_row(
            "SELECT verdict, rationale FROM verdicts WHERE cid = ?
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![cid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
}

/// The pairs, lower id first, and the relations rejected for them. The strikes
/// are what `kbx unreject` clears: it leaves `decision` as it was.
fn rejected_relations(connection: &Connection) -> rusqlite::Result<HashSet<RejectedRelation>> {
    let mut statement = connection.prepare(
        "SELECT src, dst, rel FROM reviews
         WHERE decision = 'reject' AND strikes > 0 AND rel <> ?",
    )?;
    let rows = statement.query_map([NONE], |row| {
        let (src, dst): (String, String) = (row.get(0)?, row.get(1)?);
        Ok((ordered(&src, &dst), row.get(2)?))
    })?;
    rows.collect()
}

type RejectedRelation = ((String, String), String);

fn why_proposed(features: &Map<String, Value>) -> Vec<String> {
    let strings = |evidence: &Value, key: &str| -> Vec<String> {
        let list = evidence.get(key).and_then(Value::as_array);
        let words = list.into_iter().flatten().filter_map(Value::as_str);
        words.map(String::from).collect()
    };
    let mut why = Vec::new();
    for (name, evidence) in features {
        match name.as_str() {
            "schema_gap" => {
                let relations = strings(evidence, "relations");
                why.extend(relations.iter().map(|rel| format!("{name}:{rel}")));
            }
            "alias_cooccur" => {
                let terms = strings(evidence, "terms");
                why.extend(terms.iter().map(|term| format!("{name}:'{term}'")));
            }
            _ => {
                let score = evidence.get("score").and_then(Value::as_f64);
                why.push(format!("{name}:{:.2}", score.unwrap_or(0.0)));
            }
        }
    }
    why
}
