use kbx_core::{Note, parse_note, section_key};
use uuid::Uuid;

fn note(body: &str) -> Note {
    let src = format!("---\nid: NOTE\ntype: Concept\ntitle: T\n---\n{body}");
    parse_note("a.md", &src).unwrap()
}

fn keys(note: &Note) -> Vec<Uuid> {
    note.sections.iter().map(|s| s.key).collect()
}

fn paths(note: &Note) -> Vec<String> {
    note.sections
        .iter()
        .map(|s| s.heading_path.join(" > "))
        .collect()
}

const BODY: &str =
    "Intro.\n\n# One\n\ntext\n\n## Steps\n\na\n\n## Steps\n\nb\n\n# Two\n\n## Steps\n\nc\n";

#[test]
fn keys_are_stable_across_reparses() {
    assert_eq!(keys(&note(BODY)), keys(&note(BODY)));
}

#[test]
fn keys_do_not_depend_on_section_text_or_line_endings() {
    let edited = BODY.replace("text\n", "text\n\nwith a new paragraph\n");
    assert_eq!(keys(&note(BODY)), keys(&note(&edited)));
    let crlf = BODY.replace('\n', "\r\n");
    assert_eq!(keys(&note(BODY)), keys(&note(&crlf)));
}

#[test]
fn renaming_a_heading_changes_its_key_and_its_childrens() {
    let before = note(BODY);
    let after = note(&BODY.replace("# One", "# Uno"));
    let (before, after) = (keys(&before), keys(&after));
    assert_eq!(before[0], after[0], "preamble");
    assert_ne!(before[1], after[1], "renamed heading");
    assert_ne!(before[2], after[2], "child of renamed heading");
    assert_eq!(before[4], after[4], "unrelated heading");
    assert_eq!(before[5], after[5], "unrelated child");
}

#[test]
fn inserting_a_section_does_not_move_other_keys() {
    let before = note(BODY);
    let after = note(&format!("# Zero\n\nnew\n\n{BODY}").replace("Intro.\n\n", ""));
    for section in before
        .sections
        .iter()
        .filter(|s| !s.heading_path.is_empty())
    {
        assert!(
            keys(&after).contains(&section.key),
            "{:?}",
            section.heading_path
        );
    }
}

#[test]
fn duplicate_headings_get_distinct_keys_and_ordinals() {
    let note = note(BODY);
    let ordinals: Vec<_> = note.sections.iter().map(|s| s.ordinal).collect();
    assert_eq!(ordinals, [0, 0, 0, 1, 0, 0]);
    let mut unique = keys(&note);
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), note.sections.len());
}

#[test]
fn keys_depend_on_the_note_id() {
    let path = ["A".to_string()];
    assert_ne!(section_key("X", &path, 0), section_key("Y", &path, 0));
}

#[test]
fn key_inputs_cannot_run_together() {
    let nested = ["a".to_string(), "b".to_string()];
    let joined = ["a b".to_string()];
    let slash = ["a/b".to_string()];
    assert_ne!(section_key("X", &nested, 0), section_key("X", &joined, 0));
    assert_ne!(section_key("X", &nested, 0), section_key("X", &slash, 0));
}

/// Keys are persisted, so the derivation must never change by accident.
#[test]
fn key_derivation_is_pinned() {
    let key = section_key("01J9Z8K3M2Q4R5S6T7V8W9X0Y1", &["Flow".to_string()], 0);
    assert_eq!(key.get_version_num(), 5);
    assert_eq!(key.to_string(), "3d6af885-c434-579b-a4c8-0d7ed6b53c14");
}

#[test]
fn heading_levels_can_skip_and_rise() {
    let note = note("# A\n\n### B\n\n## C\n\n#### D\n\n# E\n");
    assert_eq!(paths(&note), ["A", "A > B", "A > C", "A > C > D", "E"]);
}

#[test]
fn setext_headings_count_and_quoted_or_fenced_ones_do_not() {
    let note = note("Real\n====\n\n> # Quoted\n\n```\n# Fenced\n```\n\n- # Listed\n");
    assert_eq!(paths(&note), ["Real"]);
}

#[test]
fn heading_text_drops_inline_markup() {
    let note = note("## The **bold** `code` [[target|link]] one\n");
    assert_eq!(note.sections[0].heading_path, ["The bold code link one"]);
    assert_eq!(note.sections[0].slug_path, "the-bold-code-link-one");
    assert_eq!(note.sections[0].mentions[0].target, "target");
}

#[test]
fn slugs_fold_punctuation_and_keep_letters() {
    let note = note("# Fixes & Notes: v2.0!\n\n## Café Über\n");
    assert_eq!(note.sections[1].slug_path, "fixes-notes-v2-0/café-über");
}

#[test]
fn blank_preamble_makes_no_section_and_empty_body_makes_none() {
    assert_eq!(paths(&note("\n\n# A\n")), ["A"]);
    assert!(note("").sections.is_empty());
    assert!(note("\n  \n").sections.is_empty());
}

#[test]
fn a_body_without_headings_is_one_section() {
    let note = note("Just text.\n");
    assert_eq!(paths(&note), [""]);
    assert_eq!(note.sections[0].slug_path, "");
}

#[test]
fn sections_are_flat_and_tile_the_body() {
    let note = note(BODY);
    for pair in note.sections.windows(2) {
        assert_eq!(pair[0].byte_range.end, pair[1].byte_range.start);
    }
}

#[test]
fn a_rule_right_after_the_frontmatter_is_not_a_heading() {
    assert_eq!(paths(&note("---\nx\n")), [""]);
}

fn block_ids(note: &Note) -> Vec<&str> {
    note.sections
        .iter()
        .flat_map(|s| &s.blocks)
        .map(|b| b.id.as_str())
        .collect()
}

#[test]
fn block_ids_are_found_where_obsidian_allows_them() {
    let note = note(
        "para one\nstill para ^p-1\n\n- item ^li\n  - sub ^sub\n\n1. loose\n\n   second ^loose\n\n> quote ^q\n",
    );
    assert_eq!(block_ids(&note), ["p-1", "li", "sub", "loose", "q"]);
}

#[test]
fn a_block_id_on_its_own_line_labels_the_block_above() {
    let src = "---\nid: A\ntype: C\ntitle: T\n---\n| a |\n|---|\n| b |\n\n^tbl\n";
    let note = parse_note("a.md", src).unwrap();
    let block = &note.sections[0].blocks[0];
    assert_eq!(block.id, "tbl");
    assert!(src[block.byte_range.clone()].starts_with("| a |"));
    assert!(src[block.byte_range.clone()].trim_end().ends_with("^tbl"));
}

#[test]
fn a_heading_block_id_does_not_change_the_heading() {
    let plain = note("## Steps\n");
    let tagged = note("## Steps ^steps\n");
    assert_eq!(block_ids(&tagged), ["steps"]);
    assert_eq!(keys(&plain), keys(&tagged));
    assert_eq!(note("## ^only").sections[0].heading_path, [""]);
}

#[test]
fn things_that_look_like_block_ids_are_ignored() {
    let note = note("x^2\n\n`a ^b`\n\n```\ncode ^c\n```\n\nfoot[^1]\n\n[^1]: note\n\n^bad_id!\n");
    assert!(block_ids(&note).is_empty());
}
