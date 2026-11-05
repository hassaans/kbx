//! `search = "builtin"`, on the tiny fixed model in `tests/fixtures/tiny-model`.
//! Its weights are random, so these check the wiring, not what it finds. What
//! it finds is measured in docs/search-engine.md.

mod common;

use std::fs;
use std::path::Path;

use common::{CLOSED_PORT, FIXTURE, FakePath, Fixture, copy_dir, stderr, stdout};
use rusqlite::Connection;

const TINY_MODEL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/tiny-model"
);
const INDEX: &str = ".kbx/search-index.sqlite";

/// The fixture vault with `path` for `PATH`, set to the built-in engine. `kbx
/// init` needs qmd until the built-in engine is the default, so it gets one
/// that's gone again before the test begins.
fn vault(path: FakePath) -> Fixture {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    vault.ok(&["init"]);
    let config = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    let text = text.replace("search = \"qmd\"", "search = \"builtin\"");
    fs::write(config, text).unwrap();
    vault.ok(&["index"]);
    vault.with_path(path)
}

fn generators(vault: &Fixture) -> Vec<String> {
    let connection = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let mut statement = connection
        .prepare("SELECT generators FROM candidates")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn from_knn(vault: &Fixture) -> usize {
    let rows = generators(vault);
    rows.iter().filter(|row| row.contains("qmd_knn")).count()
}

/// Run with the model in place and no way to reach any other, as a home folder
/// of its own.
fn with_model(vault: &Fixture, args: &[&str]) -> std::process::Output {
    let home = vault.root().join(".home");
    let home = home.to_str().unwrap();
    let env = [
        ("KBX_MODEL_DIR", TINY_MODEL),
        ("KBX_MODEL_URL", CLOSED_PORT),
        ("HOME", home),
        ("XDG_CACHE_HOME", home),
    ];
    vault.kbx_with(args, &env)
}

/// Run with no model and nowhere to get one.
fn offline(vault: &Fixture, args: &[&str]) -> std::process::Output {
    let home = vault.root().join(".home");
    let home = home.to_str().unwrap();
    let env = [
        ("KBX_MODEL_URL", CLOSED_PORT),
        ("HOME", home),
        ("XDG_CACHE_HOME", home),
        ("LOCALAPPDATA", home),
    ];
    vault.kbx_with(args, &env)
}

const KNN: [&str; 5] = ["suggest", "--judge", "none", "--generators", "qmd_knn"];

#[test]
fn suggest_finds_knn_candidates_with_no_qmd_on_path() {
    let vault = vault(FakePath::empty());
    let output = with_model(&vault, &KNN);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(from_knn(&vault) > 0, "{}", stdout(&output));
    assert!(vault.root().join(INDEX).is_file());
}

#[test]
fn the_builtin_engine_never_runs_qmd() {
    let path = FakePath::empty();
    let calls = path.dir().join("calls");
    path.tool("qmd", &format!("echo called >> \"{}\"", calls.display()));
    let vault = vault(path);
    let output = with_model(&vault, &["suggest", "--judge", "none"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!calls.exists(), "{}", fs::read_to_string(&calls).unwrap());
}

#[test]
fn a_second_run_needs_no_model() {
    let vault = vault(FakePath::empty());
    assert!(with_model(&vault, &KNN).status.success());
    let first = from_knn(&vault);

    // With the model gone, nothing has to be embedded, so nothing is missed.
    let output = offline(&vault, &KNN);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stderr(&output).contains("keyword"), "{}", stderr(&output));
    assert_eq!(from_knn(&vault), first);
}

#[test]
fn no_network_still_gets_keyword_results_and_says_why() {
    let vault = vault(FakePath::empty());
    let output = offline(&vault, &KNN);
    let log = stderr(&output);
    assert!(output.status.success(), "{log}");
    assert!(log.contains("searching by keyword alone"), "{log}");
    assert!(log.contains("could not download"), "{log}");
    assert!(from_knn(&vault) > 0, "{}", stdout(&output));
}

#[test]
fn vectors_arrive_once_the_model_does() {
    let vault = vault(FakePath::empty());
    assert!(offline(&vault, &KNN).status.success());
    let output = with_model(&vault, &KNN);
    assert!(output.status.success(), "{}", stderr(&output));

    let connection = Connection::open(vault.root().join(INDEX)).unwrap();
    let query = "SELECT count(*) FROM texts WHERE vector IS NULL";
    let missing: i64 = connection.query_row(query, [], |row| row.get(0)).unwrap();
    assert_eq!(missing, 0);
}

#[test]
fn a_dry_run_writes_no_index_and_says_what_is_missing() {
    let vault = vault(FakePath::empty());
    let output = with_model(&vault, &["suggest", "--judge", "none", "--dry-run"]);
    let log = stderr(&output);
    assert!(output.status.success(), "{log}");
    assert!(log.contains("qmd_knn skipped"), "{log}");
    assert!(log.contains("run `kbx suggest`"), "{log}");
    assert!(!vault.root().join(INDEX).exists());
}

#[test]
fn a_dry_run_searches_the_index_there_is() {
    let vault = vault(FakePath::empty());
    assert!(with_model(&vault, &KNN).status.success());
    let args = [
        "suggest",
        "--judge",
        "none",
        "--generators",
        "qmd_knn",
        "--dry-run",
    ];
    let output = with_model(&vault, &args);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("qmd_knn"), "{}", stdout(&output));
}

#[test]
fn a_generator_list_without_knn_builds_no_index() {
    let vault = vault(FakePath::empty());
    let args = [
        "suggest",
        "--judge",
        "none",
        "--generators",
        "alias_cooccur",
    ];
    assert!(with_model(&vault, &args).status.success());
    assert!(!vault.root().join(INDEX).exists());
}

#[test]
fn doctor_leaves_qmd_alone_on_the_builtin_engine() {
    let path = FakePath::empty();
    let calls = path.dir().join("calls");
    path.tool("npm", &format!("echo called >> \"{}\"", calls.display()));
    let vault = vault(path);
    let output = offline(&vault, &["doctor"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).lines().any(|line| line.starts_with("qmd ")));
    assert!(!calls.exists());
}
