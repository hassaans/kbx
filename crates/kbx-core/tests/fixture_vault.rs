//! Holds `tests/fixtures/vault/` to what its README promises.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use kbx_core::{Note, Ontology, ParseError, Vault, parse_note};

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
const MALFORMED: [&str; 2] = [
    "lint/01KBX000000000000000000025-bad-yaml.md",
    "lint/01KBX000000000000000000026-unterminated.md",
];

fn markdown_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            markdown_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "md") {
            found.push(path);
        }
    }
}

/// Parses the vault, returning the notes and the files that aren't notes.
/// Malformed files are left out: [`malformed`] covers them.
fn load() -> (Vault, Vec<String>) {
    let mut files = Vec::new();
    markdown_files(Path::new(VAULT), &mut files);
    files.sort();
    let mut notes = Vec::new();
    let mut plain = Vec::new();
    for file in files {
        let relative = file.strip_prefix(VAULT).unwrap().to_path_buf();
        match parse_note(relative.clone(), &fs::read_to_string(&file).unwrap()) {
            Ok(note) => notes.push(note),
            Err(ParseError::NoFrontmatter) => plain.push(relative.display().to_string()),
            Err(_) => assert!(MALFORMED.contains(&relative.to_str().unwrap())),
        }
    }
    (Vault::new(notes), plain)
}

fn slug(note: &Note) -> &str {
    let stem = note.path.file_stem().unwrap().to_str().unwrap();
    stem.strip_prefix(&format!("{}-", note.id)).unwrap_or(stem)
}

fn find<'v>(vault: &'v Vault, wanted: &str) -> &'v Note {
    vault
        .notes()
        .iter()
        .find(|note| slug(note) == wanted)
        .unwrap()
}

fn scope(note: &Note) -> String {
    note.path
        .components()
        .next()
        .unwrap()
        .as_os_str()
        .to_string_lossy()
        .into_owned()
}

/// Undirected adjacency over typed and inline links, by note id.
fn adjacency(vault: &Vault) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut neighbours: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for note in vault.notes() {
        let typed = note.relations.iter().flat_map(|entry| &entry.links);
        for link in typed.chain(note.mentions()) {
            if let Some(target) = vault.resolve(link) {
                neighbours
                    .entry(&note.id)
                    .or_default()
                    .insert(&target.note.id);
                neighbours
                    .entry(&target.note.id)
                    .or_default()
                    .insert(&note.id);
            }
        }
    }
    neighbours
}

fn linked(vault: &Vault, a: &str, b: &str) -> bool {
    let (a, b) = (find(vault, a), find(vault, b));
    adjacency(vault)
        .get(a.id.as_str())
        .is_some_and(|n| n.contains(b.id.as_str()))
}

#[test]
fn every_file_but_the_readme_is_a_note() {
    let (vault, plain) = load();
    assert_eq!(plain, ["README.md"]);
    assert_eq!(vault.notes().len(), 26);
}

#[test]
fn the_malformed_files_fail_for_the_reasons_the_readme_gives() {
    let parse = |path: &str| {
        let src = fs::read_to_string(Path::new(VAULT).join(path)).unwrap();
        parse_note(path, &src).unwrap_err()
    };
    assert!(matches!(parse(MALFORMED[0]), ParseError::Yaml(_)));
    assert!(matches!(
        parse(MALFORMED[1]),
        ParseError::UnterminatedFrontmatter
    ));
}

#[test]
fn ids_are_ulids_and_prefix_their_filenames() {
    let (vault, _) = load();
    let crockford = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut ids = BTreeMap::new();
    for note in vault.notes() {
        assert_eq!(note.id.len(), 26, "{}", note.id);
        assert!(
            note.id.chars().all(|c| crockford.contains(c)),
            "{}",
            note.id
        );
        *ids.entry(&note.id).or_insert(0) += 1;
        if slug(note) != "legacy-runbook" {
            let stem = note.path.file_stem().unwrap().to_str().unwrap();
            assert!(stem.starts_with(&note.id), "{stem}");
        }
    }
    let shared: Vec<_> = ids.iter().filter(|(_, count)| **count > 1).collect();
    assert_eq!(shared.len(), 1, "only the id.duplicate pair: {shared:?}");
}

#[test]
fn each_finding_code_comes_from_the_note_the_readme_names() {
    let (vault, _) = load();
    let ontology = Ontology::parse(ONTOLOGY).unwrap();
    let mut found: Vec<(String, &str)> = vault
        .validate(&ontology)
        .iter()
        .map(|finding| {
            let note = vault
                .notes()
                .iter()
                .find(|n| n.path == finding.path)
                .unwrap();
            (slug(note).to_string(), finding.code.as_str())
        })
        .collect();
    found.sort();
    let expected = [
        ("id-duplicate-a", "id.duplicate"),
        ("id-duplicate-b", "id.duplicate"),
        ("link-unresolved", "link.unresolved"),
        ("link-unresolved", "link.unresolved"),
        ("link-unstable", "link.unstable"),
        ("rel-cardinality", "rel.cardinality"),
        ("rel-domain", "rel.domain"),
        ("rel-range", "rel.range"),
        ("rel-unknown", "rel.unknown"),
        ("restart-service", "schema.gap"),
        ("type-unknown", "type.unknown"),
    ]
    .map(|(note, code)| (note.to_string(), code));
    assert_eq!(found, expected);
}

#[test]
fn common_neighbors_and_two_hop_bridge_pair() {
    let (vault, _) = load();
    assert!(!linked(&vault, "deploy-service", "rotate-keys"));
    let neighbours = adjacency(&vault);
    let a = &neighbours[find(&vault, "deploy-service").id.as_str()];
    let b = &neighbours[find(&vault, "rotate-keys").id.as_str()];
    let shared: BTreeSet<_> = a.intersection(b).copied().collect();
    let expected = [find(&vault, "rate-limits"), find(&vault, "retries")];
    assert_eq!(shared, expected.iter().map(|n| n.id.as_str()).collect());
}

#[test]
fn schema_gap_note_has_candidates_in_its_scope() {
    let (vault, _) = load();
    let ontology = Ontology::parse(ONTOLOGY).unwrap();
    let gap = find(&vault, "restart-service");
    assert_eq!(gap.class, "Procedure");
    assert!(gap.relation("depends_on").is_none());
    let range = ontology.lookup("depends_on").unwrap().range();
    let candidates = vault
        .notes()
        .iter()
        .filter(|n| n.id != gap.id && scope(n) == scope(gap) && range.contains(&n.class));
    assert!(candidates.count() >= 3);
}

#[test]
fn alias_cooccur_note_mentions_an_alias_without_linking() {
    let (vault, _) = load();
    let source =
        fs::read_to_string(Path::new(VAULT).join(&find(&vault, "incident-response").path)).unwrap();
    let target = find(&vault, "rate-limits");
    assert!(
        target
            .aliases
            .iter()
            .any(|alias| source.contains(alias.as_str()))
    );
    assert!(!linked(&vault, "incident-response", "rate-limits"));
}

#[test]
fn qmd_knn_pair_shares_vocabulary_without_links_or_aliases() {
    let (vault, _) = load();
    assert!(!linked(&vault, "session-expiry", "cookie-lifetime"));
    let text =
        |slug: &str| fs::read_to_string(Path::new(VAULT).join(&find(&vault, slug).path)).unwrap();
    for shared in ["session", "cookie", "idle timeout", "browser"] {
        assert!(text("session-expiry").contains(shared));
        assert!(text("cookie-lifetime").contains(shared));
    }
    assert!(find(&vault, "session-expiry").aliases.is_empty());
    assert!(find(&vault, "cookie-lifetime").aliases.is_empty());
}

#[test]
fn hub_outranks_every_other_node() {
    let (vault, _) = load();
    let neighbours = adjacency(&vault);
    let hub = find(&vault, "platform");
    let degree = neighbours[hub.id.as_str()].len();
    assert!(degree >= 12, "{degree}");
    let runner_up = neighbours
        .iter()
        .filter(|(id, _)| **id != hub.id)
        .map(|(_, n)| n.len());
    assert!(degree >= 2 * runner_up.max().unwrap());

    let scopes: BTreeSet<_> = vault
        .notes()
        .iter()
        .filter(|n| neighbours[hub.id.as_str()].contains(n.id.as_str()))
        .map(scope)
        .collect();
    assert!(scopes.len() >= 3, "{scopes:?}");
}

#[test]
fn class_pairs_vary_for_type_pair_prior() {
    let (vault, _) = load();
    let pairs: BTreeSet<_> = vault
        .notes()
        .iter()
        .flat_map(|note| {
            note.relations
                .iter()
                .flat_map(|entry| &entry.links)
                .map(move |l| (note, l))
        })
        .filter_map(|(note, link)| Some((&note.class, &vault.resolve(link)?.note.class)))
        .collect();
    assert!(pairs.len() >= 6, "{pairs:?}");
}

#[test]
fn the_full_example_note_exercises_parsing() {
    let (vault, _) = load();
    let note = find(&vault, "oauth-flow");
    assert!(note.kbx.is_some() && note.qmd.is_some());

    let paths: Vec<_> = note.sections.iter().map(|s| s.slug_path.as_str()).collect();
    assert_eq!(
        paths,
        ["", "flow", "flow/steps", "flow/steps", "failure-modes"]
    );
    assert_eq!(note.sections[3].ordinal, 1);

    let blocks: Vec<_> = note
        .sections
        .iter()
        .flat_map(|s| &s.blocks)
        .map(|b| b.id.as_str())
        .collect();
    assert_eq!(blocks, ["send-user", "register", "swap-table"]);
    assert_eq!(note.mentions().count(), 1);
}

#[test]
fn inverse_keys_and_alias_links_are_present() {
    let (vault, _) = load();
    assert!(find(&vault, "tokens").relation("required_by").is_some());
    assert!(find(&vault, "auth-overview").relation("has_part").is_some());
    let link = &find(&vault, "retries").relation("see_also").unwrap().links[0];
    assert_eq!(
        vault.resolve(link).unwrap().via,
        kbx_core::ResolvedVia::Alias
    );
}
