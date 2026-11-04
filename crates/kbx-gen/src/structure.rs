use std::collections::BTreeMap;

use kbx_core::Graph;
use serde_json::json;

use crate::snapshot::Snapshot;
use crate::{Candidate, GenError, Generator, Scope};

/// What a hub counts for, next to any other shared neighbour.
const HUB_DAMPING: f64 = 0.1;

/// The one relation a hub may bridge (§5).
const PART_OF: &str = "part_of";

/// Unlinked pairs with shared typed neighbours, scored by how many.
#[derive(Debug, Clone, Copy)]
pub struct CommonNeighbors;

impl Generator for CommonNeighbors {
    fn name(&self) -> &'static str {
        "common_neighbors"
    }

    fn run(&self, g: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = Snapshot::load(g)?;
        Ok(from_shared(&snapshot, |_| 1.0))
    }
}

/// Unlinked pairs with shared typed neighbours, each worth `1 / ln(degree)`.
#[derive(Debug, Clone, Copy)]
pub struct AdamicAdar;

impl Generator for AdamicAdar {
    fn name(&self) -> &'static str {
        "adamic_adar"
    }

    fn run(&self, g: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = Snapshot::load(g)?;
        // A shared neighbour has at least two neighbours, so ln is above 0.
        Ok(from_shared(&snapshot, |id| {
            1.0 / (snapshot.degree(id) as f64).ln()
        }))
    }
}

/// A candidate for each unlinked pair of neighbours of some note, with `worth`
/// summed over the notes they share and hubs damped.
fn from_shared(snapshot: &Snapshot, worth: impl Fn(&str) -> f64) -> Vec<Candidate> {
    let mut shared: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for (via, members) in &snapshot.neighbors {
        let members: Vec<&str> = members.iter().map(String::as_str).collect();
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                if !snapshot.linked(a, b) {
                    shared.entry((a, b)).or_default().push(via);
                }
            }
        }
    }
    let candidate = |((a, b), via): ((&str, &str), Vec<&str>)| {
        let damped = |id: &&str| match snapshot.hubs.contains(*id) {
            true => worth(id) * HUB_DAMPING,
            false => worth(id),
        };
        Candidate {
            src: a.to_string(),
            dst: b.to_string(),
            score: via.iter().map(damped).sum(),
            features: json!({ "via": via }),
        }
    };
    shared.into_iter().map(candidate).collect()
}

/// `A→X←B` with A and B unlinked, scored by the product of the two relation
/// weights, summed over each X. A hub is an X only by `part_of`.
#[derive(Debug, Clone, Copy)]
pub struct TwoHopBridge;

impl Generator for TwoHopBridge {
    fn name(&self) -> &'static str {
        "two_hop_bridge"
    }

    fn run(&self, g: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = Snapshot::load(g)?;
        let mut into: BTreeMap<&str, BTreeMap<&str, f64>> = BTreeMap::new();
        for edge in &snapshot.forward {
            if snapshot.hubs.contains(&edge.dst) && edge.rel != PART_OF {
                continue;
            }
            let weight = into
                .entry(&edge.dst)
                .or_default()
                .entry(&edge.src)
                .or_default();
            *weight = weight.max(edge.weight);
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
