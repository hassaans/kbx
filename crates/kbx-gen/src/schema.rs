use std::collections::BTreeMap;

use kbx_core::{ANY, Graph};
use serde_json::json;

use crate::snapshot::Snapshot;
use crate::{Candidate, GenError, Generator, Scope};

/// Low, because §5 ranks these by the other signals: a pair with nothing else
/// going for it sorts below any pair that has.
const SCORE: f64 = 0.1;

/// Notes missing a relation their class `requires`, paired with each note of
/// the same scope that the relation's `range` allows.
#[derive(Debug, Clone, Copy)]
pub struct SchemaGap;

impl Generator for SchemaGap {
    fn name(&self) -> &'static str {
        "schema_gap"
    }

    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = Snapshot::load(g)?;
        // A hidden link has no edge but still meets the requirement, as in
        // `kbx lint`. An inverse stated by the target has an edge and no link.
        let mut met = g.connection().prepare(
            "SELECT EXISTS(SELECT 1 FROM links WHERE src = ?1 AND key = ?2)
                 OR EXISTS(SELECT 1 FROM edges WHERE src = ?1 AND rel = ?2)",
        )?;

        let mut gaps: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
        for (id, note) in &snapshot.notes {
            let Some(class) = scope.ontology.classes.get(&note.class) else {
                continue;
            };
            for key in &class.requires {
                if met.query_row([id, key], |row| row.get(0))? {
                    continue;
                }
                let Some(relation) = scope.ontology.lookup(key) else {
                    continue;
                };
                let range = relation.range();
                let any = range.iter().any(|class| class == ANY);
                for (other, target) in &snapshot.notes {
                    let fits = any || range.contains(&target.class);
                    if fits
                        && other != id
                        && target.scope == note.scope
                        && !snapshot.linked(id, other)
                    {
                        gaps.entry((id, other)).or_default().push(key);
                    }
                }
            }
        }
        let candidates = gaps.into_iter().map(|((src, dst), relations)| Candidate {
            src: src.to_string(),
            dst: dst.to_string(),
            score: SCORE,
            features: json!({ "relations": relations }),
        });
        Ok(candidates.collect())
    }
}
