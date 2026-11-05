//! Drives `kbx doctor` and the QMD install, with a fake `PATH` standing in for
//! Node and npm. No test here may reach a real `npm`.

mod common;

use std::fs;
use std::process::Output;

use common::{FakePath, Fixture, stderr, stdout};
use serde_json::Value;
use tempfile::TempDir;

const MANUAL: &str = "npm i -g @tobilu/qmd";

/// `npm` that logs its arguments, makes noise and installs `qmd` beside itself.
fn working_npm(path: &FakePath) {
    let script = if cfg!(windows) {
        "echo %* >> \"%~dp0npm.log\"\r\n\
         echo NPM-STDOUT-NOISE\r\n\
         echo NPM-STDERR-NOISE 1>&2\r\n\
         copy NUL \"%~dp0qmd.cmd\" > NUL"
    } else {
        "echo \"$@\" >> \"${0%/*}/npm.log\"\n\
         echo NPM-STDOUT-NOISE\n\
         echo NPM-STDERR-NOISE >&2\n\
         echo '#!/bin/sh' > \"${0%/*}/qmd\"\n\
         /bin/chmod +x \"${0%/*}/qmd\""
    };
    path.tool("node", "");
    path.tool("npm", script);
}

/// `npm` that fails as it does without write access to the prefix.
fn failing_npm(path: &FakePath) {
    let script = if cfg!(windows) {
        "echo npm error code EACCES\r\necho npm error path /usr/lib 1>&2\r\nexit /b 243"
    } else {
        "echo npm error code EACCES\necho npm error path /usr/lib >&2\nexit 243"
    };
    path.tool("node", "");
    path.tool("npm", script);
}

fn npm_calls(path: &FakePath) -> Vec<String> {
    let log = fs::read_to_string(path.dir().join("npm.log")).unwrap_or_default();
    log.lines().map(|line| line.trim().to_string()).collect()
}

/// The row a check printed.
fn row<'a>(output: &'a str, check: &str) -> &'a str {
    output
        .lines()
        .find(|line| line.split_whitespace().next() == Some(check))
        .unwrap_or_else(|| panic!("no `{check}` row in:\n{output}"))
}

fn status<'a>(output: &'a str, check: &str) -> &'a str {
    row(output, check).split_whitespace().nth(1).unwrap()
}

fn assert_other_checks_ran(output: &Output) {
    let text = stdout(output);
    assert_eq!(status(&text, "vault"), "ok", "{text}");
    assert_eq!(status(&text, "ontology"), "ok", "{text}");
    assert_eq!(status(&text, "index"), "ok", "{text}");
    assert_eq!(status(&text, "ram"), "ok", "{text}");
    assert_eq!(status(&text, "qmd"), "fail", "{text}");
}

/// An indexed fixture vault, moved onto `path` for the run under test.
fn indexed_with(path: FakePath) -> Fixture {
    let mut vault = Fixture::new();
    vault.ok(&["index"]);
    vault.path = path;
    vault
}

// --- checks -----------------------------------------------------------------

#[test]
fn a_healthy_vault_reports_every_check_and_exits_0() {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    let output = vault.ok(&["doctor"]);

    assert_eq!(status(&output, "vault"), "ok");
    assert_eq!(status(&output, "ontology"), "ok");
    assert_eq!(status(&output, "index"), "ok");
    assert!(row(&output, "index").contains("25 notes"), "{output}");
    assert_eq!(
        row(&output, "search").split_whitespace().nth(2),
        Some("qmd")
    );
    assert_eq!(status(&output, "qmd"), "ok");
    assert!(
        row(&output, "qmd").contains(vault.path.program("qmd").to_str().unwrap()),
        "{output}"
    );
    assert_eq!(status(&output, "ram"), "ok");
    assert!(row(&output, "ram").contains("GiB"), "{output}");
}

#[test]
fn outside_a_vault_the_machine_checks_still_run() {
    let path = FakePath::with_qmd();
    let dir = TempDir::new().unwrap();
    let output = path.kbx(dir.path(), &["doctor"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_eq!(status(&text, "vault"), "warn", "{text}");
    assert!(row(&text, "vault").contains("kbx init"), "{text}");
    assert_eq!(status(&text, "search"), "ok");
    assert_eq!(status(&text, "ram"), "ok");
}

#[test]
fn an_unbuilt_index_is_a_warning() {
    let vault = Fixture::new();
    let output = vault.ok(&["doctor"]);
    assert_eq!(status(&output, "index"), "warn", "{output}");
    assert!(row(&output, "index").contains("kbx index"), "{output}");
}

#[test]
fn an_index_with_no_notes_in_a_vault_with_markdown_is_a_warning() {
    let vault = Fixture::bare(FakePath::with_qmd());
    vault.ok(&["init", "--search", "qmd"]);
    vault.write("a.md", "---\nid: [\n---\n");
    vault.write("b/c.md", "---\nid: [\n---\n");
    assert!(!vault.kbx(&["index"]).status.success());

    let output = vault.kbx(&["doctor"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_eq!(status(&text, "index"), "warn", "{text}");
    assert!(row(&text, "index").contains("0 notes"), "{text}");
    assert!(row(&text, "index").contains("2 markdown files"), "{text}");
    assert!(row(&text, "index").contains("kbx index"), "{text}");
}

#[test]
fn an_empty_index_of_an_empty_vault_is_fine() {
    let vault = Fixture::bare(FakePath::with_qmd());
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    let output = vault.ok(&["doctor"]);
    assert_eq!(status(&output, "index"), "ok", "{output}");
}

#[test]
fn a_vault_of_only_excluded_markdown_is_a_warning_as_index_fails_on_it() {
    let vault = Fixture::bare(FakePath::with_qmd());
    vault.ok(&["init", "--search", "qmd"]);
    vault.write("node_modules/pkg/README.md", "Plain text.\n");
    assert!(!vault.kbx(&["index"]).status.success());

    let output = vault.ok(&["doctor"]);

    assert_eq!(status(&output, "index"), "warn", "{output}");
    assert!(
        row(&output, "index").contains("1 markdown file"),
        "{output}"
    );
}

#[test]
fn an_index_built_from_another_ontology_is_a_warning() {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    let file = vault.root().join(".kbx/ontology.yaml");
    let edited = format!("{}# edited\n", fs::read_to_string(&file).unwrap());
    fs::write(file, edited).unwrap();
    let output = vault.ok(&["doctor"]);
    assert_eq!(status(&output, "index"), "warn", "{output}");
    assert!(row(&output, "index").contains("kbx index"), "{output}");
}

#[test]
fn a_broken_ontology_fails_and_the_rest_still_reports() {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    fs::write(vault.root().join(".kbx/ontology.yaml"), "version: [").unwrap();
    let output = vault.kbx(&["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert_eq!(status(&text, "ontology"), "fail", "{text}");
    assert_eq!(status(&text, "index"), "ok", "{text}");
    assert_eq!(status(&text, "qmd"), "ok", "{text}");
}

#[test]
fn a_corrupt_database_fails_and_the_rest_still_reports() {
    let vault = Fixture::new();
    fs::write(
        vault.root().join(".kbx/graph.sqlite"),
        "not a database, at all",
    )
    .unwrap();
    let output = vault.kbx(&["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert_eq!(status(&text, "index"), "fail", "{text}");
    assert_eq!(status(&text, "qmd"), "ok", "{text}");
    assert_eq!(status(&text, "ram"), "ok", "{text}");
}

// --- installing QMD from doctor ---------------------------------------------

#[test]
fn doctor_installs_qmd_silently_and_finds_it_on_the_next_run() {
    let path = FakePath::empty();
    working_npm(&path);
    let vault = indexed_with(path);

    let first = vault.kbx(&["doctor"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(npm_calls(&vault.path), ["i -g @tobilu/qmd"]);
    assert_eq!(stderr(&first), "", "the install must be silent");
    assert!(!stdout(&first).contains("NOISE"), "{}", stdout(&first));
    assert_eq!(status(&stdout(&first), "qmd"), "ok");

    let second = vault.kbx(&["doctor"]);
    assert!(second.status.success());
    assert_eq!(status(&stdout(&second), "qmd"), "ok");
    assert_eq!(npm_calls(&vault.path).len(), 1, "qmd is already there");
}

#[test]
fn doctor_does_not_touch_npm_when_qmd_is_present() {
    let path = FakePath::with_qmd();
    working_npm(&path);
    let vault = indexed_with(path);
    vault.ok(&["doctor"]);
    assert_eq!(npm_calls(&vault.path), Vec::<String>::new());
}

#[test]
fn doctor_without_node_or_npm_names_node_22_and_keeps_reporting() {
    let vault = indexed_with(FakePath::empty());
    let output = vault.kbx(&["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("Node >= 22"),
        "{}",
        stderr(&output)
    );
    assert_other_checks_ran(&output);
}

#[test]
fn doctor_with_npm_but_no_node_names_node_22() {
    let path = FakePath::empty();
    path.tool("npm", "exit 0");
    let vault = indexed_with(path);
    let output = vault.kbx(&["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("Node >= 22"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_failed_install_prints_the_command_its_output_and_the_manual_line() {
    let path = FakePath::empty();
    failing_npm(&path);
    let vault = indexed_with(path);
    let output = vault.kbx(&["doctor"]);

    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains(&format!("`{MANUAL}` failed")), "{text}");
    assert!(
        text.contains("npm error code EACCES"),
        "stdout of npm: {text}"
    );
    assert!(
        text.contains("npm error path /usr/lib"),
        "stderr of npm: {text}"
    );
    assert!(text.lines().any(|line| line.trim() == MANUAL), "{text}");
    assert_other_checks_ran(&output);
}

#[test]
fn an_install_that_leaves_qmd_off_path_is_a_failure_not_a_silent_success() {
    let path = FakePath::empty();
    path.tool("node", "");
    path.tool("npm", "exit 0");
    let vault = indexed_with(path);
    let output = vault.kbx(&["doctor"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("not on PATH"), "{text}");
    assert!(text.contains("npm prefix -g"), "{text}");
    assert_other_checks_ran(&output);
}

// --- installing QMD from init -----------------------------------------------

#[test]
fn init_installs_qmd_silently() {
    let path = FakePath::empty();
    working_npm(&path);
    let dir = TempDir::new().unwrap();
    let output = path.kbx(dir.path(), &["init", "--search", "qmd"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(npm_calls(&path), ["i -g @tobilu/qmd"]);
    assert_eq!(stderr(&output), "");
    assert!(!stdout(&output).contains("NOISE"), "{}", stdout(&output));
}

#[test]
fn init_without_node_exits_non_zero_and_leaves_a_working_vault() {
    let path = FakePath::empty();
    let dir = TempDir::new().unwrap();
    let output = path.kbx(dir.path(), &["init", "--search", "qmd"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("Node >= 22"),
        "{}",
        stderr(&output)
    );
    assert!(dir.path().join("kbx.toml").is_file());
    assert!(dir.path().join(".kbx/graph.sqlite").is_file());
}

#[test]
fn init_with_a_failing_install_prints_the_same_report() {
    let path = FakePath::empty();
    failing_npm(&path);
    let dir = TempDir::new().unwrap();
    let output = path.kbx(dir.path(), &["init", "--search", "qmd"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains(&format!("`{MANUAL}` failed")), "{text}");
    assert!(text.contains("npm error code EACCES"), "{text}");
    assert!(text.lines().any(|line| line.trim() == MANUAL), "{text}");
}

// --- logging ----------------------------------------------------------------

#[test]
fn log_json_writes_one_object_per_line_to_stderr() {
    let vault = Fixture::new();
    let output = vault.kbx(&["--log", "json", "index"]);
    assert!(output.status.success());
    let text = stderr(&output);
    assert!(text.contains("bad-yaml"), "{text}");
    for line in text.lines() {
        let event: Value = serde_json::from_str(line).expect(line);
        assert_eq!(event["level"], "WARN", "{line}");
        assert!(event["fields"]["message"].is_string(), "{line}");
    }
}

#[test]
fn log_is_accepted_after_the_subcommand() {
    let vault = Fixture::new();
    let output = vault.kbx(&["index", "--log", "json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output)
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).is_ok())
    );
}
