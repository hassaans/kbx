//! `search = "builtin"`, on the tiny fixed model in `tests/fixtures/tiny-model`.
//! Its weights are random, so these check the wiring, not what it finds. What
//! it finds is measured in docs/search-engine.md. Also what `kbx init` and `kbx
//! doctor` do with the built-in default.

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

/// The fixture vault as a new user gets it, with `path` for `PATH`.
fn vault(path: FakePath) -> Fixture {
    let vault = Fixture::bare(path);
    copy_dir(Path::new(FIXTURE), vault.root());
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    vault
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
fn init_writes_builtin_and_runs_neither_npm_nor_qmd() {
    let path = FakePath::empty();
    let calls = path.dir().join("calls");
    let log = format!("echo called >> \"{}\"", calls.display());
    path.tool("npm", &log);
    path.tool("node", &log);
    let vault = Fixture::bare(path);
    let out = vault.ok(&["init"]);
    let config = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    assert!(config.contains("search = \"builtin\""), "{config}");
    assert!(out.contains("built-in engine"), "{out}");
    assert!(!out.contains("found qmd"), "{out}");
    assert!(!calls.exists());
}

#[test]
fn doctor_installs_nothing_with_the_builtin_default() {
    let path = FakePath::empty();
    let calls = path.dir().join("calls");
    let log = format!("echo called >> \"{}\"", calls.display());
    path.tool("npm", &log);
    path.tool("node", &log);
    let vault = vault(path);

    let output = offline(&vault, &["doctor"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}{}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert!(
        text.lines()
            .any(|l| l.starts_with("search") && l.contains("builtin")),
        "{text}"
    );
    assert!(!text.lines().any(|l| l.starts_with("qmd ")), "{text}");
    assert!(!calls.exists());
}

#[test]
fn doctor_outside_a_vault_installs_nothing() {
    let path = FakePath::empty();
    let calls = path.dir().join("calls");
    path.tool("npm", &format!("echo called >> \"{}\"", calls.display()));
    let dir = tempfile::TempDir::new().unwrap();
    let output = path.kbx(dir.path(), &["doctor"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!calls.exists());
}

#[test]
fn doctor_says_when_the_model_is_ready() {
    let vault = vault(FakePath::empty());
    let output = with_model(&vault, &["doctor"]);
    assert!(
        stdout(&output).contains("builtin, model ready"),
        "{}",
        stdout(&output)
    );
}

/// `kbx.toml` of `body`, without the line `search = …` of `kbx init`.
fn without_search_key(vault: &Fixture, qmd_line: Option<&str>) {
    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let kept = text
        .lines()
        .filter(|line| !line.starts_with("search = ") && !line.starts_with("qmd = "));
    let mut kept: Vec<&str> = kept.collect();
    if let Some(line) = qmd_line {
        kept.insert(0, line);
    }
    fs::write(file, kept.join("\n") + "\n").unwrap();
}

#[test]
fn a_file_with_no_search_key_and_no_collections_gets_the_builtin_engine() {
    let vault = vault(FakePath::empty());
    without_search_key(&vault, None);
    let output = offline(&vault, &["doctor"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("builtin"), "{}", stdout(&output));
}

#[test]
fn a_file_with_no_search_key_keeps_the_collections_it_lists() {
    let vault = vault(FakePath::with_qmd());
    let line = r#"qmd = { bin = "qmd", collections = ["notes"], roots = { notes = "." } }"#;
    without_search_key(&vault, Some(line));
    let text = vault.ok(&["doctor"]);
    assert!(
        text.contains("qmd, as kbx.toml lists qmd collections"),
        "{text}"
    );
    assert!(text.lines().any(|line| line.starts_with("qmd ")), "{text}");
}

#[test]
fn a_vault_written_with_search_qmd_stays_on_qmd() {
    let path = FakePath::with_qmd();
    let vault = Fixture::bare(path);
    copy_dir(Path::new(FIXTURE), vault.root());
    vault.ok(&["init", "--search", "qmd"]);
    let config = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    assert!(config.contains("search = \"qmd\""), "{config}");
    let text = vault.ok(&["doctor"]);
    assert!(
        text.lines().any(|line| line.starts_with("collections")),
        "{text}"
    );
}

#[test]
fn init_refuses_to_contradict_a_vaults_backend() {
    let vault = vault(FakePath::with_qmd());
    let output = vault.kbx(&["init", "--search", "qmd"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("already chooses"),
        "{}",
        stderr(&output)
    );
    vault.ok(&["init", "--search", "builtin"]);
    vault.ok(&["init"]);
}

#[test]
fn a_folder_excluded_later_leaves_the_search_index_at_the_next_suggest() {
    let vault = vault(FakePath::empty());
    let word = "xylophonewatermelon";
    let id = common::id(90);
    vault.write(
        "private/note.md",
        &format!("---\nid: {id}\ntype: Concept\ntitle: Private\n---\n\n{word}\n"),
    );
    vault.ok(&["index"]);
    assert!(with_model(&vault, &KNN).status.success());
    let terms = |vault: &Fixture| -> i64 {
        let connection = Connection::open(vault.root().join(INDEX)).unwrap();
        let query = "SELECT count(*) FROM terms WHERE terms MATCH ?";
        connection
            .query_row(query, [word], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(terms(&vault), 1);

    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        format!(
            "exclude = [\"private\"]\n{}",
            text.replacen("exclude =", "# exclude =", 1)
        ),
    )
    .unwrap();
    let output = with_model(&vault, &KNN);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(terms(&vault), 0);
}
