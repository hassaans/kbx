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

fn nested() -> Resolver {
    let mut resolver = Resolver::default();
    let aliases: [&str; 0] = [];
    let paths = [
        "companies/acme/people/ann.md",
        "companies/acme/people/Bob.MD",
        "companies/acme/notes/plan.md",
        "companies/zeta/notes/plan.md",
        "notes/plan.md",
        "companies/acme/my-notes/todo.md",
    ];
    for (index, path) in paths.into_iter().enumerate() {
        resolver.add(index, &format!("id{index}"), Path::new(path), aliases);
    }
    resolver
}

#[test]
fn a_partial_path_resolves_when_it_ends_one_note_path() {
    let resolver = nested();
    let found = Some((0, ResolvedVia::Filename));
    assert_eq!(resolver.resolve("people/ann"), found);
    assert_eq!(resolver.resolve("acme/people/ann"), found);
    assert_eq!(resolver.resolve("people/ann.md"), found);
    assert_eq!(resolver.resolve("people\\ann"), found);
    assert_eq!(
        resolver.resolve("people/Bob"),
        Some((1, ResolvedVia::Filename))
    );
    assert_eq!(resolver.match_count("people/ann"), 1);
}

#[test]
fn a_partial_path_matches_only_at_a_folder_boundary() {
    let resolver = nested();
    assert_eq!(resolver.resolve("notes/todo"), None);
    assert_eq!(resolver.resolve("ople/ann"), None);
    assert_eq!(resolver.resolve("/people/ann"), None);
    assert_eq!(resolver.resolve("../people/ann"), None);
    assert_eq!(resolver.resolve("people//ann"), None);
    assert_eq!(resolver.resolve("people/ann/"), None);
    assert_eq!(resolver.resolve("acme/people"), None);
    assert_eq!(resolver.match_count("notes/todo"), 0);
}

#[test]
fn a_partial_path_keeps_its_case() {
    let resolver = nested();
    assert_eq!(resolver.resolve("People/ann"), None);
    assert_eq!(resolver.resolve("people/bob"), None);
}

#[test]
fn a_partial_path_that_ends_several_note_paths_does_not_resolve() {
    let resolver = nested();
    assert_eq!(
        resolver.resolve("companies/acme/notes/plan"),
        Some((2, ResolvedVia::Filename))
    );
    assert_eq!(
        resolver.resolve("acme/notes/plan"),
        Some((2, ResolvedVia::Filename))
    );
    assert_eq!(
        resolver.resolve("zeta/notes/plan"),
        Some((3, ResolvedVia::Filename))
    );

    let mut resolver = Resolver::default();
    let aliases: [&str; 0] = [];
    resolver.add(0, "A", Path::new("x/shared/plan.md"), aliases);
    resolver.add(1, "B", Path::new("y/shared/plan.md"), aliases);
    resolver.add(2, "C", Path::new("z/shared/plan.md"), aliases);
    assert_eq!(resolver.resolve("shared/plan"), None);
    assert_eq!(resolver.match_count("shared/plan"), 3);
}

#[test]
fn a_full_path_beats_a_longer_path_that_ends_with_it() {
    let resolver = nested();
    assert_eq!(
        resolver.resolve("notes/plan"),
        Some((4, ResolvedVia::Filename))
    );
}

#[test]
fn a_partial_path_comes_after_every_other_step() {
    let mut resolver = Resolver::default();
    let none: [&str; 0] = [];
    resolver.add(0, "01A", Path::new("deep/people/ann.md"), none);
    resolver.add(1, "people/ann", Path::new("by-id.md"), none);
    assert_eq!(resolver.resolve("people/ann"), Some((1, ResolvedVia::Id)));

    let mut resolver = Resolver::default();
    resolver.add(0, "01A", Path::new("deep/people/ann.md"), none);
    resolver.add(1, "01B", Path::new("by-alias.md"), ["people/ann"]);
    assert_eq!(
        resolver.resolve("people/ann"),
        Some((1, ResolvedVia::Alias))
    );

    // Two notes differ in case alone: the path step is ambiguous, and decides.
    let mut resolver = Resolver::default();
    resolver.add(0, "01A", Path::new("a/b.md"), none);
    resolver.add(1, "01B", Path::new("A/b.md"), none);
    resolver.add(2, "01C", Path::new("deep/a/b.md"), none);
    assert_eq!(resolver.resolve("a/b"), None);
    assert_eq!(resolver.match_count("a/b"), 2);
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
