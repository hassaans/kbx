//! Adopting a vault that was never written for kbx: `tests/fixtures/existing-vault`
//! goes through index, suggest, a fake judge and accept, with every note compared
//! byte for byte. The fake judge is `sh`, so this file is Unix only.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{FakePath, Fixture, copy_dir, stderr};
use kbx_core::inferred_id;
use rusqlite::Connection;
use serde_json::{Value, json};

const EXISTING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/existing-vault"
);
const NOTES: usize = 7;
const PINNED: &str = "01KBX000000000000000000100";
const GENERATORS: [&str; 2] = ["--generators", "alias_cooccur"];

fn existing() -> Fixture {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(EXISTING), vault.root());
    vault.ok(&["init"]);
    vault
}

fn indexed() -> Fixture {
    let vault = existing().writing();
    vault.ok(&["index"]);
    vault
}

fn database(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

/// `path -> (id, type, title)` from the index.
fn files(vault: &Fixture) -> BTreeMap<String, (String, String, String)> {
    let db = database(vault);
    let mut statement = db
        .prepare("SELECT path, id, type, title FROM files")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// Every markdown file in the vault, by path.
fn snapshot(vault: &Fixture) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    for folder in ["", "Projects", "Areas"] {
        for entry in fs::read_dir(vault.root().join(folder)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "md") {
                let name = path.strip_prefix(vault.root()).unwrap();
                let name = name.to_str().unwrap().to_string();
                found.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    found
}

fn cids(vault: &Fixture) -> Vec<(String, String, String)> {
    let db = database(vault);
    let mut statement = db
        .prepare("SELECT cid, src, dst FROM candidates ORDER BY cid")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// The candidate joining two notes, by path.
fn cid_of(vault: &Fixture, a: &str, b: &str) -> String {
    let (a, b) = (inferred_id(a), inferred_id(b));
    let found = cids(vault)
        .into_iter()
        .find(|(_, src, dst)| (*src == a && *dst == b) || (*src == b && *dst == a));
    found
        .unwrap_or_else(|| panic!("no candidate for {a} and {b}"))
        .0
}

/// A `claude` that accepts every pair of the batch as `see_also`.
fn fake_judge(vault: &Fixture) -> FakePath {
    let path = FakePath::with_qmd();
    let verdicts: Vec<Value> = cids(vault)
        .iter()
        .map(|(cid, _, _)| {
            json!({"cid": cid, "rationale": "Same launch.", "verdict": "accept",
                   "rel": "see_also", "confidence": 0.9})
        })
        .collect();
    let batch = json!({
        "type": "result", "subtype": "success", "is_error": false,
        "structured_output": {"verdicts": verdicts}, "total_cost_usd": 0.001,
        "usage": {"input_tokens": 10, "cache_read_input_tokens": 0, "output_tokens": 50},
    });
    let probe = json!({"type": "result", "is_error": false,
                       "structured_output": {"ok": true}, "total_cost_usd": 0.0});
    fs::write(path.dir().join("batch.out"), batch.to_string()).unwrap();
    fs::write(path.dir().join("probe.out"), probe.to_string()).unwrap();
    fs::write(path.dir().join("auth.out"), r#"{"loggedIn": true}"#).unwrap();
    path.tool(
        "claude",
        r#"d="${0%/*}"
if [ "$1" = auth ]; then /bin/cat "$d/auth.out"; exit 0; fi
case "$2" in
  'reply with'*) /bin/cat "$d/probe.out" ;;
  *) /bin/cat > /dev/null; /bin/cat "$d/batch.out" ;;
esac"#,
    );
    path
}

#[test]
fn every_note_of_an_existing_vault_is_indexed_with_what_it_infers() {
    let vault = existing();
    let out = vault.ok(&["index"]);
    assert!(out.starts_with(&format!("indexed {NOTES},")), "{out}");
    assert!(
        out.contains(", 0 skipped, 0 failed, 0 hidden links"),
        "{out}"
    );
    assert!(out.ends_with(", 6 inferred\n"), "{out}");

    let files = files(&vault);
    assert_eq!(files.len(), NOTES);
    let row = |path: &str, class: &str, title: &str| {
        let want = (inferred_id(path), class.to_string(), title.to_string());
        assert_eq!(files[path], want, "{path}");
    };
    row("Inbox.md", "Note", "Inbox");
    row("Projects/Alpha Launch.md", "Note", "Alpha launch plan");
    row("Projects/Retro Notes.md", "Note", "Retro notes");
    row("Areas/Health Check.md", "Note", "Health check");
    row("Areas/Weekly Review.md", "Concept", "Weekly Review");
}

#[test]
fn a_note_with_explicit_keys_is_not_touched_by_inference() {
    let vault = indexed();
    let want = (
        PINNED.to_string(),
        "Reference".to_string(),
        "Pinned reference".to_string(),
    );
    assert_eq!(files(&vault)["Areas/Pinned.md"], want);
}

#[test]
fn only_accept_writes_to_a_note() {
    let vault = existing();
    let before = snapshot(&vault);
    vault.ok(&["index"]);
    vault.ok(&["lint"]);
    vault.ok(&["doctor"]);
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);
    vault.ok(&["index", "--full"]);
    assert_eq!(snapshot(&vault), before);
}

#[test]
fn an_existing_vault_lints_clean_with_links_by_filename() {
    let vault = indexed();
    let out = vault.ok(&["lint", "--strict"]);
    assert!(!out.contains("link.unstable"), "{out}");
}

#[test]
fn suggest_without_a_judge_finds_candidates() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);
    assert!(!cids(&vault).is_empty());
    cid_of(
        &vault,
        "Projects/Alpha Launch.md",
        "Projects/Beta Rollout.md",
    );
}

#[test]
fn plain_suggest_without_a_judge_finds_candidates() {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(EXISTING), vault.root());
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    vault.ok(&["suggest", "--judge", "none"]);
    assert!(!cids(&vault).is_empty());
}

#[test]
fn inline_links_alone_give_the_structural_generators_candidates() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none"]);

    // Unlinked, and both link to the Health Check and the Retro Notes.
    let cid = cid_of(
        &vault,
        "Projects/Alpha Launch.md",
        "Projects/Beta Rollout.md",
    );
    let db = database(&vault);
    let found: String = db
        .query_row(
            "SELECT generators FROM candidates WHERE cid = ?",
            [cid],
            |row| row.get(0),
        )
        .unwrap();
    assert!(found.contains("common_neighbors"), "{found}");
    assert!(found.contains("adamic_adar"), "{found}");
}

#[test]
fn a_mention_weight_of_zero_turns_inline_links_off() {
    let vault = indexed();
    let config = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, text + "\n[generators]\nmention_weight = 0\n").unwrap();
    vault.ok(&["suggest", "--judge", "none"]);

    let db = database(&vault);
    let structural: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM candidates
             WHERE generators LIKE '%common_neighbors%' OR generators LIKE '%adamic_adar%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(structural, 0);
}

#[test]
fn a_generators_table_without_a_mention_weight_gets_the_default() {
    let vault = indexed();
    let config = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, text + "\n[generators]\n").unwrap();
    vault.ok(&["suggest", "--judge", "none"]);

    let db = database(&vault);
    let structural: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE generators LIKE '%common_neighbors%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(structural > 0);
}

#[test]
fn a_mention_weight_outside_zero_to_one_is_refused() {
    let vault = indexed();
    let config = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, text + "\n[generators]\nmention_weight = 1.5\n").unwrap();
    let output = vault.kbx(&["suggest", "--judge", "none"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("mention_weight"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn accept_writes_the_relation_alone_and_a_reindex_keeps_every_id() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);
    let vault = {
        let path = fake_judge(&vault);
        vault.with_path(path)
    };
    let out = vault.ok(&[
        "suggest",
        "--judge",
        "claude-cli",
        GENERATORS[0],
        GENERATORS[1],
    ]);
    assert!(out.contains("candidates"), "{out}");
    let ids = files(&vault);
    let before = snapshot(&vault);

    let (alpha, beta) = ("Projects/Alpha Launch.md", "Projects/Beta Rollout.md");
    let cid = cid_of(&vault, alpha, beta);
    vault.ok(&["accept", &cid]);

    let mut want = before.clone();
    want.insert(
        alpha.to_string(),
        b"---
aliases: [alpha]
tags: [project, launch]
created: 2026-01-12
status: active
see_also: [\"[[Projects/Beta Rollout]]\"]
---
# Alpha launch plan

Depends on the [[Health Check]] passing, and on the [[Retro Notes]].
"
        .to_vec(),
    );
    want.insert(
        beta.to_string(),
        b"---
tags:
  - project
cssclasses: wide  # keep this comment
see_also: [\"[[Projects/Alpha Launch]]\"]
---

# Beta rollout

Follows the alpha launch plan. Reuses the [[Health Check]] and what the
[[Retro Notes]] say.
"
        .to_vec(),
    );
    assert_eq!(snapshot(&vault), want);

    vault.ok(&["index"]);
    assert_eq!(files(&vault), ids);
    vault.ok(&["index", "--full"]);
    assert_eq!(files(&vault), ids);
    let edges: i64 = database(&vault)
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE rel = 'see_also' AND origin = 'frontmatter'
             AND src IN (?1, ?2) AND dst IN (?1, ?2)",
            [inferred_id(alpha), inferred_id(beta)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(edges, 2);
    assert_eq!(snapshot(&vault), want, "indexing writes nothing");
}

#[test]
fn accept_gives_a_note_without_frontmatter_a_block_with_the_relation_alone() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);
    let (retro, weekly) = ("Projects/Retro Notes.md", "Areas/Weekly Review.md");
    let cid = cid_of(&vault, retro, weekly);
    let ids = files(&vault);

    let body = fs::read_to_string(vault.root().join(retro)).unwrap();
    vault.ok(&["accept", &cid, "--rel", "see_also"]);
    let text = fs::read_to_string(vault.root().join(retro)).unwrap();
    let block = "---\nsee_also: [\"[[Areas/Weekly Review]]\"]\n---\n";
    assert_eq!(text, format!("{block}{body}"));

    vault.ok(&["index"]);
    assert_eq!(files(&vault), ids);
}

#[test]
fn a_note_with_all_its_keys_links_to_an_inferred_one_by_path_and_keeps_its_mirror() {
    let vault = existing().writing();
    vault.write(
        "Areas/Pinned.md",
        &format!(
            "---\nid: {PINNED}\ntype: Reference\ntitle: Pinned reference\n---\n\
             About the weekly twin.\n"
        ),
    );
    vault.write(
        "Projects/Weekly Twin.md",
        "# Weekly twin\n\nPinned reference.\n",
    );
    vault.ok(&["index"]);
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);
    let twin = inferred_id("Projects/Weekly Twin.md");
    let pair = cids(&vault)
        .into_iter()
        .find(|(_, src, dst)| (*src == PINNED && *dst == twin) || (*src == twin && *dst == PINNED));
    let cid = pair.expect("a candidate for the twins").0;
    vault.ok(&["accept", &cid, "--rel", "see_also"]);

    let pinned = fs::read_to_string(vault.root().join("Areas/Pinned.md")).unwrap();
    let want = format!(
        "---\nid: {PINNED}\ntype: Reference\ntitle: Pinned reference\n\
         see_also: [\"[[Projects/Weekly Twin]]\"]\n\
         qmd:\n  metadata: {{type: Reference, tags: []}}\n---\nAbout the weekly twin.\n"
    );
    assert_eq!(pinned, want);
    let inferred = fs::read_to_string(vault.root().join("Projects/Weekly Twin.md")).unwrap();
    let want =
        format!("---\nsee_also: [\"[[{PINNED}]]\"]\n---\n# Weekly twin\n\nPinned reference.\n");
    assert_eq!(inferred, want);
}

#[test]
fn accept_no_write_previews_the_relation_alone_and_changes_no_note() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);
    let before = snapshot(&vault);
    let cid = cid_of(
        &vault,
        "Projects/Alpha Launch.md",
        "Projects/Beta Rollout.md",
    );

    let out = vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    assert_eq!(snapshot(&vault), before);
    assert!(
        out.contains("+see_also: [\"[[Projects/Beta Rollout]]\"]"),
        "{out}"
    );
    assert!(!out.contains("+id:") && !out.contains("+qmd:"), "{out}");

    vault.ok(&["accept", &cid, "--rel", "see_also"]);
    let after = snapshot(&vault);
    let alpha = String::from_utf8_lossy(&after["Projects/Alpha Launch.md"]);
    assert!(
        alpha.contains("see_also: [\"[[Projects/Beta Rollout]]\"]"),
        "{alpha}"
    );
}

#[test]
fn accept_no_write_keeps_a_scope_default_across_candidates_sharing_a_note() {
    let vault = existing();
    vault.write("Projects/Gamma.md", "Follows the alpha work.\n");
    let path = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&path).unwrap();
    let edited = text.replace(
        "Projects = { default_type = \"Note\" }",
        "Projects = { default_type = \"Procedure\" }",
    );
    fs::write(&path, edited).unwrap();
    vault.ok(&["index"]);
    vault.ok(&["suggest", "--judge", "none", GENERATORS[0], GENERATORS[1]]);

    // `depends_on` starts at a Procedure, which only the scope default says.
    let alpha = "Projects/Alpha Launch.md";
    let first = cid_of(&vault, alpha, "Projects/Beta Rollout.md");
    let second = cid_of(&vault, alpha, "Projects/Gamma.md");
    vault.ok(&[
        "accept",
        &first,
        &second,
        "--rel",
        "depends_on",
        "--no-write",
    ]);
}

#[test]
fn staging_leaves_a_note_with_inferred_keys_alone() {
    let vault = indexed();
    let before = snapshot(&vault);
    let out = vault.ok(&[
        "suggest",
        "--judge",
        "none",
        GENERATORS[0],
        GENERATORS[1],
        "--stage",
    ]);
    assert!(out.contains("notes with inferred keys alone"), "{out}");
    let after = snapshot(&vault);
    assert_eq!(after, before);
}

#[test]
fn init_writes_a_default_type_for_the_vault_and_each_scope() {
    let vault = existing();
    let text = fs::read_to_string(vault.root().join(".kbx/kbx.toml")).unwrap();
    assert!(text.contains("default_type = \"Note\"  #"), "{text}");
    assert!(
        text.contains("Areas = { default_type = \"Note\" }\n"),
        "{text}"
    );
    assert!(
        text.contains("Projects = { default_type = \"Note\" }\n"),
        "{text}"
    );
}

#[test]
fn a_kbx_toml_from_before_default_type_falls_back_on_note() {
    let vault = existing();
    let path = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("default_type"))
        .collect();
    fs::write(&path, kept.join("\n").replace("[scope]", "")).unwrap();
    vault.ok(&["index"]);
    assert_eq!(files(&vault)["Inbox.md"].1, "Note");
}

#[test]
fn a_scope_default_applies_and_changing_it_rebuilds_the_index() {
    let vault = indexed();
    let path = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&path).unwrap();
    let edited = text.replace(
        "Projects = { default_type = \"Note\" }",
        "Projects = { default_type = \"Entity\" }",
    );
    fs::write(&path, edited).unwrap();

    let doctor = vault.ok(&["doctor"]);
    assert!(doctor.contains("stale, run `kbx index`"), "{doctor}");
    vault.ok(&["index"]);
    let files = files(&vault);
    assert_eq!(files["Projects/Alpha Launch.md"].1, "Entity");
    assert_eq!(files["Inbox.md"].1, "Note");
    assert_eq!(files["Areas/Health Check.md"].1, "Note");
    assert_eq!(files["Areas/Pinned.md"].1, "Reference");
}

#[test]
fn doctor_says_what_to_add_for_a_default_type_the_ontology_lacks() {
    let vault = indexed();
    let path = vault.root().join(".kbx/ontology.yaml");
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("  Note:")).collect();
    fs::write(&path, kept.join("\n")).unwrap();

    let output = vault.kbx(&["doctor"]);
    let out = common::stdout(&output);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(out.contains("`default_type` gives `Note`"), "{out}");
    assert!(out.contains("add `Note: {description:"), "{out}");

    let lint = common::stdout(&vault.kbx(&["lint"]));
    assert!(
        lint.contains("default type `Note` is not a class"),
        "{lint}"
    );
}

#[test]
fn doctor_names_the_vaults_own_types_the_ontology_lacks() {
    let vault = existing();
    vault.write("Projects/Standup.md", "---\ntype: meeting\n---\nDaily.\n");
    vault.ok(&["index"]);
    let out = vault.ok(&["doctor"]);
    assert!(
        out.contains("`meeting` (1) are in notes but not in"),
        "{out}"
    );
}
