use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use kbx_core::{Graph, section_excerpt, split_frontmatter};
use kbx_gen::{EXCERPT_TOKENS, Proposal};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::JudgeId;

/// About four characters to a token, as the index counts them.
pub(crate) const CHARS_PER_TOKEN: usize = 4;

const EXCERPT_CHARS: usize = EXCERPT_TOKENS as usize * CHARS_PER_TOKEN;

/// A batch item (§6.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CandidateView {
    /// The candidate.
    pub cid: String,
    /// The first note.
    pub src: NoteView,
    /// The second note.
    pub dst: NoteView,
    /// The signals that proposed the pair, see [`why_proposed`].
    pub why_proposed: Vec<String>,
}

/// One note of a pair, as the judge sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteView {
    /// The note's id.
    pub id: String,
    /// The note's class.
    #[serde(rename = "type")]
    pub class: String,
    /// The note's title.
    pub title: String,
    /// The section a section-level pair is about, or else the start of the
    /// body, without the frontmatter.
    pub excerpt: String,
}

/// About how many input tokens `view` adds to a batch.
pub fn view_tokens(view: &CandidateView) -> i64 {
    let json = serde_json::to_string(view).expect("a view serializes");
    (json.len() / CHARS_PER_TOKEN) as i64
}

/// What [`views`] came to.
#[derive(Debug, Default)]
pub struct Views {
    /// What this judge may see.
    pub ready: Vec<CandidateView>,
    /// Pairs left out because `[judge.overrides]` keeps a note from this judge.
    pub pinned: usize,
    /// Pairs left out because a note is gone or can't be read.
    pub unreadable: usize,
}

/// Builds the batch items for `candidates`, leaving out every pair with a note
/// pinned to a backend other than `judge`.
pub fn views(
    g: &Graph,
    vault: &Path,
    candidates: &[Proposal],
    overrides: &BTreeMap<String, String>,
    judge: JudgeId,
) -> rusqlite::Result<Views> {
    collect(g, vault, candidates, overrides, judge, false)
}

/// Like [`views`], but only the pairs with a note pinned to `judge`: what falls
/// to it however the rest of the run is judged.
pub(crate) fn pinned_views(
    g: &Graph,
    vault: &Path,
    candidates: &[Proposal],
    overrides: &BTreeMap<String, String>,
    judge: JudgeId,
) -> rusqlite::Result<Views> {
    collect(g, vault, candidates, overrides, judge, true)
}

fn collect(
    g: &Graph,
    vault: &Path,
    candidates: &[Proposal],
    overrides: &BTreeMap<String, String>,
    judge: JudgeId,
    only_pinned: bool,
) -> rusqlite::Result<Views> {
    let mut found = Views::default();
    for candidate in candidates {
        let (Some(src), Some(dst)) = (locate(g, &candidate.src)?, locate(g, &candidate.dst)?)
        else {
            // Already counted, when this is the second pass over the pairs.
            found.unreadable += usize::from(!only_pinned);
            continue;
        };
        if pinned_away(&src.0, overrides, judge) || pinned_away(&dst.0, overrides, judge) {
            found.pinned += 1;
            continue;
        }
        if only_pinned && pin(&src.0, overrides).is_none() && pin(&dst.0, overrides).is_none() {
            continue;
        }
        let src = src.read(g, vault, candidate.src_section.as_deref());
        let dst = dst.read(g, vault, candidate.dst_section.as_deref());
        let (Ok(src), Ok(dst)) = (src, dst) else {
            found.unreadable += 1;
            continue;
        };
        found.ready.push(CandidateView {
            cid: candidate.cid.clone(),
            src,
            dst,
            why_proposed: why_proposed(candidate),
        });
    }
    Ok(found)
}

/// A note's path, then what the index knows of it.
struct Located(String, NoteView);

impl Located {
    /// The note with its excerpt: `section` if there is one, or else its start.
    fn read(self, g: &Graph, vault: &Path, section: Option<&str>) -> io::Result<NoteView> {
        let text = fs::read_to_string(vault.join(&self.0))?;
        let cut = section.map(|key| section_excerpt(g.connection(), &text, key));
        let cut = cut.transpose().map_err(io::Error::other)?.flatten();
        Ok(NoteView {
            excerpt: cut.unwrap_or_else(|| excerpt(&text)),
            ..self.1
        })
    }
}

fn locate(g: &Graph, id: &str) -> rusqlite::Result<Option<Located>> {
    let mut statement = g
        .connection()
        .prepare_cached("SELECT path, type, title FROM files WHERE id = ?")?;
    statement
        .query_row(params![id], |row| {
            let view = NoteView {
                id: id.to_string(),
                class: row.get(1)?,
                title: row.get(2)?,
                excerpt: String::new(),
            };
            Ok(Located(row.get(0)?, view))
        })
        .optional()
}

/// Whether the most specific override covering `path` names another backend.
fn pinned_away(path: &str, overrides: &BTreeMap<String, String>, judge: JudgeId) -> bool {
    pin(path, overrides).is_some_and(|backend| backend != judge.as_str())
}

/// What the most specific override covering `path` names.
fn pin<'a>(path: &str, overrides: &'a BTreeMap<String, String>) -> Option<&'a str> {
    let path = path.replace('\\', "/");
    let covers = |folder: &str| {
        let folder = folder.trim_end_matches('/');
        path.strip_prefix(folder)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    overrides
        .iter()
        .filter(|(folder, _)| covers(folder))
        .max_by_key(|(folder, _)| folder.trim_end_matches('/').len())
        .map(|(_, backend)| backend.as_str())
}

fn excerpt(text: &str) -> String {
    let body = match split_frontmatter(text) {
        Ok(Some(frontmatter)) => &text[frontmatter.body_start..],
        _ => text,
    };
    body.trim().chars().take(EXCERPT_CHARS).collect()
}

/// A candidate's provenance in the §6.3 form: `adamic_adar:0.42`,
/// `schema_gap:depends_on`, `alias_cooccur:'token refresh'`.
pub fn why_proposed(candidate: &Proposal) -> Vec<String> {
    let strings = |value: &Value, key: &str| -> Vec<String> {
        let list = value.get(key).and_then(Value::as_array);
        let words = list.into_iter().flatten().filter_map(Value::as_str);
        words.map(String::from).collect()
    };
    let mut why = Vec::new();
    for (name, evidence) in &candidate.features {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    fn overrides(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        let pair = |(k, v): &(&str, &str)| (k.to_string(), v.to_string());
        entries.iter().map(pair).collect()
    }

    #[test]
    fn provenance_reads_as_in_the_spec() {
        let features = json!({
            "adamic_adar": {"score": 0.4213, "via": ["x"]},
            "alias_cooccur": {"score": 1.0, "src": "a", "terms": ["token refresh"]},
            "schema_gap": {"score": 0.5, "src": "a", "relations": ["depends_on"]},
        });
        let features: Map<String, Value> = features.as_object().unwrap().clone();
        let candidate = Proposal {
            cid: "c".to_string(),
            src: "a".to_string(),
            dst: "b".to_string(),
            generators: Vec::new(),
            features,
            score: 0.5,
            src_section: None,
            dst_section: None,
        };
        assert_eq!(
            why_proposed(&candidate),
            [
                "adamic_adar:0.42",
                "alias_cooccur:'token refresh'",
                "schema_gap:depends_on"
            ]
        );
    }

    #[test]
    fn the_excerpt_skips_the_frontmatter_and_stops_at_the_budget() {
        let text = format!("---\nsecret: 1\n---\n\n{}", "é".repeat(EXCERPT_CHARS + 50));
        let found = excerpt(&text);
        assert!(!found.contains("secret"));
        assert_eq!(found.chars().count(), EXCERPT_CHARS);
    }

    #[test]
    fn an_override_covers_its_folder_and_no_lookalike() {
        let pins = overrides(&[("team-y/legal/", "ollama")]);
        let away = |path| pinned_away(path, &pins, JudgeId::ClaudeCli);
        assert!(away("team-y/legal/a.md"));
        assert!(away("team-y/legal/deep/a.md"));
        assert!(!away("team-y/legal-old/a.md"));
        assert!(!away("team-y/a.md"));
    }

    #[test]
    fn the_most_specific_override_wins() {
        let pins = overrides(&[("a", "ollama"), ("a/b", "claude-cli")]);
        assert!(!pinned_away("a/b/n.md", &pins, JudgeId::ClaudeCli));
        assert!(pinned_away("a/n.md", &pins, JudgeId::ClaudeCli));
        assert!(pinned_away("a/b/n.md", &pins, JudgeId::CodexCli));
    }
}
