use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

use kbx_core::{Graph, split_frontmatter};
use serde_json::json;
use tracing::warn;

use crate::snapshot::Snapshot;
use crate::{Candidate, GenError, Generator, Scope};

/// A one-word term shorter than this matches too much to mean anything.
const MIN_WORD_CHARS: usize = 3;

/// The most pairs one term may make. A project's name said in a fifth of a
/// vault is thousands of pairs, all of them saying the same thing; this keeps
/// those from the notes that say it most.
pub const MAX_PAIRS_PER_TERM: usize = 50;

/// The length, spaces included, at which a term stops gaining for being longer.
const FULL_LENGTH_CHARS: f64 = 16.0;

/// What each word after the first adds to a term's length beyond its space: two
/// words are more of a name than one word as long.
const EXTRA_WORD_CHARS: f64 = 4.0;

/// What one mention leaves unsaid: one is 0.8 of what a term can say, two 0.96.
const MENTION_MISS: f64 = 0.2;

/// A title or alias to look for, by its words, and the notes it names, each
/// with the ways it's written there, as `(id, text)` sorted.
struct Term<'a> {
    words: Vec<String>,
    owners: Vec<(&'a str, &'a str)>,
    /// How many notes it names.
    named: usize,
}

impl Term<'_> {
    /// How telling a mention of this term is, from 0 to 1: rarer in the vault,
    /// longer, and naming fewer notes is more. `said` is how many of the
    /// `notes` say it.
    fn specificity(&self, said: usize, notes: usize) -> f64 {
        let (said, notes) = (said as f64, notes as f64);
        let rarity = ((notes + 1.0) / (said + 1.0)).ln() / (notes + 1.0).ln();
        let chars = self.words.iter().map(|w| w.chars().count()).sum::<usize>();
        let extra = self.words.len().saturating_sub(1) as f64;
        let length = (chars as f64 + extra * (1.0 + EXTRA_WORD_CHARS)) / FULL_LENGTH_CHARS;
        (rarity.max(0.0) * length.min(1.0)) / self.named as f64
    }
}

/// What the terms naming one note say in another's body.
struct Found<'a> {
    /// The chance that every mention is a coincidence.
    miss: f64,
    mentions: u32,
    terms: BTreeSet<&'a str>,
}

/// Notes whose body says another note's title or alias without linking it.
/// Matches whole words, ignoring case and the spacing between them.
///
/// The score is the odds of a chance that grows with how telling each term is
/// ([`Term::specificity`]) and how often it's said, so the union reads it as
/// that chance. A term makes at most [`MAX_PAIRS_PER_TERM`] pairs.
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
        let mut named: HashMap<Vec<String>, Vec<(&str, &str)>> = HashMap::new();
        for (id, text) in titles.chain(names) {
            let words = words(text);
            let long_enough = words.len() > 1
                || words
                    .first()
                    .is_some_and(|w| w.chars().count() >= MIN_WORD_CHARS);
            if !long_enough {
                continue;
            }
            named.entry(words).or_default().push((id, text));
        }
        // In order, so that a pair's chance adds up the same way each run.
        let mut terms: Vec<Term<'_>> = named
            .into_iter()
            .map(|(words, mut owners)| {
                owners.sort_unstable();
                owners.dedup();
                let named = owners.chunk_by(|a, b| a.0 == b.0).count();
                Term {
                    words,
                    owners,
                    named,
                }
            })
            .collect();
        terms.sort_unstable_by(|a, b| a.words.cmp(&b.words));
        // By first word, so a body is read once however many terms there are.
        let mut by_first: HashMap<&str, Vec<usize>> = HashMap::new();
        for (at, term) in terms.iter().enumerate() {
            by_first.entry(&term.words[0]).or_default().push(at);
        }

        // For each term, the notes that say it and how often.
        let notes: Vec<&str> = snapshot.notes.keys().map(String::as_str).collect();
        let mut said: Vec<Vec<(u32, u32)>> = vec![Vec::new(); terms.len()];
        for (at, note) in snapshot.notes.values().enumerate() {
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
            let mut counts: BTreeMap<usize, u32> = BTreeMap::new();
            for (start, word) in body.iter().enumerate() {
                for &term in by_first.get(word.as_str()).into_iter().flatten() {
                    if body[start..].starts_with(&terms[term].words) {
                        *counts.entry(term).or_default() += 1;
                    }
                }
            }
            for (term, count) in counts {
                said[term].push((at as u32, count));
            }
        }

        let mut found: BTreeMap<(u32, &str), Found<'_>> = BTreeMap::new();
        for (term, mut sayers) in terms.iter().zip(said) {
            let specificity = term.specificity(sayers.len(), notes.len());
            if specificity <= 0.0 {
                continue;
            }
            // The notes that say it most, then by id, which is their order.
            sayers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut made = 0;
            'sayers: for (src, count) in sayers {
                for owner in term.owners.chunk_by(|a, b| a.0 == b.0) {
                    let dst = owner[0].0;
                    if made == MAX_PAIRS_PER_TERM {
                        break 'sayers;
                    }
                    let id = notes[src as usize];
                    if dst == id || snapshot.linked(id, dst) {
                        continue;
                    }
                    made += 1;
                    let pair = found.entry((src, dst)).or_insert_with(|| Found {
                        miss: 1.0,
                        mentions: 0,
                        terms: BTreeSet::new(),
                    });
                    let telling = 1.0 - MENTION_MISS.powi(count.min(64) as i32);
                    pair.miss *= 1.0 - specificity * telling;
                    pair.mentions += count;
                    pair.terms.extend(owner.iter().map(|(_, text)| text));
                }
            }
        }

        let candidates = found.into_iter().map(|((src, dst), found)| {
            let id = notes[src as usize];
            let chance = 1.0 - found.miss;
            Candidate {
                src: id.to_string(),
                dst: dst.to_string(),
                score: chance / found.miss,
                features: json!({ "src": id, "terms": found.terms, "mentions": found.mentions }),
            }
        });
        Ok(candidates.collect())
    }
}

pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}
