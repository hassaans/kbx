//! Narrows a run's pairs to sections, for `write.section_anchors`. The rules
//! are in `docs/sections.md`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use kbx_core::{Graph, parse_note, section_cid};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value};
use tracing::warn;

use crate::GenError;
use crate::alias::words;
use crate::pipeline::{Proposal, Run};

/// A section of a note, as placing a pair needs it.
struct Read {
    key: String,
    words: Vec<String>,
    /// A link can reach it.
    linkable: bool,
}

/// A note's sections, in file order.
type Sections = Vec<Read>;

/// Gives each pair `qmd_knn` or `alias_cooccur` found the section at each end
/// that a link can reach, and the [`section_cid`] that goes with them. A pair
/// with neither stays as it was, cid and all.
pub fn place_sections(g: &Graph, vault: &Path, run: &mut Run) -> Result<(), GenError> {
    let mut read: HashMap<String, Sections> = HashMap::new();
    for proposal in &mut run.candidates {
        let terms = alias_terms(&proposal.features);
        let src = place(g, vault, &mut read, proposal, &proposal.src, &terms)?;
        let dst = place(g, vault, &mut read, proposal, &proposal.dst, &terms)?;
        proposal.cid = section_cid(&proposal.src, &proposal.dst, src.as_deref(), dst.as_deref());
        proposal.src_section = src;
        proposal.dst_section = dst;
    }
    Ok(())
}

/// Drops the candidates with sections, once `write.section_anchors` is off, so
/// that no anchor gets written after it. The run being stored proposes them
/// again as note-level pairs.
pub fn forget_sections(g: &Graph) -> rusqlite::Result<()> {
    g.connection().execute(
        "DELETE FROM candidates WHERE src_section IS NOT NULL OR dst_section IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// The section of `id` for this pair: where the terms occur most, which is exact
/// evidence, or else what the search found. Either way a link has to reach it.
fn place(
    g: &Graph,
    vault: &Path,
    read: &mut HashMap<String, Sections>,
    proposal: &Proposal,
    id: &str,
    terms: &[Vec<String>],
) -> rusqlite::Result<Option<String>> {
    if !read.contains_key(id) {
        read.insert(id.to_string(), read_sections(g, vault, id)?);
    }
    let sections = &read[id];
    let found = most_terms(sections, terms);
    let searched = search_section(&proposal.features, id);
    let linkable = |key: &String| sections.iter().any(|s| s.key == *key && s.linkable);
    Ok([found, searched].into_iter().flatten().find(linkable))
}

/// What `alias_cooccur` matched, either way round, each term as its words.
fn alias_terms(features: &Map<String, Value>) -> Vec<Vec<String>> {
    let Some(evidence) = features.get("alias_cooccur") else {
        return Vec::new();
    };
    let reverse = evidence.get("reverse");
    let lists = [Some(evidence), reverse].into_iter().flatten();
    let lists = lists.filter_map(|evidence| evidence.get("terms")?.as_array());
    let terms = lists.flatten().filter_map(Value::as_str);
    terms.map(words).collect()
}

/// The section `qmd_knn` recorded for `id`.
fn search_section(features: &Map<String, Value>, id: &str) -> Option<String> {
    let sections = features.get("qmd_knn")?.get("sections")?;
    sections.get(id)?.as_str().map(String::from)
}

/// The key of the section of `id` that says `terms` most often, the first of
/// any that tie. `None` if the note says none of them.
fn most_terms(sections: &Sections, terms: &[Vec<String>]) -> Option<String> {
    let mut best: Option<(&String, usize)> = None;
    for section in sections {
        let words = &section.words;
        let hits = (0..words.len())
            .filter(|at| terms.iter().any(|term| words[*at..].starts_with(term)))
            .count();
        if hits > best.map_or(0, |(_, most)| most) {
            best = Some((&section.key, hits));
        }
    }
    best.map(|(key, _)| key.clone())
}

fn read_sections(g: &Graph, vault: &Path, id: &str) -> rusqlite::Result<Sections> {
    let connection = g.connection();
    let path: Option<String> = connection
        .query_row("SELECT path FROM files WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(path) = path.map(|path| vault.join(path)) else {
        return Ok(Vec::new());
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            warn!("{}: {error}", path.display());
            return Ok(Vec::new());
        }
    };
    let note = match parse_note(&path, &text) {
        Ok(note) => note,
        Err(error) => {
            warn!("{}: {error}", path.display());
            return Ok(Vec::new());
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut statement = connection.prepare_cached(
        "SELECT key, start_line, end_line FROM sections WHERE id = ? ORDER BY start_line",
    )?;
    let rows = statement.query_map(params![id], |row| {
        let (key, start, end): (String, i64, i64) = (row.get(0)?, row.get(1)?, row.get(2)?);
        Ok((key, start as usize, end as usize))
    })?;
    let mut sections = Vec::new();
    for row in rows {
        let (key, start, end) = row?;
        let end = end.min(lines.len());
        let section = lines.get(start.saturating_sub(1)..end).unwrap_or_default();
        sections.push(Read {
            linkable: note.section_fragment(&key).is_some(),
            words: words(&section.join("\n")),
            key,
        });
    }
    Ok(sections)
}
