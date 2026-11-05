//! What stays out of a vault's scopes and index: `exclude` in `kbx.toml`,
//! `.gitignore` files and hidden folders. See `docs/excluding.md`.

mod common;

use std::fs;
use std::process::Command;

use common::{FakePath, Fixture, id, stderr, stdout};
use rusqlite::Connection;

fn note(n: u32, title: &str) -> String {
    let id = id(n);
    format!("---\nid: {id}\ntype: Concept\ntitle: {title}\n---\n\nThe body.\n")
}

fn bare() -> Fixture {
    Fixture::bare(FakePath::with_qmd())
}

fn paths(vault: &Fixture) -> Vec<String> {
    let connection = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let mut statement = connection
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn config(vault: &Fixture) -> toml::Value {
    let text = fs::read_to_string(vault.root().join(".kbx/kbx.toml")).unwrap();
    toml::from_str(&text).unwrap()
}

fn list(value: &toml::Value) -> Vec<&str> {
    let items = value.as_array().unwrap();
    items.iter().map(|item| item.as_str().unwrap()).collect()
}

fn edit_config(vault: &Fixture, edit: impl Fn(String) -> String) {
    let file = vault.root().join(".kbx/kbx.toml");
    fs::write(&file, edit(fs::read_to_string(&file).unwrap())).unwrap();
}

fn set_exclude(vault: &Fixture, exclude: &str) {
    edit_config(vault, |text| {
        let kept = text.lines().filter(|line| !line.starts_with("exclude ="));
        let kept = kept.collect::<Vec<_>>().join("\n");
        format!("exclude = {exclude}\n{kept}\n")
    });
}

fn git(vault: &Fixture, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(vault.root())
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

#[test]
fn init_makes_no_scope_of_an_excluded_folder_and_says_why() {
    let vault = bare();
    vault.write("team-a/one.md", &note(1, "One"));
    vault.write("node_modules/pkg/README.md", "# A package\n");
    vault.write("credentials/keys.md", &note(2, "Keys"));
    vault.write("Secrets/more.md", &note(3, "More"));
    vault.write("out/built.md", &note(4, "Built"));
    vault.write(".obsidian/app.json", "{}");
    vault.write(".gitignore", "/out/\n");

    let printed = vault.ok(&["init"]);

    assert_eq!(list(&config(&vault)["scopes"]), ["team-a"]);
    for line in [
        "skipped credentials: matches `credentials` in `exclude`",
        "skipped node_modules: matches `node_modules` in `exclude`",
        "skipped Secrets: matches `secrets` in `exclude`",
        "skipped out: ignored by .gitignore",
        "skipped .obsidian: hidden",
    ] {
        assert!(printed.contains(line), "no {line:?} in:\n{printed}");
    }
    assert!(!printed.contains("skipped team-a"), "{printed}");
}

#[test]
fn init_writes_the_default_exclude_list() {
    let vault = bare();
    vault.ok(&["init"]);

    let exclude = config(&vault);
    let exclude = list(&exclude["exclude"]);
    for name in [
        "node_modules",
        ".git",
        ".obsidian",
        ".kbx",
        "credentials",
        "secrets",
        ".ssh",
        ".aws",
    ] {
        assert!(exclude.contains(&name), "{name} not in {exclude:?}");
    }
}

#[test]
fn rerunning_init_keeps_a_hand_edited_exclude() {
    let vault = bare();
    vault.ok(&["init"]);
    set_exclude(&vault, r#"["drafts"]"#);

    vault.ok(&["init"]);

    assert_eq!(list(&config(&vault)["exclude"]), ["drafts"]);
}

#[test]
fn the_index_leaves_out_excluded_folders_at_any_depth() {
    let vault = bare();
    vault.write("team-a/one.md", &note(1, "One"));
    vault.write("team-a/credentials/two.md", &note(2, "Two"));
    vault.write("team-a/deep/node_modules/three.md", &note(3, "Three"));
    vault.write("team-a/.ssh/four.md", &note(4, "Four"));
    vault.ok(&["init"]);

    vault.ok(&["index"]);

    assert_eq!(paths(&vault), ["team-a/one.md"]);
}

#[test]
fn the_index_reads_gitignore_files_with_or_without_a_repository() {
    let vault = bare();
    vault.write(".gitignore", "drafts/\n*.private.md\n");
    vault.write("a/one.md", &note(1, "One"));
    vault.write("a/drafts/two.md", &note(2, "Two"));
    vault.write("a/three.private.md", &note(3, "Three"));
    vault.write("b/.gitignore", "/*\n!/keep.md\n");
    vault.write("b/keep.md", &note(4, "Keep"));
    vault.write("b/drop.md", &note(5, "Drop"));
    vault.ok(&["init"]);

    vault.ok(&["index"]);

    assert_eq!(paths(&vault), ["a/one.md", "b/keep.md"]);
}

/// `/*` here is how one owner's vault keeps most of its notes out of git. That
/// says what git tracks, not what kbx may read.
#[test]
fn git_info_exclude_is_not_read() {
    let vault = bare();
    git(&vault, &["init", "-q"]);
    vault.write(".git/info/exclude", "/*\n");
    vault.write("a/one.md", &note(1, "One"));
    vault.ok(&["init"]);

    vault.ok(&["index"]);

    assert_eq!(paths(&vault), ["a/one.md"]);
    assert_eq!(list(&config(&vault)["scopes"]), ["a"]);
}

#[test]
fn a_vault_inside_a_repository_honours_the_gitignore_above_it() {
    let repository = bare();
    git(&repository, &["init", "-q"]);
    repository.write(".gitignore", "vault/private/\n");
    repository.write("vault/a/one.md", &note(1, "One"));
    repository.write("vault/private/two.md", &note(2, "Two"));
    let root = repository.root().join("vault");

    let printed = repository.path.kbx(&root, &["init"]);
    assert!(stdout(&printed).contains("skipped private"), "{printed:?}");
    repository.path.kbx(&root, &["index"]);

    let database = Connection::open(root.join(".kbx/graph.sqlite")).unwrap();
    let notes: i64 = database
        .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(notes, 1);
}

#[test]
fn a_gitignore_above_a_vault_outside_any_repository_means_nothing() {
    let parent = bare();
    parent.write(".gitignore", "*\n");
    parent.write("vault/a/one.md", &note(1, "One"));
    let root = parent.root().join("vault");

    parent.path.kbx(&root, &["init"]);
    parent.path.kbx(&root, &["index"]);

    let database = Connection::open(root.join(".kbx/graph.sqlite")).unwrap();
    let notes: i64 = database
        .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(notes, 1);
}

#[test]
fn a_folder_excluded_later_leaves_the_index() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.write("b/two.md", &note(2, "Two"));
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    assert_eq!(paths(&vault).len(), 2);

    set_exclude(&vault, r#"["b"]"#);
    vault.ok(&["index"]);

    assert_eq!(paths(&vault), ["a/one.md"]);
}

#[test]
fn naming_an_excluded_note_does_not_index_it_and_drops_what_was() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.write("b/two.md", &note(2, "Two"));
    vault.ok(&["init"]);
    vault.ok(&["index"]);

    set_exclude(&vault, r#"["b"]"#);
    vault.ok(&["index", "b/two.md"]);
    assert_eq!(paths(&vault), ["a/one.md"]);

    vault.ok(&["index", "b"]);
    vault.ok(&["index", "--full", "b/two.md"]);
    assert_eq!(paths(&vault), ["a/one.md"]);
}

/// The pre-commit hook lints what's staged, which `git add -f` can put here.
#[test]
fn lint_passes_over_an_excluded_note() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.write("credentials/bad.md", "---\nid: nope\n---\n");
    vault.ok(&["init"]);

    let printed = vault.ok(&["lint", "credentials/bad.md"]);
    assert!(!printed.contains("note.invalid"), "{printed}");

    let whole = vault.ok(&["lint"]);
    assert!(!whole.contains("credentials"), "{whole}");
}

#[test]
fn an_excluded_note_does_not_take_an_id_from_a_visible_one() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.write("credentials/copy.md", &note(1, "Copy"));
    vault.ok(&["init"]);

    let printed = vault.ok(&["lint", "a/one.md"]);

    assert!(!printed.contains("id.duplicate"), "{printed}");
}

#[test]
fn a_kbx_toml_without_exclude_gets_the_defaults_and_doctor_says_so() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.write("credentials/two.md", &note(2, "Two"));
    vault.ok(&["init"]);
    edit_config(&vault, |text| {
        let kept = text.lines().filter(|line| !line.starts_with("exclude ="));
        kept.collect::<Vec<_>>().join("\n") + "\n"
    });

    vault.ok(&["index"]);
    assert_eq!(paths(&vault), ["a/one.md"]);

    let printed = stdout(&vault.kbx(&["doctor"]));
    let row = printed.lines().find(|line| line.starts_with("exclude"));
    let row = row.unwrap_or_else(|| panic!("no exclude row in:\n{printed}"));
    assert!(row.contains("warn"), "{row}");
    assert!(row.contains("the defaults apply"), "{row}");
    assert!(row.contains("credentials"), "{row}");
}

#[test]
fn doctor_is_quiet_about_a_kbx_toml_with_exclude() {
    let vault = bare();
    vault.ok(&["init"]);

    let printed = stdout(&vault.kbx(&["doctor"]));

    let row = printed.lines().find(|line| line.starts_with("exclude"));
    let row = row.unwrap_or_else(|| panic!("no exclude row in:\n{printed}"));
    assert!(row.contains("ok"), "{row}");
}

#[test]
fn doctor_warns_about_a_scope_that_is_excluded() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.write("b/two.md", &note(2, "Two"));
    vault.ok(&["init"]);
    set_exclude(&vault, r#"["b"]"#);

    let printed = stdout(&vault.kbx(&["doctor"]));

    let row = printed.lines().find(|line| line.starts_with("exclude"));
    let row = row.unwrap_or_else(|| panic!("no exclude row in:\n{printed}"));
    assert!(row.contains("warn"), "{row}");
    assert!(row.contains("scope b"), "{row}");
}

/// Reading on without the rule would be the wrong way to fail.
#[test]
fn a_bad_exclude_pattern_stops_the_index() {
    let vault = bare();
    vault.write("a/one.md", &note(1, "One"));
    vault.ok(&["init"]);
    set_exclude(&vault, r#"["[z-a"]"#);

    let output = vault.kbx(&["index"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("[z-a"), "{}", stderr(&output));
}
