use kbx_core::{
    Finding, FindingCode, Link, Note, Ontology, ResolvedVia, Severity, Vault, parse_note,
};

const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

fn ontology() -> Ontology {
    Ontology::parse(ONTOLOGY).unwrap()
}

/// A note at `{name}.md` whose id is [`ulid`]`(name)`.
fn note(name: &str, class: &str, frontmatter: &str, body: &str) -> Note {
    let src = format!(
        "---\nid: {}\ntype: {class}\ntitle: {name}\n{frontmatter}---\n{body}",
        ulid(name)
    );
    parse_note(format!("{name}.md"), &src).unwrap()
}

/// Ids are opaque in the validator, so a readable id stands in for a ULID.
fn ulid(name: &str) -> String {
    format!("ID{name}")
}

fn link(name: &str) -> String {
    format!("\"[[{}]]\"", ulid(name))
}

fn findings(notes: Vec<Note>) -> Vec<Finding> {
    Vault::new(notes).validate(&ontology())
}

fn codes(notes: Vec<Note>) -> Vec<&'static str> {
    findings(notes).iter().map(|f| f.code.as_str()).collect()
}

#[test]
fn only_link_unstable_is_a_warning() {
    assert_eq!(FindingCode::LinkUnstable.severity(), Severity::Warning);
    for code in [
        FindingCode::TypeUnknown,
        FindingCode::RelUnknown,
        FindingCode::RelRange,
        FindingCode::RelDomain,
        FindingCode::SchemaGap,
        FindingCode::RelCardinality,
        FindingCode::LinkUnresolved,
        FindingCode::NoteInvalid,
        FindingCode::IdDuplicate,
    ] {
        assert_eq!(code.severity(), Severity::Error, "{code}");
    }
    assert_eq!(Severity::Warning.as_str(), "warning");
    assert_eq!(Severity::Error.to_string(), "error");
}

#[test]
fn a_clean_vault_has_no_findings() {
    let notes = vec![
        note("tokens", "Concept", "", ""),
        note(
            "flow",
            "Procedure",
            &format!(
                "depends_on: [{}]\nauthor: X\ncssclasses: []\n",
                link("tokens")
            ),
            &format!("See [[{}]].\n", ulid("tokens")),
        ),
    ];
    assert_eq!(codes(notes), [""; 0]);
}

#[test]
fn type_unknown() {
    let found = findings(vec![note("a", "Recipe", "", "")]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::TypeUnknown);
    assert_eq!(found[0].code.as_str(), "type.unknown");
    assert_eq!(found[0].path.to_str(), Some("a.md"));
    assert_eq!(found[0].line, 3);
    assert!(found[0].message.contains("Recipe"));
}

#[test]
fn rel_unknown() {
    let notes = vec![
        note(
            "a",
            "Concept",
            &format!("author: X\nrelates_to: [{}]\n", link("b")),
            "",
        ),
        note("b", "Concept", "", ""),
    ];
    let found = findings(notes);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::RelUnknown);
    assert_eq!(found[0].code.as_str(), "rel.unknown");
    assert_eq!(found[0].line, 6);
    assert!(found[0].message.contains("relates_to"));
}

#[test]
fn rel_unknown_for_the_judge_only_none_label() {
    let notes = vec![
        note("a", "Concept", &format!("none: [{}]\n", link("b")), ""),
        note("b", "Concept", "", ""),
    ];
    assert_eq!(codes(notes), ["rel.unknown"]);
}

#[test]
fn rel_unknown_when_a_relation_key_holds_something_other_than_wikilinks() {
    for value in [
        "bare text",
        "[bare, text]",
        "[[unquoted]]",
        "[\"[[b]]\", bare]",
    ] {
        let frontmatter = format!("see_also: {value}\n");
        let found = findings(vec![note("a", "Concept", &frontmatter, "")]);
        assert_eq!(found.len(), 1, "{value}");
        assert_eq!(found[0].code, FindingCode::RelUnknown, "{value}");
        assert_eq!(found[0].line, 5);
    }
}

#[test]
fn rel_unknown_when_a_relation_key_holds_a_nested_map() {
    let found = findings(vec![note("a", "Concept", "see_also:\n  a: 1\n", "")]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::RelUnknown);
    assert!(found[0].message.contains("see_also"));
}

#[test]
fn an_empty_relation_key_is_not_a_finding_by_itself() {
    assert_eq!(
        codes(vec![note("a", "Concept", "see_also: []\npart_of:\n", "")]),
        [""; 0]
    );
}

#[test]
fn rel_range() {
    let notes = vec![
        note(
            "a",
            "Procedure",
            &format!("depends_on: [{0}]\n", link("b")),
            "",
        ),
        note("b", "Decision", "", ""),
    ];
    let found = findings(notes);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::RelRange);
    assert_eq!(found[0].code.as_str(), "rel.range");
    assert_eq!(found[0].line, 5);
    assert!(found[0].message.contains("Decision"));
}

#[test]
fn rel_domain() {
    let notes = vec![
        note("tokens", "Concept", "", ""),
        note(
            "idea",
            "Concept",
            &format!("depends_on: [{}]\n", link("tokens")),
            "",
        ),
    ];
    let found = findings(notes);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, FindingCode::RelDomain);
    assert_eq!(found[0].code.as_str(), "rel.domain");
    assert_eq!(found[0].path.to_str(), Some("idea.md"));
    assert_eq!(found[0].line, 5);
    assert!(found[0].message.contains("depends_on"));
}

#[test]
fn rel_domain_any_admits_every_class() {
    let notes = vec![
        note("tokens", "Concept", "", ""),
        note(
            "choice",
            "Decision",
            &format!("see_also: [{}]\n", link("tokens")),
            "",
        ),
    ];
    assert_eq!(codes(notes), Vec::<&str>::new());
}

#[test]
fn rel_domain_of_an_inverse_key_is_the_relation_range() {
    // `required_by` reads `depends_on` backwards, so it may sit on what
    // `depends_on` may target, which a Decision is not.
    let notes = vec![
        note(
            "flow",
            "Procedure",
            &format!("depends_on: [{}]\n", link("tokens")),
            "",
        ),
        note(
            "tokens",
            "Concept",
            &format!("required_by: [{}]\n", link("flow")),
            "",
        ),
        note(
            "choice",
            "Decision",
            &format!("required_by: [{}]\n", link("flow")),
            "",
        ),
    ];
    assert_eq!(codes(notes), ["rel.domain"]);
}

#[test]
fn a_source_of_unknown_class_is_reported_once_as_type_unknown() {
    let notes = vec![
        note("tokens", "Concept", "", ""),
        note(
            "odd",
            "Recipe",
            &format!("depends_on: [{}]\n", link("tokens")),
            "",
        ),
    ];
    assert_eq!(codes(notes), ["type.unknown"]);
}

#[test]
fn rel_range_any_admits_every_class() {
    let notes = vec![
        note("a", "Decision", &format!("see_also: [{}]\n", link("b")), ""),
        note("b", "Reference", "", ""),
    ];
    assert_eq!(codes(notes), [""; 0]);
}

/// §7 writes inverse keys into target notes, so they must validate, with domain and range swapped.
#[test]
fn inverse_keys_are_known_and_checked_against_the_swapped_range() {
    let good = vec![
        note(
            "a",
            "Concept",
            &format!("required_by: [{}]\n", link("b")),
            "",
        ),
        note(
            "b",
            "Procedure",
            &format!("depends_on: [{}]\n", link("a")),
            "",
        ),
    ];
    assert_eq!(codes(good), [""; 0]);
    let bad = vec![
        note(
            "a",
            "Concept",
            &format!("required_by: [{}]\n", link("b")),
            "",
        ),
        note("b", "Decision", "", ""),
    ];
    assert_eq!(codes(bad), ["rel.range"]);
}

#[test]
fn a_target_of_unknown_class_is_reported_once_as_type_unknown() {
    let notes = vec![
        note(
            "a",
            "Procedure",
            &format!("depends_on: [{}]\n", link("b")),
            "",
        ),
        note("b", "Recipe", "", ""),
    ];
    assert_eq!(codes(notes), ["type.unknown"]);
}

#[test]
fn schema_gap() {
    let found = findings(vec![note("a", "Procedure", "", "")]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::SchemaGap);
    assert_eq!(found[0].code.as_str(), "schema.gap");
    assert_eq!(found[0].line, 3);
    assert!(found[0].message.contains("depends_on"));
}

#[test]
fn schema_gap_when_the_required_key_is_empty() {
    assert_eq!(
        codes(vec![note("a", "Procedure", "depends_on: []\n", "")]),
        ["schema.gap"]
    );
}

#[test]
fn rel_cardinality() {
    let notes = vec![
        note(
            "a",
            "Decision",
            &format!("supersedes: [{}, {}]\n", link("b"), link("c")),
            "",
        ),
        note("b", "Decision", "", ""),
        note("c", "Decision", "", ""),
    ];
    let found = findings(notes);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::RelCardinality);
    assert_eq!(found[0].code.as_str(), "rel.cardinality");
    assert_eq!(found[0].line, 5);
    assert!(found[0].message.contains("supersedes"));
}

#[test]
fn rel_cardinality_counts_notes_not_spellings() {
    let notes = vec![
        note(
            "a",
            "Decision",
            &format!("supersedes: [{0}, \"[[b alias]]\"]\n", link("b")),
            "",
        ),
        note("b", "Decision", "aliases: [b alias]\n", ""),
    ];
    assert_eq!(codes(notes), [""; 0]);
}

#[test]
fn rel_cardinality_does_not_apply_to_the_inverse() {
    let notes = vec![
        note(
            "a",
            "Decision",
            &format!("superseded_by: [{}, {}]\n", link("b"), link("c")),
            "",
        ),
        note("b", "Decision", "", ""),
        note("c", "Decision", "", ""),
    ];
    assert_eq!(codes(notes), [""; 0]);
}

#[test]
fn link_unstable() {
    let notes = vec![
        note("a", "Concept", "see_also: [\"[[b]]\"]\n", ""),
        note("b", "Concept", "", ""),
    ];
    let found = findings(notes);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::LinkUnstable);
    assert_eq!(found[0].code.as_str(), "link.unstable");
    assert_eq!(found[0].line, 5);
}

#[test]
fn link_unstable_in_the_body() {
    let notes = vec![
        note("a", "Concept", "", "\nSee [[b]].\n"),
        note("b", "Concept", "", ""),
    ];
    let found = findings(notes);
    assert_eq!(found[0].code, FindingCode::LinkUnstable);
    assert_eq!(found[0].line, 7);
}

#[test]
fn link_unresolved() {
    let found = findings(vec![note(
        "a",
        "Concept",
        "see_also: [\"[[ghost]]\"]\n",
        "",
    )]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::LinkUnresolved);
    assert_eq!(found[0].code.as_str(), "link.unresolved");
    assert_eq!(found[0].line, 5);
    assert!(found[0].message.contains("ghost"));
}

#[test]
fn link_unresolved_in_the_body() {
    let found = findings(vec![note("a", "Concept", "", "See [[ghost]].\n")]);
    assert_eq!(found[0].code, FindingCode::LinkUnresolved);
    assert_eq!(found[0].line, 6);
}

#[test]
fn a_partial_path_to_several_notes_says_how_many() {
    let plan = |path, id| {
        parse_note(
            path,
            &format!("---\nid: {id}\ntype: Concept\ntitle: T\n---\n"),
        )
        .unwrap()
    };
    let linker = parse_note(
        "a.md",
        "---\nid: A\ntype: Concept\ntitle: T\n---\nSee [[shared/plan]] and [[ghost/plan]].\n",
    )
    .unwrap();
    let found = findings(vec![
        linker,
        plan("x/shared/plan.md", "X"),
        plan("y/shared/plan.md", "Y"),
    ]);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].code, FindingCode::LinkUnresolved);
    assert_eq!(
        found[0].message,
        "`shared/plan` does not resolve to exactly one note: 2 notes match"
    );
    assert_eq!(
        found[1].message,
        "`ghost/plan` does not resolve to exactly one note"
    );
}

#[test]
fn a_partial_path_is_unstable_only_to_a_note_with_its_own_id() {
    let linker = parse_note(
        "a.md",
        "---\nid: A\ntype: Concept\ntitle: T\n---\nSee [[deep/stated]] and [[deep/inferred]].\n",
    )
    .unwrap();
    let stated = parse_note(
        "x/deep/stated.md",
        "---\nid: S\ntype: Concept\ntitle: T\n---\n",
    )
    .unwrap();
    let inferred = parse_note("x/deep/inferred.md", "No keys.\n").unwrap();
    let vault = Vault::new(vec![linker, stated, inferred]);
    let found = vault.validate_note(&vault.notes()[0], &ontology());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::LinkUnstable);
    assert!(found[0].message.contains("deep/stated"));
}

/// §8: only the author's lint reports unresolved links, so callers can pick the notes they check.
#[test]
fn validate_note_checks_one_note_against_the_whole_vault() {
    let vault = Vault::new(vec![
        note("a", "Concept", "see_also: [\"[[ghost]]\"]\n", ""),
        note("b", "Recipe", "", ""),
    ]);
    let found = vault.validate_note(&vault.notes()[0], &ontology());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, FindingCode::LinkUnresolved);
}

#[test]
fn findings_are_ordered_by_note_then_line() {
    let notes = vec![
        note(
            "a",
            "Concept",
            "see_also: [\"[[ghost]]\"]\nnope: [\"[[ghost]]\"]\n",
            "[[ghost]]\n",
        ),
        note("b", "Recipe", "", ""),
    ];
    let lines: Vec<_> = findings(notes)
        .iter()
        .map(|f| (f.path.clone(), f.line))
        .collect();
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted);
}

fn vault(notes: Vec<Note>) -> Vault {
    Vault::new(notes)
}

fn resolve<'v>(vault: &'v Vault, text: &str) -> Option<(&'v str, ResolvedVia)> {
    let src = format!("---\nid: Z\ntype: C\ntitle: T\nx: \"[[{text}]]\"\n---\n");
    let parsed = parse_note("z.md", &src).unwrap();
    let link = &parsed.relation("x").unwrap().links[0];
    vault.resolve(link).map(|r| (r.note.id.as_str(), r.via))
}

#[test]
fn links_resolve_by_id_then_filename_then_alias() {
    let vault = vault(vec![
        parse_note(
            "dir/01ABC-tokens.md",
            "---\nid: 01ABC\ntype: Concept\ntitle: T\naliases: [Access Token]\n---\n",
        )
        .unwrap(),
        parse_note(
            "other.md",
            "---\nid: 01DEF\ntype: Concept\ntitle: U\naliases: [01ABC, 01abc-tokens]\n---\n",
        )
        .unwrap(),
    ]);
    assert_eq!(resolve(&vault, "01ABC"), Some(("01ABC", ResolvedVia::Id)));
    assert_eq!(
        resolve(&vault, "01ABC-tokens"),
        Some(("01ABC", ResolvedVia::Id))
    );
    assert_eq!(
        resolve(&vault, "01ABC-renamed"),
        Some(("01ABC", ResolvedVia::Id))
    );
    assert_eq!(
        resolve(&vault, "01ABC#Heading"),
        Some(("01ABC", ResolvedVia::Id))
    );
    assert_eq!(
        resolve(&vault, "other"),
        Some(("01DEF", ResolvedVia::Filename))
    );
    assert_eq!(
        resolve(&vault, "OTHER.md"),
        Some(("01DEF", ResolvedVia::Filename))
    );
    assert_eq!(
        resolve(&vault, "dir/01abc-tokens"),
        Some(("01ABC", ResolvedVia::Filename))
    );
    assert_eq!(
        resolve(&vault, "01abc-tokens"),
        Some(("01ABC", ResolvedVia::Filename))
    );
    assert_eq!(
        resolve(&vault, "access token"),
        Some(("01ABC", ResolvedVia::Alias))
    );
    assert_eq!(resolve(&vault, "nope"), None);
    assert_eq!(resolve(&vault, "wrong/other"), None);
}

#[test]
fn ambiguous_links_do_not_resolve() {
    let make = |path: &str, id: &str, extra: &str| {
        parse_note(
            path,
            &format!("---\nid: {id}\ntype: Concept\ntitle: T\n{extra}---\n"),
        )
        .unwrap()
    };
    let vault = vault(vec![
        make("a/same.md", "1", "aliases: [shared]\n"),
        make("b/same.md", "2", "aliases: [shared]\n"),
        make("c.md", "3", ""),
        make("d.md", "3", ""),
    ]);
    assert_eq!(resolve(&vault, "shared"), None);
    assert_eq!(resolve(&vault, "same"), None);
    assert_eq!(
        resolve(&vault, "a/same"),
        Some(("1", ResolvedVia::Filename))
    );
}

fn at(path: &str, id: &str, extra: &str) -> Note {
    parse_note(
        path,
        &format!("---\nid: {id}\ntype: Concept\ntitle: {path}\n{extra}---\n"),
    )
    .unwrap()
}

fn resolved_path(vault: &Vault, text: &str) -> Option<String> {
    let link = Link {
        target: text.to_string(),
        fragment: None,
        display: None,
        line: 1,
    };
    let resolved = vault.resolve(&link)?;
    Some(resolved.note.path.to_str().unwrap().to_string())
}

#[test]
fn the_lowest_path_keeps_a_shared_id_whatever_the_order_given() {
    let vault = vault(vec![at("b.md", "1", ""), at("a.md", "1", "")]);
    assert_eq!(resolved_path(&vault, "1"), Some("a.md".to_string()));
    assert_eq!(resolved_path(&vault, "1-slug"), Some("a.md".to_string()));
}

#[test]
fn lowest_means_the_string_order_the_index_uses() {
    // Path comparison goes by component and would put `a/` first, but `-`
    // sorts before `/`.
    let vault = vault(vec![at("a/x.md", "1", ""), at("a-b/x.md", "1", "")]);
    assert_eq!(resolved_path(&vault, "1"), Some("a-b/x.md".to_string()));
}

#[test]
fn a_note_that_lost_its_id_answers_to_nothing() {
    let vault = vault(vec![
        at("a.md", "1", ""),
        at("b.md", "1", "aliases: [lost, shared]\n"),
        at("c.md", "2", "aliases: [shared]\n"),
    ]);
    assert_eq!(resolved_path(&vault, "lost"), None);
    assert_eq!(resolved_path(&vault, "b"), None);
    // The index has no row for b.md, so the alias isn't ambiguous there.
    assert_eq!(resolved_path(&vault, "shared"), Some("c.md".to_string()));
}

#[test]
fn id_duplicate_is_reported_on_every_file_at_the_id_line() {
    let vault = vault(vec![
        at("a.md", "1", ""),
        at("b.md", "1", ""),
        at("c.md", "1", ""),
        at("d.md", "2", ""),
    ]);
    let found = vault.validate(&ontology());
    let places: Vec<_> = found
        .iter()
        .map(|f| (f.code.as_str(), f.path.to_str().unwrap(), f.line))
        .collect();
    assert_eq!(
        places,
        [
            ("id.duplicate", "a.md", 2),
            ("id.duplicate", "b.md", 2),
            ("id.duplicate", "c.md", 2),
        ]
    );
    assert!(found[0].message.contains("b.md") && found[0].message.contains("c.md"));
    assert!(found[1].message.contains("a.md"));
}

#[test]
fn a_note_aliasing_itself_twice_is_not_ambiguous() {
    let vault = vault(vec![
        parse_note(
            "a.md",
            "---\nid: 1\ntype: Concept\ntitle: T\naliases: [x, X]\n---\n",
        )
        .unwrap(),
    ]);
    assert_eq!(resolve(&vault, "x"), Some(("1", ResolvedVia::Alias)));
    assert_eq!(resolve(&vault, "a"), Some(("1", ResolvedVia::Filename)));
}

#[test]
fn the_id_prefix_needs_the_hyphen_form() {
    let make = |path: &str, id: &str| {
        parse_note(
            path,
            &format!("---\nid: {id}\ntype: Concept\ntitle: T\n---\n"),
        )
        .unwrap()
    };
    let vault = vault(vec![
        make("01ABC-tokens.md", "01ABC"),
        make("01ABC.backup.md", "01DEF"),
        make("01ABC/other.md", "01GHI"),
    ]);
    assert_eq!(
        resolve(&vault, "01ABC-tokens"),
        Some(("01ABC", ResolvedVia::Id))
    );
    assert_eq!(
        resolve(&vault, "01ABC.backup"),
        Some(("01DEF", ResolvedVia::Filename))
    );
    assert_eq!(
        resolve(&vault, "01ABC/other"),
        Some(("01GHI", ResolvedVia::Filename))
    );
    assert_eq!(resolve(&vault, "01ABC other"), None);
}
