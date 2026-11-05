use std::collections::{BTreeMap, BTreeSet};

use kbx_core::Graph;
use serde_json::json;

use crate::snapshot::Snapshot;
use crate::{Candidate, GenError, Generator, Scope};

/// What a hub counts for, next to any other shared neighbour.
const HUB_DAMPING: f64 = 0.1;

/// The one relation a hub may bridge (§5).
const PART_OF: &str = "part_of";

/// The snapshot the structural generators read: with inline links, unless
/// their weight is 0.
fn load(g: &Graph, scope: &Scope<'_>) -> rusqlite::Result<Snapshot> {
    let snapshot = Snapshot::load(g)?;
    Ok(match scope.mention_weight > 0.0 {
        true => snapshot,
        false => snapshot.without_inline(),
    })
}

/// Unlinked pairs with shared neighbours, scored by how many.
#[derive(Debug, Clone, Copy)]
pub struct CommonNeighbors;

impl Generator for CommonNeighbors {
    fn name(&self) -> &'static str {
        "common_neighbors"
    }

    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = load(g, scope)?;
        Ok(from_shared(&snapshot, scope.mention_weight, |_| 1.0))
    }
}

/// Unlinked pairs with shared neighbours, each worth `1 / ln(degree)`.
#[derive(Debug, Clone, Copy)]
pub struct AdamicAdar;

impl Generator for AdamicAdar {
    fn name(&self) -> &'static str {
        "adamic_adar"
    }

    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = load(g, scope)?;
        // A shared neighbour has at least two neighbours, so ln is above 0.
        Ok(from_shared(&snapshot, scope.mention_weight, |degree| {
            1.0 / (degree as f64).ln()
        }))
    }
}

/// A candidate for each unlinked pair of neighbours of some note, with `worth`
/// of its degree summed over the notes they share and hubs damped.
///
/// A pair sharing a note by typed edges alone counts as if inline links didn't
/// exist. Otherwise the note's degree takes them in, and each inline end
/// multiplies what it's worth by `mention_weight`.
fn from_shared(
    snapshot: &Snapshot,
    mention_weight: f64,
    worth: impl Fn(usize) -> f64,
) -> Vec<Candidate> {
    let vias: BTreeSet<&String> = snapshot
        .neighbors
        .keys()
        .chain(snapshot.inline.keys())
        .collect();
    let mut shared: BTreeMap<(&str, &str), Vec<(&str, f64)>> = BTreeMap::new();
    for via in vias {
        let typed_ends = ends(snapshot.neighbors.get(via), 0);
        let inline_ends = ends(snapshot.inline.get(via), 1);
        let mut members: Vec<_> = typed_ends.chain(inline_ends).collect();
        members.sort_unstable();
        let typed = match snapshot.hubs.contains(via) {
            true => worth(snapshot.degree(via)) * HUB_DAMPING,
            false => worth(snapshot.degree(via)),
        };
        let loose = worth(snapshot.reach(via));
        for (i, (a, inline_a)) in members.iter().enumerate() {
            for (b, inline_b) in &members[i + 1..] {
                if !snapshot.linked(a, b) {
                    let value = match inline_a + inline_b {
                        0 => typed,
                        inline => loose * mention_weight.powi(inline),
                    };
                    shared.entry((a, b)).or_default().push((via, value));
                }
            }
        }
    }
    let candidate = |((a, b), via): ((&str, &str), Vec<(&str, f64)>)| Candidate {
        src: a.to_string(),
        dst: b.to_string(),
        score: via.iter().map(|(_, value)| value).sum(),
        features: json!({ "via": via.iter().map(|(id, _)| id).collect::<Vec<_>>() }),
    };
    shared.into_iter().map(candidate).collect()
}

/// The notes in `set`, each with how many inline ends it stands for.
fn ends(set: Option<&BTreeSet<String>>, inline: i32) -> impl Iterator<Item = (&str, i32)> {
    let ids = set.into_iter().flatten();
    ids.map(move |id| (id.as_str(), inline))
}

/// `A→X←B` with A and B unlinked, scored by the product of the two relation
/// weights, summed over each X. A hub is an X only by `part_of`. An inline link
/// weighs `mention_weight`, unless a typed edge joins the same two notes and
/// weighs more, and is never `part_of`.
#[derive(Debug, Clone, Copy)]
pub struct TwoHopBridge;

impl Generator for TwoHopBridge {
    fn name(&self) -> &'static str {
        "two_hop_bridge"
    }

    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = load(g, scope)?;
        let typed = snapshot
            .forward
            .iter()
            .filter(|edge| !snapshot.hubs.contains(&edge.dst) || edge.rel == PART_OF)
            .map(|edge| (&edge.src, &edge.dst, edge.weight));
        let inline = snapshot
            .mentions
            .iter()
            .map(|(src, dst)| (src, dst, scope.mention_weight));
        let mut into: BTreeMap<&str, BTreeMap<&str, f64>> = BTreeMap::new();
        for (src, dst, weight) in typed.chain(inline) {
            let best = into.entry(dst).or_default().entry(src).or_default();
            *best = best.max(weight);
        }

        let mut bridged: BTreeMap<(&str, &str), (f64, Vec<&str>)> = BTreeMap::new();
        for (via, sources) in &into {
            let sources: Vec<_> = sources.iter().collect();
            for (i, (a, weight_a)) in sources.iter().enumerate() {
                for (b, weight_b) in &sources[i + 1..] {
                    if !snapshot.linked(a, b) {
                        let bridge = bridged.entry((a, b)).or_default();
                        bridge.0 += *weight_a * *weight_b;
                        bridge.1.push(via);
                    }
                }
            }
        }
        let candidates = bridged.into_iter().map(|((a, b), (score, via))| Candidate {
            src: a.to_string(),
            dst: b.to_string(),
            score,
            features: json!({ "via": via }),
        });
        Ok(candidates.collect())
    }
}
