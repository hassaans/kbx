use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use kbx_core::{Graph, cid, locked_notes, suppressed_pairs};
use rusqlite::{Transaction, TransactionBehavior, params};
use serde_json::{Map, Value};

use crate::alias::AliasCooccur;
use crate::knn::QmdKnn;
use crate::prior::TypePairPrior;
use crate::schema::SchemaGap;
use crate::search::Search;
use crate::snapshot::{Snapshot, ordered};
use crate::structure::{AdamicAdar, CommonNeighbors, TwoHopBridge};
use crate::{GenError, Generator, Scope};

/// Every generator. The union keeps the direction of the first to find a pair,
/// so the ones whose direction means something come first.
const NAMES: [&str; 7] = [
    "schema_gap",
    "alias_cooccur",
    "common_neighbors",
    "adamic_adar",
    "two_hop_bridge",
    "qmd_knn",
    "type_pair_prior",
];

/// The most of a note the judge sees (§6.3).
pub const EXCERPT_TOKENS: i64 = 600;

/// The ids, type, title and `why_proposed` of a batch item, around its excerpts.
const ITEM_TOKENS: i64 = 80;

/// How many candidates a run may keep (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The most pairs any one note may be in, at either end.
    pub per_note: usize,
    /// The most pairs in all.
    pub per_run: usize,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            per_note: 40,
            per_run: 2000,
        }
    }
}

/// A row of the `candidates` table.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// See [`cid`].
    pub cid: String,
    /// The note a relation would be written to.
    pub src: String,
    /// The note it would point at.
    pub dst: String,
    /// Every contributor, by name, sorted.
    pub generators: Vec<String>,
    /// Each contributor's raw `score` and evidence, under its name.
    pub features: Map<String, Value>,
    /// The signals combined, from 0 to 1.
    pub score: f64,
    /// The section key at the `src` end, once [`place_sections`] has found one.
    ///
    /// [`place_sections`]: crate::place_sections
    pub src_section: Option<String>,
    /// The section key at the `dst` end, likewise.
    pub dst_section: Option<String>,
}

/// What [`suggest`] came to.
#[derive(Debug)]
pub struct Run {
    /// What to keep, best first.
    pub candidates: Vec<Proposal>,
    /// Pairs dropped for being linked or suppressed by rejections.
    pub dropped: usize,
    /// Pairs left out for touching a note under a `kbx.reviewed` lock.
    pub locked: usize,
    /// Pairs the [`Budget`] left out.
    pub over_budget: usize,
    /// Generators that failed, by name, and why.
    pub skipped: Vec<(String, GenError)>,
}

/// A name in `--generators` that isn't a generator.
#[derive(Debug, thiserror::Error)]
#[error("unknown generator {0}; the generators are {list}", list = NAMES.join(", "))]
pub struct UnknownGenerator(String);

/// The generators called `names`, or all of them, in the union's order.
/// `qmd_knn` needs `search`, and without one is left out of "all of them".
pub fn generators(
    names: Option<&[String]>,
    search: Option<Box<dyn Search>>,
) -> Result<Vec<Box<dyn Generator>>, UnknownGenerator> {
    if let Some(unknown) = names
        .into_iter()
        .flatten()
        .find(|name| !NAMES.contains(&name.as_str()))
    {
        return Err(UnknownGenerator(unknown.clone()));
    }
    let wanted = |name: &str| names.is_none_or(|names| names.iter().any(|n| n == name));
    let mut search = search;
    let mut list: Vec<Box<dyn Generator>> = Vec::new();
    for name in NAMES.into_iter().filter(|name| wanted(name)) {
        match name {
            "schema_gap" => list.push(Box::new(SchemaGap)),
            "alias_cooccur" => list.push(Box::new(AliasCooccur)),
            "common_neighbors" => list.push(Box::new(CommonNeighbors)),
            "adamic_adar" => list.push(Box::new(AdamicAdar)),
            "two_hop_bridge" => list.push(Box::new(TwoHopBridge)),
            "type_pair_prior" => list.push(Box::new(TypePairPrior)),
            _ => {
                if let Some(search) = search.take() {
                    list.push(Box::new(QmdKnn::new(search)));
                }
            }
        }
    }
    Ok(list)
}

/// Runs `generators`, unions their pairs, and applies the filters and budget.
/// A generator that fails is listed in [`Run::skipped`]; the rest carry on.
pub fn suggest(
    g: &Graph,
    generators: &[Box<dyn Generator>],
    scope: &Scope<'_>,
    budget: Budget,
) -> Result<Run, GenError> {
    let mut union: BTreeMap<(String, String), Merged> = BTreeMap::new();
    let mut skipped = Vec::new();
    for generator in generators {
        let name = generator.name();
        let candidates = match generator.run_within(g, scope, budget) {
            Ok(candidates) => candidates,
            Err(error) => {
                skipped.push((name.to_string(), error));
                continue;
            }
        };
        for candidate in candidates {
            let touches = |id: &str| candidate.src == id || candidate.dst == id;
            if candidate.src == candidate.dst || scope.note.is_some_and(|id| !touches(id)) {
                continue;
            }
            let pair = ordered(&candidate.src, &candidate.dst);
            let merged = union.entry(pair).or_insert_with(|| Merged {
                src: candidate.src.clone(),
                dst: candidate.dst.clone(),
                features: Map::new(),
                miss: 1.0,
            });
            let mut evidence = match candidate.features {
                Value::Object(evidence) => evidence,
                _ => Map::new(),
            };
            evidence.insert("score".to_string(), candidate.score.into());
            merged.miss *= 1.0 - squash(candidate.score);
            let aligned = candidate.src == merged.src;
            merged.add(name, evidence, aligned);
        }
    }

    let snapshot = Snapshot::load(g)?;
    let suppressed = suppressed_pairs(g.connection())?;
    // At either end: the schema may yet turn a pair around, and accepting
    // writes to both notes.
    let locked = locked_notes(g.connection())?;
    let reachable = union.len();
    union.retain(|pair, _| !locked.contains(&pair.0) && !locked.contains(&pair.1));
    let locked = reachable - union.len();
    let found = union.len();
    let mut proposals: Vec<Proposal> = union
        .into_iter()
        .filter(|(pair, _)| !suppressed.contains(pair) && !snapshot.linked(&pair.0, &pair.1))
        .map(|(_, merged)| merged.proposal())
        .collect();
    let dropped = found - proposals.len();

    for generator in generators {
        if let Err(error) = generator.rerank(g, scope, &mut proposals) {
            skipped.push((generator.name().to_string(), error));
        }
    }
    proposals.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.cid.cmp(&b.cid)));
    let total = proposals.len();
    let mut per_note: HashMap<String, usize> = HashMap::new();
    let mut candidates = Vec::new();
    for proposal in proposals {
        let spent = |id: &str| per_note.get(id).copied().unwrap_or(0);
        if candidates.len() < budget.per_run
            && spent(&proposal.src) < budget.per_note
            && spent(&proposal.dst) < budget.per_note
        {
            *per_note.entry(proposal.src.clone()).or_default() += 1;
            *per_note.entry(proposal.dst.clone()).or_default() += 1;
            candidates.push(proposal);
        }
    }
    Ok(Run {
        over_budget: total - candidates.len(),
        candidates,
        dropped,
        locked,
        skipped,
    })
}

/// A pair during the union.
struct Merged {
    src: String,
    dst: String,
    features: Map<String, Value>,
    /// The chance that every signal so far is wrong.
    miss: f64,
}

impl Merged {
    /// Records what `name` found. If it found the pair both ways, the way that
    /// matches `src` comes first, with the other under `reverse`.
    fn add(&mut self, name: &str, evidence: Map<String, Value>, aligned: bool) {
        let (mut main, reverse) = match self.features.remove(name) {
            None => (evidence, None),
            Some(earlier) if aligned => (evidence, Some(earlier)),
            Some(Value::Object(earlier)) => (earlier, Some(evidence.into())),
            Some(_) => (evidence, None),
        };
        if let Some(reverse) = reverse {
            main.insert("reverse".to_string(), reverse);
        }
        self.features.insert(name.to_string(), main.into());
    }

    fn proposal(self) -> Proposal {
        Proposal {
            cid: cid(&self.src, &self.dst),
            generators: self.features.keys().cloned().collect(),
            src: self.src,
            dst: self.dst,
            features: self.features,
            score: 1.0 - self.miss,
            src_section: None,
            dst_section: None,
        }
    }
}

/// Puts a raw score of any scale between 0 and 1, keeping its order, so that
/// no generator's scale drowns out the others.
fn squash(raw: f64) -> f64 {
    let raw = raw.max(0.0);
    raw / (1.0 + raw)
}

/// Writes a run to `candidates`, in one transaction.
///
/// A run can be limited by note or generator, so rows it didn't find stay. A
/// pair it did find is replaced, keeping its `created_at`. A pair since linked
/// or suppressed, or whose notes are gone, is neither kept nor written. Each
/// row records the `content_hash` of both notes, for `kbx accept` to check. A
/// pair has one row, so one with other sections, and another cid, gives way.
pub fn store(g: &Graph, run: &Run) -> Result<(), GenError> {
    let connection = g.connection();
    // Immediate, so that what's checked here is still true at the commit.
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    let snapshot = Snapshot::load(g)?;
    let suppressed = suppressed_pairs(connection)?;
    let eligible = |src: &str, dst: &str| {
        snapshot.notes.contains_key(src)
            && snapshot.notes.contains_key(dst)
            && !snapshot.linked(src, dst)
            && !suppressed.contains(&ordered(src, dst))
    };

    let mut statement = tx.prepare("SELECT cid, src, dst FROM candidates")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let stored = rows.collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    drop(statement);
    for (cid, src, dst) in stored {
        if !eligible(&src, &dst) {
            tx.execute("DELETE FROM candidates WHERE cid = ?", [cid])?;
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let fresh = run.candidates.iter().filter(|c| eligible(&c.src, &c.dst));
    for candidate in fresh {
        let generators = Value::from(candidate.generators.clone()).to_string();
        let features = Value::from(candidate.features.clone()).to_string();
        tx.execute(
            "DELETE FROM candidates WHERE cid <> ?1
                 AND ((src = ?2 AND dst = ?3) OR (src = ?3 AND dst = ?2))",
            params![candidate.cid, candidate.src, candidate.dst],
        )?;
        tx.execute(
            "INSERT INTO candidates (cid, src, dst, generators, features, score, created_at,
                 src_hash, dst_hash, src_section, dst_section)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 (SELECT content_hash FROM files WHERE id = ?2),
                 (SELECT content_hash FROM files WHERE id = ?3), ?8, ?9)
             ON CONFLICT(cid) DO UPDATE SET src = excluded.src, dst = excluded.dst,
                 generators = excluded.generators, features = excluded.features,
                 score = excluded.score, src_hash = excluded.src_hash,
                 dst_hash = excluded.dst_hash, src_section = excluded.src_section,
                 dst_section = excluded.dst_section",
            params![
                candidate.cid,
                candidate.src,
                candidate.dst,
                generators,
                features,
                candidate.score,
                now,
                candidate.src_section,
                candidate.dst_section
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// The tokens each candidate would add to a judge batch: an excerpt from each
/// note, at most [`EXCERPT_TOKENS`], and the item around them.
pub fn estimate_tokens(g: &Graph, candidates: &[Proposal]) -> rusqlite::Result<Vec<i64>> {
    let mut statement = g
        .connection()
        .prepare("SELECT id, SUM(tokens) FROM sections GROUP BY id")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let notes = rows.collect::<rusqlite::Result<HashMap<String, i64>>>()?;
    let excerpt = |id: &str| notes.get(id).copied().unwrap_or(0).min(EXCERPT_TOKENS);
    let estimate = |c: &Proposal| excerpt(&c.src) + excerpt(&c.dst) + ITEM_TOKENS;
    Ok(candidates.iter().map(estimate).collect())
}
