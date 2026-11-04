//! `kbx export`, `kbx import` and `kbx suggest --eval` through the binary.

mod common;

use std::fs;
use std::path::Path;

use common::{Fixture, stderr, stdout};
use rusqlite::Connection;
use serde_json::Value;

fn db(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

/// Every row of `reviews`, column by column, in a fixed order.
fn table(vault: &Fixture) -> Vec<Vec<String>> {
    let db = db(vault);
    let mut statement = db
        .prepare(
            "SELECT quote(src), quote(dst), quote(rel), quote(decision), quote(reason),
                 quote(strikes), quote(decided_at)
             FROM reviews ORDER BY src, dst, rel",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| (0..7).map(|i| row.get::<_, String>(i)).collect())
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn suggested() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    suggest(&vault);
    vault
}

fn suggest(vault: &Fixture) {
    vault.ok(&[
        "suggest",
        "--judge",
        "none",
        "--generators",
        "common_neighbors",
    ]);
}

fn cids(vault: &Fixture) -> Vec<String> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT cid FROM candidates ORDER BY score DESC, cid")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

/// A vault with an accepted, a rejected and a deferred pair.
fn reviewed() -> Fixture {
    let vault = suggested();
    let cids = cids(&vault);
    assert!(cids.len() >= 3, "the fixture should give three candidates");
    vault.ok(&[
        "accept",
        &cids[0],
        "--rel",
        "see_also",
        "--reason",
        "same \"topic\"\nreally",
    ]);
    vault.ok(&[
        "reject",
        &cids[1],
        "--rel",
        "see_also",
        "--reason",
        "é unrelated",
    ]);
    vault.ok(&["defer", &cids[2]]);
    vault
}

fn export_reviews(vault: &Fixture) -> String {
    vault.ok(&["export", "reviews", "--jsonl"])
}

#[test]
fn reviews_round_trip_through_import_to_an_identical_table() {
    let vault = reviewed();
    let exported = export_reviews(&vault);
    assert_eq!(exported.lines().count(), 3);

    let other = Fixture::new();
    other.write("reviews.jsonl", &exported);
    let printed = other.ok(&["import", "reviews", "--jsonl", "reviews.jsonl"]);
    assert!(printed.contains("3 reviews"), "{printed}");

    assert_eq!(table(&other), table(&vault));
    assert_eq!(export_reviews(&other), exported);
}

#[test]
fn importing_again_changes_nothing() {
    let vault = reviewed();
    let before = table(&vault);
    let exported = export_reviews(&vault);
    vault.write("reviews.jsonl", &exported);
    vault.ok(&["import", "reviews", "--jsonl", "reviews.jsonl"]);
    vault.ok(&["import", "reviews", "--jsonl", "reviews.jsonl"]);
    assert_eq!(table(&vault), before);
}

#[test]
fn a_bad_import_leaves_the_table_alone() {
    let vault = reviewed();
    let before = table(&vault);
    let first = export_reviews(&vault).lines().next().unwrap().to_string();
    let changed = first.replace("accept", "reject").replace("reject", "defer");
    vault.write("reviews.jsonl", &format!("{changed}\nnot json\n"));

    let output = vault.kbx(&["import", "reviews", "--jsonl", "reviews.jsonl"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("line 2"), "{}", stderr(&output));
    assert_eq!(table(&vault), before);
}

#[test]
fn import_reads_stdin_without_a_file() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let vault = reviewed();
    let exported = export_reviews(&vault);
    let other = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kbx"))
        .args(["import", "reviews", "--jsonl"])
        .current_dir(other.root())
        .env("PATH", other.path.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(exported.as_bytes()).unwrap();
    drop(stdin);
    assert!(child.wait_with_output().unwrap().status.success());
    assert_eq!(table(&other), table(&vault));
}

#[test]
fn exports_change_nothing_on_disk() {
    let vault = reviewed();
    let before = fs::read(vault.root().join(".kbx/graph.sqlite")).unwrap();
    vault.ok(&["export", "edges", "--jsonl"]);
    vault.ok(&["export", "reviews", "--jsonl"]);
    vault.ok(&["export", "rdf"]);
    assert_eq!(
        fs::read(vault.root().join(".kbx/graph.sqlite")).unwrap(),
        before
    );
}

#[test]
fn edges_are_one_object_a_line_and_match_the_table() {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    let printed = vault.ok(&["export", "edges", "--jsonl"]);
    let count: i64 = db(&vault)
        .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(printed.lines().count() as i64, count);
    let first: Value = serde_json::from_str(printed.lines().next().unwrap()).unwrap();
    assert!(first["src"].is_string() && first["rel"].is_string());
    assert_eq!(
        vault.ok(&["export", "edges"]),
        printed,
        "--jsonl is the default"
    );
}

#[test]
fn rdf_comes_out_as_turtle_a_parser_accepts() {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    let printed = vault.ok(&["export", "rdf"]);
    let parser = oxttl::TurtleParser::new();
    let triples: Vec<_> = parser
        .for_slice(printed.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(triples.len() > 20);
}

#[test]
fn exporting_the_graph_before_any_index_says_so() {
    let vault = Fixture::new();
    for what in ["edges", "rdf"] {
        let output = vault.kbx(&["export", what]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains("kbx index"), "{}", stderr(&output));
    }
}

/// Every file of the vault and its contents, `.kbx` included.
fn contents(dir: &Path, into: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(Result::unwrap).collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if entry.path().is_dir() {
            contents(&entry.path(), into);
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            into.push((entry.path().display().to_string(), bytes));
        }
    }
}

#[test]
fn eval_scores_the_generators_and_changes_nothing() {
    let vault = reviewed();
    let mut before = Vec::new();
    contents(vault.root(), &mut before);

    let printed = vault.ok(&["suggest", "--eval", "--generators", "common_neighbors"]);

    let mut after = Vec::new();
    contents(vault.root(), &mut after);
    assert_eq!(after, before, "--eval must leave the vault alone");

    let row = printed
        .lines()
        .find(|line| line.starts_with("common_neighbors"))
        .unwrap_or_else(|| panic!("no row in {printed}"));
    let columns: Vec<_> = row.split_whitespace().collect();
    // Found again, though accepting linked the pair and deleted its candidate.
    assert_eq!(columns[3], "1", "{printed}");
    assert_eq!(columns[5], "1.00", "{printed}");
    assert!(printed.contains("1 accepted pair"), "{printed}");
}

#[test]
fn eval_takes_no_flags_that_would_write_or_limit() {
    let vault = suggested();
    for flag in [&["--dry-run"][..], &["--judge", "auto"], &["--budget", "3"]] {
        let args = [&["suggest", "--eval"], flag].concat();
        let output = vault.kbx(&args);
        assert!(!output.status.success(), "{args:?}: {}", stdout(&output));
    }
}
