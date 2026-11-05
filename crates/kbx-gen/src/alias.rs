use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

use kbx_core::{Graph, split_frontmatter};
use serde_json::json;
use tracing::warn;

use crate::snapshot::Snapshot;
use crate::{Candidate, GenError, Generator, Scope};

/// A one-word term shorter than this matches too much to mean anything.
const MIN_WORD_CHARS: usize = 3;

/// A title or alias to look for: its words, as written, and whose it is.
struct Term<'a> {
    words: Vec<String>,
    text: &'a str,
    id: &'a str,
}

/// Notes whose body says another note's title or alias without linking it.
/// Matches whole words, ignoring case and the spacing between them.
#[derive(Debug, Clone, Copy)]
pub struct AliasCooccur;

impl Generator for AliasCooccur {
    fn name(&self) -> &'static str {
        "alias_cooccur"
    }

    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let snapshot = Snapshot::load(g)?;
        let mut statement = g.connection().prepare("SELECT id, alias FROM aliases")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let aliases = rows.collect::<rusqlite::Result<Vec<(String, String)>>>()?;

        let titles = snapshot.notes.iter().map(|(id, n)| (&**id, &*n.title));
        let names = aliases.iter().map(|(id, alias)| (&**id, &**alias));
        // By first word, so a body is read once however many terms there are.
        let mut terms: HashMap<String, Vec<Term<'_>>> = HashMap::new();
        for (id, text) in titles.chain(names) {
            let words = words(text);
            let long_enough = words.len() > 1
                || words
                    .first()
                    .is_some_and(|w| w.chars().count() >= MIN_WORD_CHARS);
            if long_enough {
                let term = Term {
                    text,
                    id,
                    words: words.clone(),
                };
                terms.entry(words[0].clone()).or_default().push(term);
            }
        }

        let mut candidates = Vec::new();
        for (id, note) in &snapshot.notes {
            let path = scope.vault.join(&note.path);
            let src = match fs::read_to_string(&path) {
                Ok(src) => src,
                Err(error) => {
                    warn!("{}: {error}", path.display());
                    continue;
                }
            };
            let body = match split_frontmatter(&src) {
                Ok(Some(frontmatter)) => &src[frontmatter.body_start..],
                Ok(None) => &src,
                Err(_) => continue,
            };
            let body = words(body);

            let mut found: BTreeMap<&str, (u32, BTreeSet<&str>)> = BTreeMap::new();
            for (at, word) in body.iter().enumerate() {
                for term in terms.get(word).into_iter().flatten() {
                    if term.id != id && body[at..].starts_with(&term.words) {
                        let hits = found.entry(term.id).or_default();
                        hits.0 += 1;
                        hits.1.insert(term.text);
                    }
                }
            }
            for (dst, (count, terms)) in found {
                if !snapshot.linked(id, dst) {
                    candidates.push(Candidate {
                        src: id.clone(),
                        dst: dst.to_string(),
                        score: f64::from(count),
                        features: json!({ "src": id, "terms": terms }),
                    });
                }
            }
        }
        Ok(candidates)
    }
}

pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}
