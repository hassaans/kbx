//! The paths qmd is given, on every platform. `qmd.rs` covers the rest of the
//! integration with a stand-in that only runs on Unix; this one is also batch,
//! because `\\?\` paths only exist on Windows, where qmd rejects them.

mod common;

use std::fs;
use std::path::Path;

use common::{FIXTURE, FakePath, Fixture, copy_dir};
use kbx_core::canonical_path;

const SCOPES: [&str; 4] = ["decisions", "engineering", "lint", "operations"];

/// A qmd with no collections, which logs the folder of each `collection add`
/// to `added`, a line each, without the quotes batch would keep.
fn logging_qmd() -> FakePath {
    let path = FakePath::empty();
    path.tool(
        "qmd",
        if cfg!(windows) {
            r#"if "%3 %4"=="collection add" >>"%~dp0added" echo %~5"#
        } else {
            r#"if [ "$3 $4" = "collection add" ]; then echo "$5" >> "${0%/*}/added"; fi"#
        },
    );
    path
}

fn fixture_vault() -> Fixture {
    let vault = Fixture::bare(logging_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    vault
}

/// Takes the folders qmd has been given since the last call.
fn take_added(vault: &Fixture) -> Vec<String> {
    let log = vault.path.dir().join("added");
    let text = fs::read_to_string(&log).unwrap_or_default();
    let _ = fs::remove_file(log);
    let mut added: Vec<_> = text.lines().map(|line| line.trim().to_string()).collect();
    added.sort();
    added
}

/// One folder per scope, each real and in a form qmd can read.
fn assert_one_plain_path_per_scope(vault: &Fixture, added: &[String]) {
    let root = canonical_path(vault.root()).unwrap();
    let want = SCOPES.map(|scope| root.join(scope).display().to_string());
    assert_eq!(added, want);
    for folder in added {
        assert!(!folder.starts_with(r"\\?\"), "{folder}");
        assert!(Path::new(folder).is_dir(), "{folder}");
    }
}

#[test]
fn init_gives_qmd_a_plain_path_for_each_scope() {
    let vault = fixture_vault();
    let out = vault.ok(&["init"]);
    for scope in SCOPES {
        assert!(
            out.contains(&format!("created qmd collection {scope}")),
            "{out}"
        );
    }
    assert_one_plain_path_per_scope(&vault, &take_added(&vault));
}

#[test]
fn doctor_fix_gives_qmd_the_same_plain_paths() {
    let vault = fixture_vault();
    vault.ok(&["init"]);
    take_added(&vault);
    vault.kbx(&["doctor", "--fix"]);
    assert_one_plain_path_per_scope(&vault, &take_added(&vault));
}

/// `?\` also finds the prefix as TOML would escape it.
#[test]
fn kbx_toml_records_no_verbatim_path() {
    let vault = fixture_vault();
    vault.ok(&["init"]);
    vault.kbx(&["doctor", "--fix"]);
    let config = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    assert!(!config.contains(r"?\"), "{config}");
}
