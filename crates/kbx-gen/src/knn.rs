use std::collections::{BTreeMap, HashMap};
use std::fs;

use kbx_core::Graph;
use serde_json::json;
use tracing::warn;

use crate::pipeline::Budget;
use crate::search::{Query, Search};
use crate::snapshot::{Snapshot, ordered};
use crate::{Candidate, GenError, Generator, Scope};

/// Hits to ask for per note (§5).
const NEIGHBORS: usize = 20;

/// A query this long says what a note is about; more only slows the search.
const QUERY_CHARS: usize = 1000;

/// The most searches in one run. A qmd search is a process that loads its
/// models, so each costs seconds, whatever the candidate budget says.
pub const MAX_SEARCHES: usize = 100;

/// The topical neighbours of each note, from a [`Search`] backend: one search
/// per note, the least linked notes first, up to [`MAX_SEARCHES`]. Scored by
/// the best hit between the two notes.
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
        self.run_within(g, scope, Budget::default())
    }

    /// Stops searching once it holds a whole run's worth of pairs.
    fn run_within(
        &self,
        g: &Graph,
        scope: &Scope<'_>,
        budget: Budget,
    ) -> Result<Vec<Candidate>, GenError> {
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

        let mut notes: Vec<_> = snapshot
            .notes
            .iter()
            .filter(|(id, _)| scope.note.is_none_or(|wanted| wanted == *id))
            .collect();
        notes.sort_by_key(|(id, _)| (snapshot.degree(id), *id));

        let mut best: BTreeMap<(String, String), Best> = BTreeMap::new();
        let mut searches = 0;
        for (index, (id, note)) in notes.iter().enumerate() {
            if best.len() >= budget.per_run {
                break;
            }
            if searches == MAX_SEARCHES {
                let left = notes.len() - index;
                warn!("qmd_knn stopped at {MAX_SEARCHES} searches, with {left} notes to go");
                break;
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
            let texts: Vec<_> = sections
                .get(*id)
                .into_iter()
                .flatten()
                .map(|(key, start, end)| (key, section_text(&lines, *start, *end)))
                .filter(|(_, text)| !text.is_empty())
                .collect();
            let Some(key) = longest(&texts) else { continue };
            let text = note_text(&texts);
            let query = Query::Note {
                note: id,
                text: &text,
            };
            searches += 1;
            for hit in self.search.search(&query, NEIGHBORS)? {
                let known = snapshot.notes.contains_key(&hit.note);
                if !known || hit.note == **id || snapshot.linked(id, &hit.note) {
                    continue;
                }
                let pair = ordered(id, &hit.note);
                if best.get(&pair).is_none_or(|b| hit.score > b.score) {
                    let sections = [((*id).clone(), key.clone()), (hit.note, hit.section)];
                    let score = hit.score;
                    best.insert(pair, Best { score, sections });
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

/// The key of the section that says the most: the end of a pair on this side,
/// since a note's search can't tell which of its sections found the hit.
fn longest<'a>(texts: &[(&'a String, String)]) -> Option<&'a String> {
    let mut longest = None;
    for (key, text) in texts {
        if longest.is_none_or(|(_, length)| text.len() > length) {
            longest = Some((*key, text.len()));
        }
    }
    longest.map(|(key, _)| key)
}

/// One query for a note, with an equal share of it for every section.
fn note_text(texts: &[(&String, String)]) -> String {
    let share = QUERY_CHARS / texts.len().max(1);
    let parts = texts.iter().map(|(_, text)| text.chars().take(share));
    let parts: Vec<String> = parts.map(Iterator::collect).collect();
    parts.join(" ")
}

/// Lines `start` to `end`, 1-based and inclusive, as one line of limited size.
fn section_text(lines: &[&str], start: usize, end: usize) -> String {
    let end = end.min(lines.len());
    let section = lines.get(start.saturating_sub(1)..end).unwrap_or_default();
    let words = section.iter().flat_map(|line| line.split_whitespace());
    let text = words.collect::<Vec<_>>().join(" ");
    text.chars().take(QUERY_CHARS).collect()
}
