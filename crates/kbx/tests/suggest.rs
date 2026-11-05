//! Drives `kbx suggest` over copies of the fixture vault and a generated one.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use common::{FakePath, Fixture, id, stderr, stdout};
use rusqlite::Connection;
use serde_json::{Value, json};

fn indexed() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault
}

struct Row {
    src: String,
    dst: String,
    generators: Value,
    features: Value,
    score: f64,
}

fn candidates(vault: &Fixture) -> BTreeMap<String, Row> {
    let db = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let mut statement = db
        .prepare("SELECT cid, src, dst, generators, features, score FROM candidates")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            let generators: String = row.get(3)?;
            let features: String = row.get(4)?;
            let candidate = Row {
                src: row.get(1)?,
                dst: row.get(2)?,
                generators: serde_json::from_str(&generators).unwrap(),
                features: serde_json::from_str(&features).unwrap(),
                score: row.get(5)?,
            };
            Ok((row.get::<_, String>(0)?, candidate))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn find(rows: &BTreeMap<String, Row>, a: u32, b: u32) -> &Row {
    let (a, b) = (id(a), id(b));
    rows.values()
        .find(|row| (row.src == a && row.dst == b) || (row.src == b && row.dst == a))
        .unwrap_or_else(|| panic!("no candidate for {a} and {b}"))
}

/// Answers every search with one hit on `…13-cookie-lifetime`, as qmd would
/// name it.
fn with_fake_qmd(vault: &Fixture) {
    let handle = format!(
        "qmd://operations/{}-cookie-lifetime.md",
        id(13).to_lowercase()
    );
    // In parts: batch's `type` can't read a path with a `/` in it.
    let answer = vault.root().join(".kbx").join("qmd-answer.json");
    let json = format!("[{{\"score\":0.75,\"file\":\"{handle}\",\"line\":7}}]");
    fs::write(&answer, json).unwrap();
    // By full path: the fake `PATH` holds nothing else.
    let print = if cfg!(windows) { "type" } else { "/bin/cat" };
    vault
        .path
        .tool("qmd", &format!("{print} \"{}\"", answer.display()));
}

/// Runs kbx, requiring that no generator was skipped.
fn ok_with_every_generator(vault: &Fixture, args: &[&str]) -> String {
    let output = vault.kbx(args);
    let log = stderr(&output);
    assert!(output.status.success(), "{log}");
    assert!(!log.contains("skipped"), "{log}");
    stdout(&output)
}

#[test]
fn suggest_stores_each_section_5_case_with_its_provenance() {
    let vault = indexed();
    with_fake_qmd(&vault);
    let out = ok_with_every_generator(&vault, &["suggest", "--judge", "none"]);
    let rows = candidates(&vault);
    assert!(out.contains(&format!("{} candidates", rows.len())), "{out}");

    let shared = find(&rows, 8, 9);
    for generator in ["common_neighbors", "adamic_adar", "two_hop_bridge"] {
        assert!(
            shared
                .generators
                .as_array()
                .unwrap()
                .contains(&json!(generator)),
            "{generator} is missing from {}",
            shared.generators
        );
    }
    assert_eq!(shared.features["common_neighbors"]["score"], json!(2.0));
    assert_eq!(
        shared.features["two_hop_bridge"]["via"],
        json!([id(6), id(7)])
    );
    assert!(shared.features["adamic_adar"]["score"].as_f64().unwrap() > 0.0);

    let gap = find(&rows, 10, 8);
    assert_eq!(
        gap.src,
        id(10),
        "a gap points away from the note that has it"
    );
    assert_eq!(
        gap.features["schema_gap"]["relations"],
        json!(["depends_on"])
    );

    let alias = find(&rows, 11, 6);
    assert_eq!(alias.src, id(11));
    assert_eq!(
        alias.features["alias_cooccur"]["terms"],
        json!(["token bucket"])
    );

    let knn = find(&rows, 12, 13);
    assert_eq!(knn.features["qmd_knn"]["score"], json!(0.75));

    for row in rows.values() {
        let generators = row.generators.as_array().unwrap();
        assert!(!generators.is_empty());
        let features = row.features.as_object().unwrap();
        assert_eq!(features.len(), generators.len(), "one entry per generator");
        for generator in generators {
            assert!(features[generator.as_str().unwrap()]["score"].is_number());
        }
        assert!(row.score > 0.0 && row.score < 1.0);
    }
}

#[test]
fn already_linked_pairs_are_not_stored() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none"]);
    let db = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let linked: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM candidates c JOIN edges e
             ON (e.src = c.src AND e.dst = c.dst) OR (e.src = c.dst AND e.dst = c.src)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, 0);
}

#[test]
fn a_rerun_reaches_the_same_rows() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none"]);
    let first = candidates(&vault);
    vault.ok(&["suggest", "--judge", "none"]);
    let second = candidates(&vault);
    assert_eq!(
        first.keys().collect::<Vec<_>>(),
        second.keys().collect::<Vec<_>>()
    );
}

#[test]
fn generators_limits_the_run_to_those_named() {
    let vault = indexed();
    vault.ok(&["suggest", "--generators", "alias_cooccur,schema_gap"]);
    let rows = candidates(&vault);
    assert!(!rows.is_empty());
    for row in rows.values() {
        for generator in row.generators.as_array().unwrap() {
            assert!(["alias_cooccur", "schema_gap"].contains(&generator.as_str().unwrap()));
        }
    }
}

#[test]
fn an_unknown_generator_is_an_error() {
    let vault = indexed();
    let output = vault.kbx(&["suggest", "--generators", "nope"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("nope"), "{}", stderr(&output));
}

#[test]
fn note_limits_the_run_to_pairs_touching_it() {
    let vault = indexed();
    with_fake_qmd(&vault);
    ok_with_every_generator(&vault, &["suggest", "--note", &id(12)]);
    let rows = candidates(&vault);
    let generators = find(&rows, 12, 13).generators.as_array().unwrap();
    assert!(generators.contains(&json!("qmd_knn")), "{generators:?}");
    assert!(rows.values().all(|r| r.src == id(12) || r.dst == id(12)));
}

#[test]
fn an_unknown_note_is_an_error() {
    let vault = indexed();
    let output = vault.kbx(&["suggest", "--note", "nope"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("no note with id nope"));
}

#[test]
fn only_judge_none_exists_so_far() {
    let vault = indexed();
    assert!(
        !vault
            .kbx(&["suggest", "--judge", "claude-cli"])
            .status
            .success()
    );
}

#[test]
fn suggest_needs_an_index() {
    let vault = Fixture::new();
    let output = vault.kbx(&["suggest"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("kbx index"), "{}", stderr(&output));
}

#[test]
fn without_qmd_the_other_generators_still_run_and_it_says_so() {
    let vault = indexed();
    fs::remove_file(vault.path.program("qmd")).unwrap();
    let output = vault.kbx(&["suggest", "--judge", "none"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("qmd_knn skipped"),
        "{}",
        stderr(&output)
    );
    assert!(!candidates(&vault).is_empty());
}

#[test]
fn a_generator_asked_for_by_name_must_work() {
    let vault = indexed();
    fs::remove_file(vault.path.program("qmd")).unwrap();
    let output = vault.kbx(&["suggest", "--generators", "qmd_knn,schema_gap"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("qmd_knn"), "{}", stderr(&output));
    assert!(candidates(&vault).is_empty(), "a failed run stores nothing");
}

/// Every file in a folder: its bytes and when it last changed.
fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    let entries = fs::read_dir(dir).unwrap().map(Result::unwrap);
    entries
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let modified = entry.metadata().unwrap().modified().unwrap();
            (name, (fs::read(entry.path()).unwrap(), modified))
        })
        .collect()
}

#[test]
fn dry_run_prints_counts_and_tokens_and_leaves_the_database_alone() {
    let vault = indexed();
    let kbx_dir = vault.root().join(".kbx");
    let before = snapshot(&kbx_dir);
    assert!(before.contains_key("graph.sqlite"));

    let out = vault.ok(&["suggest", "--judge", "none", "--dry-run"]);

    assert_eq!(snapshot(&kbx_dir), before, "nothing in .kbx may change");
    assert!(candidates(&vault).is_empty());
    assert!(out.contains("would store"), "{out}");
    assert!(out.contains("schema_gap"), "{out}");
    assert!(out.contains("tokens"), "{out}");
    assert!(out.contains("batch 1:"), "{out}");
}

#[test]
fn dry_run_counts_what_a_real_run_then_stores() {
    let vault = indexed();
    let out = vault.ok(&["suggest", "--dry-run"]);
    vault.ok(&["suggest"]);
    let stored = candidates(&vault).len();
    assert!(
        out.contains(&format!("would store {stored} candidates")),
        "{out}"
    );
}

#[test]
fn dry_run_works_on_a_read_only_database() {
    let vault = indexed();
    let path = vault.root().join(".kbx/graph.sqlite");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    vault.ok(&["suggest", "--dry-run"]);
}

/// 120 Procedures sharing two Concepts. Every pair shares a neighbour, so
/// there are 7,140 pairs to hold down.
fn crowded() -> Fixture {
    let vault = Fixture::new();
    for entry in fs::read_dir(vault.root()).unwrap().map(Result::unwrap) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !name.starts_with('.') {
            fs::remove_dir_all(entry.path()).unwrap();
        }
    }
    for n in 1..=2 {
        let id = id(n);
        vault.write(
            &format!("crowd/{id}.md"),
            &format!("---\nid: {id}\ntype: Concept\ntitle: Concept {n}\n---\nBody.\n"),
        );
    }
    let (one, two) = (id(1), id(2));
    for n in 3..=122 {
        let id = id(n);
        vault.write(
            &format!("crowd/{id}.md"),
            &format!(
                "---\nid: {id}\ntype: Procedure\ntitle: Step {n}\n\
                 depends_on: [\"[[{one}]]\", \"[[{two}]]\"]\n---\nBody.\n"
            ),
        );
    }
    vault.ok(&["index"]);
    vault
}

#[test]
fn default_budgets_hold_40_per_note_and_2000_per_run() {
    let vault = crowded();
    let out = vault.ok(&["suggest", "--generators", "common_neighbors"]);
    let rows = candidates(&vault);
    assert_eq!(rows.len(), 2000, "{out}");
    let mut per_note: HashMap<&str, usize> = HashMap::new();
    for row in rows.values() {
        *per_note.entry(&row.src).or_default() += 1;
        *per_note.entry(&row.dst).or_default() += 1;
    }
    assert_eq!(per_note.values().max(), Some(&40));
    assert!(out.contains("over budget"), "{out}");
}

#[test]
fn a_failed_kbx_run_leaves_stdout_clean() {
    let vault = indexed();
    let output = vault.kbx(&["suggest", "--note", "nope"]);
    assert_eq!(stdout(&output), "");
}

/// A vault whose only note fails to index, so its index is built and empty.
fn empty_graph() -> Fixture {
    let vault = Fixture::bare(FakePath::with_qmd());
    vault.ok(&["init", "--search", "qmd"]);
    vault.write("a.md", "---\nid: [\n---\n");
    assert!(!vault.kbx(&["index"]).status.success());
    vault
}

#[test]
fn an_empty_graph_says_so_and_what_to_do_instead_of_0_candidates() {
    let vault = empty_graph();
    for args in [
        vec!["suggest", "--judge", "none"],
        vec!["suggest", "--judge", "none", "--dry-run"],
        vec!["suggest", "--judge", "none", "--generators", "adamic_adar"],
        vec!["suggest", "--eval"],
    ] {
        let said = vault.ok(&args);
        assert!(said.contains("the graph is empty"), "{args:?}: {said}");
        assert!(said.contains("kbx index"), "{args:?}: {said}");
        assert!(!said.contains("0 candidates"), "{args:?}: {said}");
    }
}

#[test]
fn a_graph_with_notes_never_says_it_is_empty() {
    let vault = indexed();
    with_fake_qmd(&vault);
    let said = ok_with_every_generator(&vault, &["suggest", "--judge", "none"]);
    assert!(!said.contains("the graph is empty"), "{said}");
}
