//! `place_sections` over small written vaults: which section each end of a pair
//! gets, and that whatever a link can reach has an address to show in review.

use std::fs;
use std::path::Path;

use kbx_core::{Exclusions, Graph, IndexOptions, cid, index_vault, parse_note, section_anchor};
use kbx_gen::{Proposal, Run, place_sections, store};
use serde_json::{Value, json};
use tempfile::TempDir;

const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

const A: &str = "# Intro\n\nNothing here.\n\n# Limits\n\nThe token bucket refills. ^limits\n\n\
                 # Other\n\nThe token bucket again, and token bucket once more. ^other\n";
const B: &str = "Text before any heading.\n\n# Steps\n\nFirst.\n\n# Steps\n\nSecond.\n\n\
                 # Rate-limits\n\nOne.\n\n# Rate limits\n\nTwo.\n\n\
                 # Q: A\n\nThree.\n\n# Q A\n\nFour.\n";

struct Written {
    graph: Graph,
    dir: TempDir,
}

fn written() -> Written {
    let dir = TempDir::new().unwrap();
    for (name, id, body) in [("a-note.md", "a", A), ("b-note.md", "b", B)] {
        let head = format!("---\nid: {id}\ntype: Concept\ntitle: {id}\n---\n");
        fs::write(dir.path().join(name), head + body).unwrap();
    }
    let mut graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
    let options = IndexOptions::default();
    index_vault(
        &mut graph,
        dir.path(),
        ONTOLOGY,
        &Exclusions::new(dir.path(), &[]).unwrap(),
        &options,
    )
    .unwrap();
    Written { graph, dir }
}

impl Written {
    /// The key of the `nth` section of note `id` with this heading.
    fn key(&self, id: &str, heading: &str, nth: usize) -> String {
        let name = format!("{id}-note.md");
        let text = fs::read_to_string(self.dir.path().join(&name)).unwrap();
        let note = parse_note(Path::new(&name), &text).unwrap();
        let mut found = note
            .sections
            .iter()
            .filter(|s| s.heading_path.last().map_or("", |h| h.as_str()) == heading);
        found.nth(nth).unwrap().key.to_string()
    }

    fn place(&self, features: Value) -> Proposal {
        let mut run = Run {
            candidates: vec![Proposal {
                cid: cid("a", "b"),
                src: "a".to_string(),
                dst: "b".to_string(),
                generators: Vec::new(),
                features: features.as_object().unwrap().clone(),
                score: 0.5,
                src_section: None,
                dst_section: None,
            }],
            dropped: 0,
            over_budget: 0,
            locked: 0,
            skipped: Vec::new(),
        };
        place_sections(&self.graph, self.dir.path(), &mut run).unwrap();
        run.candidates.remove(0)
    }
}

#[test]
fn a_term_places_an_end_where_it_occurs_most() {
    let vault = written();
    let features = json!({"alias_cooccur": {"src": "a", "terms": ["Token Bucket"], "score": 3.0}});
    let placed = vault.place(features);
    assert_eq!(placed.src_section, Some(vault.key("a", "Other", 0)));
    assert_eq!(placed.dst_section, None, "`b` never says the term");
    assert_ne!(placed.cid, cid("a", "b"));
}

#[test]
fn a_term_comes_before_a_search_hit_and_a_hit_fills_the_other_end() {
    let vault = written();
    let features = json!({
        "alias_cooccur": {"src": "a", "terms": ["token bucket"]},
        "qmd_knn": {"sections": {
            "a": vault.key("a", "Limits", 0),
            "b": vault.key("b", "Steps", 0),
        }},
    });
    let placed = vault.place(features);
    assert_eq!(placed.src_section, Some(vault.key("a", "Other", 0)));
    assert_eq!(placed.dst_section, Some(vault.key("b", "Steps", 0)));
}

#[test]
fn a_section_no_link_can_reach_leaves_its_end_note_level() {
    let vault = written();
    let second_steps = vault.key("b", "Steps", 1);
    let before_any_heading = vault.key("b", "", 0);
    for key in [second_steps, before_any_heading] {
        let placed = vault.place(json!({"qmd_knn": {"sections": {"b": key}}}));
        assert_eq!(placed.dst_section, None);
        assert_eq!(placed.cid, cid("a", "b"));
    }
}

#[test]
fn headings_that_share_a_slug_but_not_a_link_can_both_be_placed() {
    let vault = written();
    for heading in ["Rate-limits", "Rate limits"] {
        let key = vault.key("b", heading, 0);
        let placed = vault.place(json!({"qmd_knn": {"sections": {"b": key}}}));
        assert_eq!(placed.dst_section, Some(key), "{heading}");
    }
}

#[test]
fn a_heading_whose_link_reads_like_an_earlier_one_stays_note_level() {
    let vault = written();
    let first = vault.key("b", "Q: A", 0);
    let placed = vault.place(json!({"qmd_knn": {"sections": {"b": first}}}));
    assert_eq!(placed.dst_section, Some(first));

    let second = vault.key("b", "Q A", 0);
    let placed = vault.place(json!({"qmd_knn": {"sections": {"b": second}}}));
    assert_eq!(placed.dst_section, None);
}

#[test]
fn other_generators_leave_a_pair_at_note_level() {
    let vault = written();
    let placed = vault.place(json!({"adamic_adar": {"score": 0.4}}));
    assert_eq!((placed.src_section, placed.dst_section), (None, None));
    assert_eq!(placed.cid, cid("a", "b"));
}

#[test]
fn whatever_a_link_can_reach_has_an_address() {
    let vault = written();
    for (name, id) in [("a-note.md", "a"), ("b-note.md", "b")] {
        let text = fs::read_to_string(vault.dir.path().join(name)).unwrap();
        let note = parse_note(Path::new(name), &text).unwrap();
        assert_eq!(note.id, id);
        for section in &note.sections {
            let key = section.key.to_string();
            let indexed = section_anchor(vault.graph.connection(), &key).unwrap();
            let fragment = note.section_fragment(&key);
            assert!(indexed.is_some() || fragment.is_none(), "{key}");
            if let Some(block) = fragment.filter(|f| f.starts_with('^')) {
                assert_eq!(indexed, Some(block));
            }
        }
    }
}

#[test]
fn a_pair_has_one_row_whichever_sections_it_is_about() {
    let vault = written();
    let run = |proposal: Proposal| Run {
        candidates: vec![proposal],
        dropped: 0,
        over_budget: 0,
        locked: 0,
        skipped: Vec::new(),
    };
    let first = vault.place(json!({"qmd_knn": {"sections": {"a": vault.key("a", "Limits", 0)}}}));
    let second = vault.place(json!({"qmd_knn": {"sections": {"a": vault.key("a", "Other", 0)}}}));
    assert_ne!(first.cid, second.cid);
    store(&vault.graph, &run(first)).unwrap();
    store(&vault.graph, &run(second.clone())).unwrap();
    let cids: String = vault
        .graph
        .connection()
        .query_row("SELECT group_concat(cid) FROM candidates", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(cids, second.cid);
}
