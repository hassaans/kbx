use std::path::Path;

use kbx_core::{ResolvedVia, Resolver, parse_note};

fn resolver() -> Resolver {
    let mut resolver = Resolver::default();
    resolver.add(0, "01A", Path::new("a/01A-first.md"), ["Shared", "one"]);
    resolver.add(1, "01B", Path::new("b/second.md"), ["shared"]);
    resolver
}

#[test]
fn resolves_without_a_parsed_note() {
    let resolver = resolver();
    assert_eq!(resolver.resolve("01A"), Some((0, ResolvedVia::Id)));
    assert_eq!(resolver.resolve("01A-renamed"), Some((0, ResolvedVia::Id)));
    assert_eq!(resolver.resolve("SECOND"), Some((1, ResolvedVia::Filename)));
    assert_eq!(
        resolver.resolve("b/second.md"),
        Some((1, ResolvedVia::Filename))
    );
    assert_eq!(resolver.resolve("ONE"), Some((0, ResolvedVia::Alias)));
}

#[test]
fn an_ambiguous_or_unknown_target_does_not_resolve() {
    let resolver = resolver();
    assert_eq!(resolver.resolve("shared"), None);
    assert_eq!(resolver.resolve("missing"), None);
}

#[test]
fn a_section_knows_the_block_id_on_its_heading() {
    let src = "---\nid: 01A\ntype: Concept\ntitle: T\n---\nPreamble. ^intro\n\n# Head ^head\n\nBody. ^body\n\n# Plain\n\nOther. ^other\n";
    let note = parse_note("a.md", src).unwrap();
    let own: Vec<Option<&str>> = note
        .sections
        .iter()
        .map(|section| section.heading_block().map(|block| block.id.as_str()))
        .collect();
    assert_eq!(own, [None, Some("head"), None]);
}
