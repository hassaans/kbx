use kbx_core::{ParseError, parse_note, split_frontmatter};

/// The frontmatter from spec §3, verbatim, with a body added.
const EXAMPLE: &str = r#"---
id: 01J9Z8K3M2Q4R5S6T7V8W9X0Y1     # ULID; also the filename prefix. Never changes.
type: Procedure                    # must be a class in ontology.yaml
title: OAuth 2.0 authorization flow
aliases: [oauth flow, authorization code grant]
tags: [auth, security]
depends_on: ["[[01J9Z8…tokens]]", "[[01J9Z8…redirects]]"]   # relation keys = lists of quoted wikilinks
part_of: ["[[01J9Z8…overview]]"]
kbx:
  proposed:                        # staging area written by `kbx suggest`; never read by the compiler
    - {rel: see_also, to: "[[01J9Z8…pkce]]", by: [adamic_adar, judge], score: 0.81, id: 7f3a…}
  reviewed: 2026-11-03             # optional lock: skip re-suggestion until content hash changes
qmd:
  metadata: {type: Procedure, tags: [auth, security]}   # mirrored for QMD --filter
---
Intro before any heading.

# Flow

The client redirects the user. ^redirect-step

## Exchange

Swap the code for a token, see [[01J9Z8…tokens]].
"#;

fn minimal(extra: &str) -> String {
    format!("---\nid: A\ntype: Concept\ntitle: T\n{extra}---\nBody.\n")
}

#[test]
fn parses_the_spec_example() {
    let note = parse_note("01J9Z8K3M2Q4R5S6T7V8W9X0Y1-oauth.md", EXAMPLE).unwrap();

    assert_eq!(note.id, "01J9Z8K3M2Q4R5S6T7V8W9X0Y1");
    assert_eq!(note.class, "Procedure");
    assert_eq!(note.title, "OAuth 2.0 authorization flow");
    assert_eq!(note.aliases, ["oauth flow", "authorization code grant"]);
    assert_eq!(note.tags, ["auth", "security"]);

    let keys: Vec<_> = note.relations.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["depends_on", "part_of"]);
    let depends_on = note.relation("depends_on").unwrap();
    let targets: Vec<_> = depends_on.links.iter().map(|l| l.target.as_str()).collect();
    assert_eq!(targets, ["01J9Z8…tokens", "01J9Z8…redirects"]);
    assert_eq!(depends_on.line, 7);

    let kbx = note.kbx.as_ref().unwrap();
    assert_eq!(kbx["proposed"][0]["rel"], "see_also");
    assert_eq!(kbx["proposed"][0]["score"], 0.81);
    assert_eq!(kbx["reviewed"], "2026-11-03");
    let qmd = note.qmd.as_ref().unwrap();
    assert_eq!(qmd["metadata"]["type"], "Procedure");

    let paths: Vec<_> = note
        .sections
        .iter()
        .map(|s| s.heading_path.clone())
        .collect();
    assert_eq!(
        paths,
        [
            vec![],
            vec!["Flow".to_string()],
            vec!["Flow".to_string(), "Exchange".to_string()]
        ]
    );
    assert_eq!(note.sections[1].blocks[0].id, "redirect-step");
    assert_eq!(note.sections[2].slug_path, "flow/exchange");
    assert_eq!(note.sections[2].mentions[0].target, "01J9Z8…tokens");
    assert_eq!(note.mentions().count(), 1);
}

#[test]
fn section_offsets_index_the_whole_file() {
    let note = parse_note("a.md", EXAMPLE).unwrap();
    let flow = &note.sections[1];
    assert!(EXAMPLE[flow.byte_range.clone()].starts_with("# Flow\n"));
    assert!(EXAMPLE[flow.byte_range.clone()].ends_with("^redirect-step\n\n"));
    assert_eq!(flow.start_line, 18);
    assert_eq!(flow.end_line, 21);
    let block = &flow.blocks[0];
    assert_eq!(
        &EXAMPLE[block.byte_range.clone()],
        "The client redirects the user. ^redirect-step\n"
    );
    assert_eq!(block.start_line, 20);
}

#[test]
fn splitter_reports_yaml_and_body_spans() {
    let src = "---\na: 1\n---\nbody\n";
    let fm = split_frontmatter(src).unwrap().unwrap();
    assert_eq!(fm.yaml, "a: 1\n");
    assert_eq!(fm.body_start, 13);
    assert_eq!(&src[fm.body_start..], "body\n");
}

#[test]
fn splitter_handles_crlf_bom_and_trailing_spaces() {
    let fm = split_frontmatter("\u{feff}--- \r\na: 1\r\n---  \r\nbody\r\n")
        .unwrap()
        .unwrap();
    assert_eq!(fm.yaml, "a: 1\r\n");
}

#[test]
fn splitter_accepts_a_file_ending_at_the_closing_fence() {
    let src = "---\na: 1\n---";
    assert_eq!(
        split_frontmatter(src).unwrap().unwrap().body_start,
        src.len()
    );
}

#[test]
fn splitter_needs_the_fence_on_the_first_line() {
    assert!(split_frontmatter("\n---\na: 1\n---\n").unwrap().is_none());
    assert!(split_frontmatter("---- \na: 1\n---\n").unwrap().is_none());
    assert!(split_frontmatter("# Heading\n").unwrap().is_none());
    assert!(split_frontmatter("").unwrap().is_none());
}

#[test]
fn splitter_ignores_lines_that_only_start_with_the_fence() {
    let fm = split_frontmatter("---\na: |\n  ---\nb: ---x\n---\n")
        .unwrap()
        .unwrap();
    assert_eq!(fm.yaml, "a: |\n  ---\nb: ---x\n");
}

#[test]
fn unterminated_frontmatter_is_an_error() {
    assert!(matches!(
        parse_note("a.md", "---\nid: A\n"),
        Err(ParseError::UnterminatedFrontmatter)
    ));
}

#[test]
fn a_file_without_frontmatter_is_reported_as_such() {
    assert!(matches!(
        parse_note("a.md", "# Just notes\n"),
        Err(ParseError::NoFrontmatter)
    ));
}

#[test]
fn missing_required_keys_are_errors() {
    for (src, want) in [
        ("---\ntype: C\ntitle: T\n---\n", "id"),
        ("---\nid: A\ntitle: T\n---\n", "type"),
        ("---\nid: A\ntype: C\n---\n", "title"),
        ("---\nid: A\ntype: C\ntitle:\n---\n", "title"),
        ("---\n---\n", "id"),
    ] {
        match parse_note("a.md", src) {
            Err(ParseError::MissingKey(key)) => assert_eq!(key, want),
            other => panic!("{src:?}: {other:?}"),
        }
    }
}

#[test]
fn invalid_yaml_is_an_error() {
    let err = parse_note("a.md", "---\nid: [\n---\n").unwrap_err();
    assert!(matches!(err, ParseError::Yaml(_)));
    let err = parse_note("a.md", "---\n- a\n- b\n---\n").unwrap_err();
    assert!(matches!(err, ParseError::NotAMapping));
    let err = parse_note("a.md", "---\nid: A\nid: B\ntype: C\ntitle: T\n---\n").unwrap_err();
    assert!(matches!(err, ParseError::Yaml(_)));
}

#[test]
fn nested_maps_are_rejected_outside_the_tool_namespaces() {
    for extra in [
        "author:\n  name: X\n",
        "people: [{name: X}]\n",
        "m: [[{a: 1}]]\n",
    ] {
        match parse_note("a.md", &minimal(extra)) {
            Err(ParseError::NestedMap(key)) => assert_eq!(key, extra.split(':').next().unwrap()),
            other => panic!("{extra:?}: {other:?}"),
        }
    }
    let note = parse_note(
        "a.md",
        &minimal("kbx: {a: {b: 1}}\nqmd: {metadata: {c: 2}}\n"),
    )
    .unwrap();
    assert_eq!(note.kbx.unwrap()["a"]["b"], 1);
}

#[test]
fn scalar_aliases_and_tags_become_lists() {
    let note = parse_note("a.md", &minimal("aliases: one\ntags: [2024, x]\n")).unwrap();
    assert_eq!(note.aliases, ["one"]);
    assert_eq!(note.tags, ["2024", "x"]);
}

#[test]
fn numeric_titles_are_kept_as_text() {
    let note = parse_note("a.md", "---\nid: A\ntype: C\ntitle: 1984\n---\n").unwrap();
    assert_eq!(note.title, "1984");
}

#[test]
fn a_single_quoted_wikilink_is_a_one_item_relation() {
    let note = parse_note("a.md", &minimal("part_of: \"[[B]]\"\n")).unwrap();
    assert_eq!(note.relation("part_of").unwrap().links[0].target, "B");
}

#[test]
fn keys_without_wikilinks_are_kept_as_properties() {
    let src = minimal("author: X\ncssclasses: []\nmixed: [\"[[B]]\", plain]\n");
    let note = parse_note("a.md", &src).unwrap();
    assert!(note.relations.is_empty());
    let keys: Vec<_> = note.properties.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["author", "cssclasses", "mixed"]);
    assert_eq!(note.properties[0].line, 5);
}

#[test]
fn wikilink_parts_are_split() {
    let src =
        minimal("see_also: [\"[[target#Some heading|shown]]\", \"[[b#^blk]]\", \"[[c.md]]\"]\n");
    let note = parse_note("a.md", &src).unwrap();
    let links = &note.relation("see_also").unwrap().links;
    assert_eq!(links[0].target, "target");
    assert_eq!(links[0].fragment.as_deref(), Some("Some heading"));
    assert_eq!(links[0].display.as_deref(), Some("shown"));
    assert_eq!(links[1].fragment.as_deref(), Some("^blk"));
    assert_eq!(links[2].target, "c.md");
}

#[test]
fn inline_links_skip_code_same_note_anchors_and_attachments() {
    let body = "[[real]] `[[code]]` [[#local]] ![[pic.png]] ![[embedded note]] [[t|shown]]\n\n\
                ```\n[[fenced]]\n```\n\n| a |\n|---|\n| [[cell\\|x]] |\n";
    let src = minimal("").replace("Body.\n", body);
    let note = parse_note("a.md", &src).unwrap();
    let targets: Vec<_> = note.mentions().map(|l| l.target.as_str()).collect();
    assert_eq!(targets, ["real", "embedded note", "t", "cell"]);
    assert_eq!(note.mentions().next().unwrap().line, 6);
}

#[test]
fn a_crlf_file_parses_like_its_lf_twin() {
    let lf = parse_note("a.md", EXAMPLE).unwrap();
    let crlf = parse_note("a.md", &EXAMPLE.replace('\n', "\r\n")).unwrap();
    assert_eq!(crlf.id, lf.id);
    assert_eq!(crlf.relations, lf.relations);
    assert_eq!(crlf.class_line, lf.class_line);
    let summary = |note: &kbx_core::Note| -> Vec<_> {
        let blocks = |s: &kbx_core::Section| s.blocks.iter().map(|b| b.id.clone()).collect();
        let sections = note.sections.iter();
        sections
            .map(|s| (s.key, s.start_line, s.end_line, blocks(s)))
            .collect::<Vec<(_, _, _, Vec<_>)>>()
    };
    assert_eq!(summary(&crlf), summary(&lf));
}

#[test]
fn structured_values_are_refused_where_text_is_needed() {
    let err = parse_note("a.md", "---\nid: A\ntype: [B, C]\ntitle: T\n---\n").unwrap_err();
    assert!(matches!(err, ParseError::NotAScalar("type")));
}

#[test]
fn non_string_keys_are_errors() {
    let err = parse_note("a.md", &minimal("[a, b]: x\n")).unwrap_err();
    assert!(matches!(err, ParseError::NonStringKey(_)));
}

#[test]
fn a_tag_does_not_hide_a_nested_map() {
    let err = parse_note("a.md", &minimal("x: !tag {a: 1}\n")).unwrap_err();
    assert!(matches!(err, ParseError::NestedMap(_)));
}

#[test]
fn only_known_attachment_types_are_dropped_from_embeds() {
    let body = "![[OAuth 2.0]] ![[v1.2 notes.md]] ![[a.PNG]] ![[b.pdf]] ![[c.mp4]] ![[d.canvas]]\n";
    let src = minimal("").replace("Body.\n", body);
    let note = parse_note("a.md", &src).unwrap();
    let targets: Vec<_> = note.mentions().map(|l| l.target.as_str()).collect();
    assert_eq!(targets, ["OAuth 2.0", "v1.2 notes.md"]);
}
