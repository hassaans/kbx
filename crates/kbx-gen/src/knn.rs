use std::collections::{BTreeMap, HashMap};
use std::fs;

use kbx_core::Graph;
use serde_json::json;
use tracing::warn;

use crate::search::{Query, Search};
use crate::snapshot::{Snapshot, ordered};
use crate::{Candidate, GenError, Generator, Scope};

/// Hits to ask for per section (§5).
const NEIGHBORS: usize = 20;

/// A query this long says what a section is about; more only slows the search.
const QUERY_CHARS: usize = 1000;

/// The topical neighbours of each section, from a [`Search`] backend. Scored
/// by the best hit between the two notes.
pub struct QmdKnn {
    search: Box<dyn Search>,
}

impl QmdKnn {
    /// Searches with `search`.
    pub fn new(search: Box<dyn Search>) -> QmdKnn {
        QmdKnn { search }
    }
}

/// The best hit between two notes, with the section at each end.
struct Best {
    score: f64,
    sections: [(String, String); 2],
}

impl Generator for QmdKnn {
    fn name(&self) -> &'static str {
        "qmd_knn"
    }

    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = Snapshot::load(g)?;
        let mut statement = g.connection().prepare(
            "SELECT id, key, start_line, end_line FROM sections ORDER BY id, start_line",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        let mut sections: HashMap<String, Vec<(String, usize, usize)>> = HashMap::new();
        for row in rows {
            let (id, key, start, end): (String, String, i64, i64) = row?;
            let lines = (key, start as usize, end as usize);
            sections.entry(id).or_default().push(lines);
        }

        let mut best: BTreeMap<(String, String), Best> = BTreeMap::new();
        for (id, note) in &snapshot.notes {
            if scope.note.is_some_and(|wanted| wanted != id) {
                continue;
            }
            let path = scope.vault.join(&note.path);
            let src = match fs::read_to_string(&path) {
                Ok(src) => src,
                Err(error) => {
                    warn!("{}: {error}", path.display());
                    continue;
                }
            };
            let lines: Vec<&str> = src.lines().collect();
            for (key, start, end) in sections.get(id).into_iter().flatten() {
                let text = section_text(&lines, *start, *end);
                if text.is_empty() {
                    continue;
                }
                let query = Query::Section {
                    note: id,
                    key,
                    text: &text,
                };
                for hit in self.search.search(&query, NEIGHBORS)? {
                    let known = snapshot.notes.contains_key(&hit.note);
                    if !known || hit.note == *id || snapshot.linked(id, &hit.note) {
                        continue;
                    }
                    let pair = ordered(id, &hit.note);
                    if best.get(&pair).is_none_or(|b| hit.score > b.score) {
                        let sections = [(id.clone(), key.clone()), (hit.note, hit.section)];
                        let score = hit.score;
                        best.insert(pair, Best { score, sections });
                    }
                }
            }
        }

        let candidates = best.into_iter().map(|((src, dst), best)| {
            let sections: BTreeMap<_, _> = best.sections.into_iter().collect();
            Candidate {
                src,
                dst,
                score: best.score.max(0.0),
                features: json!({ "sections": sections }),
            }
        });
        Ok(candidates.collect())
    }
}

/// Lines `start` to `end`, 1-based and inclusive, as one line of limited size.
fn section_text(lines: &[&str], start: usize, end: usize) -> String {
    let end = end.min(lines.len());
    let section = lines.get(start.saturating_sub(1)..end).unwrap_or_default();
    let words = section.iter().flat_map(|line| line.split_whitespace());
    let text = words.collect::<Vec<_>>().join(" ");
    text.chars().take(QUERY_CHARS).collect()
}
