use std::collections::{BTreeMap, HashMap};

use kbx_core::{Graph, NONE, Ontology, ordered};
use serde_json::json;

use crate::pipeline::Proposal;
use crate::snapshot::Snapshot;
use crate::{Candidate, GenError, Generator, Scope};

/// Pseudo-counts on each side of an estimate: an unseen type pair sits at 1/2,
/// and one accept is 2/3, not certainty.
const SMOOTHING: f64 = 1.0;

/// Re-ranks the run's pairs by how their two types fared in `reviews` (§5). It
/// proposes nothing, so it can't make a pair appear, only move it.
///
/// Types count as a pair, not in order. A pair is one question whichever way
/// the generators found it (§3), and a rejection records the candidate's order,
/// not the schema's. No judge has run yet, so a candidate has no relation to
/// look up: it gets the best relation its types can hold. Decisions are weighed
/// once per row, and `kbx unreject` lifts a rejection's strikes, not its
/// verdict on the pair.
#[derive(Debug, Clone, Copy)]
pub struct TypePairPrior;

pub(crate) const NAME: &str = "type_pair_prior";

impl Generator for TypePairPrior {
    fn name(&self) -> &'static str {
        NAME
    }

    fn run(&self, _: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        Ok(Vec::new())
    }

    fn rerank(
        &self,
        g: &Graph,
        scope: &Scope<'_>,
        proposals: &mut [Proposal],
    ) -> Result<(), GenError> {
        let learned = Learned::load(g, scope.ontology)?;
        if learned.0.is_empty() {
            return Ok(());
        }
        let snapshot = Snapshot::load(g)?;
        for proposal in proposals {
            let class = |id: &str| snapshot.notes.get(id).map(|note| note.class.as_str());
            let (Some(src), Some(dst)) = (class(&proposal.src), class(&proposal.dst)) else {
                continue;
            };
            let Some(estimate) = learned.estimate(src, dst, scope.ontology) else {
                continue;
            };
            proposal.score = shifted(proposal.score, estimate.chance);
            let evidence = json!({
                "score": estimate.chance,
                "rel": estimate.rel,
                "accepts": estimate.tally.accepts,
                "rejects": estimate.tally.rejects,
            });
            proposal.features.insert(NAME.to_string(), evidence);
            proposal.generators = proposal.features.keys().cloned().collect();
        }
        Ok(())
    }
}

/// `score` as a chance, updated by the odds of `chance`. At 1/2 it stays put,
/// and it stays between 0 and 1.
fn shifted(score: f64, chance: f64) -> f64 {
    let up = score * chance;
    up / (up + (1.0 - score) * (1.0 - chance))
}

#[derive(Debug, Clone, Copy, Default)]
struct Tally {
    accepts: u32,
    rejects: u32,
}

impl Tally {
    fn chance(self) -> f64 {
        let (accepts, rejects) = (f64::from(self.accepts), f64::from(self.rejects));
        (accepts + SMOOTHING) / (accepts + rejects + 2.0 * SMOOTHING)
    }
}

struct Estimate<'a> {
    /// The best relation, unless all there is to go on names none.
    rel: Option<&'a str>,
    tally: Tally,
    chance: f64,
}

/// By the two types, lower first, then relation or [`NONE`].
struct Learned(HashMap<(String, String), BTreeMap<String, Tally>>);

impl Learned {
    /// Reads `reviews`, skipping deferrals, notes not in the index and
    /// relations the ontology has dropped.
    fn load(g: &Graph, ontology: &Ontology) -> rusqlite::Result<Learned> {
        let mut statement = g.connection().prepare(
            "SELECT s.type, d.type, r.rel, r.decision FROM reviews r
             JOIN files s ON s.id = r.src
             JOIN files d ON d.id = r.dst
             WHERE r.decision IN ('accept', 'reject')",
        )?;
        let mut learned: HashMap<_, BTreeMap<String, Tally>> = HashMap::new();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let (src, dst): (String, String) = (row.get(0)?, row.get(1)?);
            let (rel, decision): (String, String) = (row.get(2)?, row.get(3)?);
            let name = match ontology.lookup(&rel) {
                Some(key) => key.name,
                None if rel == NONE => NONE,
                None => continue,
            };
            let tally = learned
                .entry(ordered(&src, &dst))
                .or_default()
                .entry(name.to_string())
                .or_default();
            match decision.as_str() {
                "accept" => tally.accepts += 1,
                _ => tally.rejects += 1,
            }
        }
        Ok(Learned(learned))
    }

    /// The chance of a link between notes of these types, or `None` with
    /// nothing to go on. A rejection naming no relation says no to all of them.
    fn estimate<'a>(&'a self, a: &str, b: &str, ontology: &Ontology) -> Option<Estimate<'a>> {
        let tallies = self.0.get(&ordered(a, b))?;
        let blanket = tallies.get(NONE).map_or(0, |tally| tally.rejects);
        let mut best: Option<Estimate<'a>> = None;
        for (rel, tally) in tallies {
            if rel == NONE || ontology.orient(rel, a, b).is_none() {
                continue;
            }
            let tally = Tally {
                rejects: tally.rejects + blanket,
                ..*tally
            };
            let chance = tally.chance();
            if best.as_ref().is_none_or(|best| chance > best.chance) {
                let rel = Some(rel.as_str());
                best = Some(Estimate { rel, tally, chance });
            }
        }
        best.or_else(|| {
            let tally = *tallies.get(NONE)?;
            let chance = tally.chance();
            Some(Estimate {
                rel: None,
                tally,
                chance,
            })
        })
    }
}
