//! Where the config file lives: `.kbx/kbx.toml`, which `kbx init` writes and
//! which leaves everything outside `.kbx/` as it was, or a `kbx.toml` at the root
//! from before that, which still works.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{FIXTURE, FakePath, Fixture, copy_dir, stderr, stdout};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

const SUGGEST: [&str; 5] = [
    "suggest",
    "--judge",
    "none",
    "--generators",
    "common_neighbors",
];

/// Every command that reads the config, with arguments that get it that far.
const COMMANDS: [&[&str]; 14] = [
    &["index"],
    &["lint"],
    &["graph", "stats"],
    &SUGGEST,
    &["review", "--json"],
    &["sync-stage"],
    &["unreject", "--list"],
    &["accept", "nothing", "--rel", "see_also"],
    &["reject", "nothing"],
    &["defer", "nothing"],
    &["export", "edges"],
    &["export", "rdf"],
    &["bakeoff", "--prepare", "--pairs", "1"],
    &["doctor"],
];

/// The fixture vault, before `kbx init`.
fn fresh() -> Fixture {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    vault
}

/// The fixture vault with its config where an older `kbx init` put it.
fn at_root() -> Fixture {
    let vault = fresh();
    vault.ok(&["init"]);
    let root = vault.root();
    fs::rename(root.join(".kbx/kbx.toml"), root.join("kbx.toml")).unwrap();
    vault
}

/// A hash of every file and the name of every folder under `dir`, leaving out
/// the `.kbx` at its root.
fn tree_hash(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if relative == Path::new(".kbx") {
                continue;
            }
            if path.is_dir() {
                found.insert(relative, Vec::new());
                walk(root, &path, found);
            } else {
                let digest = Sha256::digest(fs::read(&path).unwrap());
                found.insert(relative, digest.to_vec());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

fn first_cid(vault: &Fixture) -> String {
    let db = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    db.query_row(
        "SELECT cid FROM candidates ORDER BY score DESC, cid LIMIT 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn inside() -> PathBuf {
    Path::new(".kbx").join("kbx.toml")
}

fn row<'a>(output: &'a str, check: &str) -> &'a str {
    let found = output.lines().find(|line| line.starts_with(check));
    found.unwrap_or_else(|| panic!("no {check} row in:\n{output}"))
}

fn assert_two_configs(output: &Output, text: &str) {
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("two config files"), "{text}");
    assert!(text.contains(&inside().display().to_string()), "{text}");
    let root = text.split(" and ").nth(1).unwrap_or_default();
    assert!(root.contains("kbx.toml"), "{text}");
    assert!(!root.contains(".kbx"), "{text}");
}

#[test]
fn init_adds_only_the_folder_to_the_root() {
    let vault = fresh();
    let before: Vec<_> = tree_hash(vault.root()).into_keys().collect();
    let printed = vault.ok(&["init"]);
    assert!(
        printed.contains(&inside().display().to_string()),
        "{printed}"
    );
    assert!(vault.root().join(".kbx/kbx.toml").is_file());
    assert!(vault.root().join(".kbx/ontology.yaml").is_file());
    assert!(vault.root().join(".kbx/graph.sqlite").is_file());
    let after: Vec<_> = tree_hash(vault.root()).into_keys().collect();
    assert_eq!(after, before);
}

#[test]
fn init_in_kbx_is_accepted_and_changes_nothing() {
    let plain = fresh();
    plain.ok(&["init"]);
    let flagged = fresh();
    flagged.ok(&["init", "--in-kbx"]);
    assert!(flagged.root().join(".kbx/kbx.toml").is_file());
    assert!(!flagged.root().join("kbx.toml").exists());
    let config = |vault: &Fixture| fs::read(vault.root().join(".kbx/kbx.toml")).unwrap();
    assert_eq!(config(&flagged), config(&plain));
}

#[test]
fn nothing_outside_kbx_changes_from_init_to_an_unwritten_accept() {
    let vault = fresh();
    let before = tree_hash(vault.root());
    vault.ok(&["init", "--in-kbx"]);
    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    let cid = first_cid(&vault);
    let printed = vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    assert!(printed.contains("+++ b/"), "{printed}");
    vault.ok(&["review", "--json"]);
    assert_eq!(tree_hash(vault.root()), before);
}

#[test]
fn qmd_setup_records_its_collections_in_the_file_in_kbx() {
    let vault = fresh();
    let before = tree_hash(vault.root());
    vault.ok(&["init", "--in-kbx", "--search", "qmd"]);
    vault.ok(&["doctor", "--fix"]);
    let config = fs::read_to_string(vault.root().join(".kbx/kbx.toml")).unwrap();
    assert!(config.contains("engineering"), "{config}");
    assert_eq!(tree_hash(vault.root()), before);
}

#[test]
fn every_command_finds_the_config_from_the_root_a_folder_and_kbx_itself() {
    let vault = Fixture::in_kbx();
    vault.ok(&["index"]);
    let folders = [
        vault.root().to_path_buf(),
        vault.root().join("engineering"),
        vault.root().join(".kbx"),
    ];
    for dir in &folders {
        for args in COMMANDS {
            let output = vault.path.kbx(dir, args);
            let (out, err) = (stdout(&output), stderr(&output));
            let lost = "run `kbx init`";
            assert!(
                !err.contains(lost) && !out.contains(lost),
                "{args:?}: {err}"
            );
        }
    }
}

#[test]
fn a_relative_vault_means_the_same_after_moving_the_file_into_kbx() {
    let vault = at_root();
    vault.ok(&["index"]);
    let stats = vault.ok(&["graph", "stats"]);
    let root = vault.root();
    fs::rename(root.join("kbx.toml"), root.join(".kbx/kbx.toml")).unwrap();
    assert_eq!(vault.ok(&["graph", "stats"]), stats);
    assert!(!root.join(".kbx/.kbx").exists());
}

#[test]
fn two_config_files_are_an_error_that_names_both() {
    let vault = Fixture::in_kbx();
    fs::copy(
        vault.root().join(".kbx/kbx.toml"),
        vault.root().join("kbx.toml"),
    )
    .unwrap();
    let folders = [
        vault.root().to_path_buf(),
        vault.root().join("engineering"),
        vault.root().join(".kbx"),
    ];
    for dir in &folders {
        let output = vault.path.kbx(dir, &["graph", "stats"]);
        assert_two_configs(&output, &stderr(&output));
    }
    let doctor = vault.kbx(&["doctor"]);
    assert_two_configs(&doctor, &stdout(&doctor));
    let init = vault.kbx(&["init"]);
    assert_two_configs(&init, &stderr(&init));
}

#[test]
fn doctor_says_which_config_file_is_in_use() {
    let inner = Fixture::in_kbx();
    let printed = inner.ok(&["doctor"]);
    let config = row(&printed, "config");
    assert!(config.contains(" ok "), "{config}");
    assert!(
        config.ends_with(&inside().display().to_string()),
        "{config}"
    );
}

#[test]
fn a_root_config_still_works_and_doctor_warns_about_it() {
    let vault = at_root();
    vault.ok(&["index"]);
    let printed = vault.ok(&["doctor"]);
    let config = row(&printed, "config");
    assert!(config.contains(" warn "), "{config}");
    assert!(config.contains("kbx doctor --fix"), "{config}");
    assert!(vault.root().join("kbx.toml").is_file(), "no --fix, no move");
}

#[test]
fn doctor_fix_moves_a_root_config_into_kbx() {
    let vault = at_root();
    vault.ok(&["index"]);
    let stats = vault.ok(&["graph", "stats"]);
    let text = fs::read(vault.root().join("kbx.toml")).unwrap();

    let printed = vault.ok(&["doctor", "--fix"]);
    let config = row(&printed, "config");
    assert!(config.contains(" ok "), "{config}");
    assert!(config.contains("moved to"), "{config}");

    assert!(!vault.root().join("kbx.toml").exists());
    assert_eq!(fs::read(vault.root().join(".kbx/kbx.toml")).unwrap(), text);
    assert_eq!(vault.ok(&["graph", "stats"]), stats);
    let printed = vault.ok(&["doctor"]);
    assert!(row(&printed, "config").contains(" ok "), "{printed}");
}

#[test]
fn doctor_fix_refuses_when_both_files_exist() {
    let vault = at_root();
    let root = vault.root();
    fs::copy(root.join("kbx.toml"), root.join(".kbx/kbx.toml")).unwrap();
    let fix = vault.kbx(&["doctor", "--fix"]);
    assert_two_configs(&fix, &stdout(&fix));
    assert!(root.join("kbx.toml").is_file());
    assert!(root.join(".kbx/kbx.toml").is_file());
}

#[test]
fn init_again_uses_the_file_in_kbx_wherever_it_was_asked_to_go() {
    let vault = Fixture::in_kbx();
    let before = tree_hash(vault.root());
    let config = fs::read(vault.root().join(".kbx/kbx.toml")).unwrap();
    for args in [&["init"][..], &["init", "--in-kbx"][..]] {
        let printed = vault.ok(args);
        assert!(!printed.contains("created"), "{printed}");
    }
    assert_eq!(tree_hash(vault.root()), before);
    assert_eq!(
        fs::read(vault.root().join(".kbx/kbx.toml")).unwrap(),
        config
    );
}

#[test]
fn init_again_leaves_a_root_config_in_use() {
    let vault = at_root();
    let before = tree_hash(vault.root());
    for args in [&["init"][..], &["init", "--in-kbx"][..]] {
        let printed = vault.ok(args);
        assert!(!printed.contains("created"), "{printed}");
    }
    assert_eq!(tree_hash(vault.root()), before);
}
