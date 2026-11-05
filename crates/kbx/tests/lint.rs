//! Drives `kbx lint` over a copy of the fixture vault.

mod common;

use std::fs;
use std::process::Output;

use common::{Fixture, id, stderr, stdout};
use serde_json::Value;

const LINT: &str = "lint/01KBX0000000000000000000";

/// What the fixture vault's README promises: code, file and line.
fn expected() -> Vec<(String, String, u64)> {
    let lint = |suffix: &str| format!("{LINT}{suffix}.md");
    let rows = [
        (
            "schema.gap",
            "operations/01KBX000000000000000000010-restart-service.md".to_string(),
            3,
        ),
        ("type.unknown", lint("17-type-unknown"), 3),
        ("rel.unknown", lint("18-rel-unknown"), 5),
        ("rel.range", lint("19-rel-range"), 6),
        ("rel.cardinality", lint("20-rel-cardinality"), 5),
        ("link.unstable", lint("21-link-unstable"), 5),
        ("link.unresolved", lint("23-link-unresolved"), 5),
        ("link.unresolved", lint("23-link-unresolved"), 7),
        ("rel.domain", lint("24-rel-domain"), 5),
        ("note.invalid", lint("25-bad-yaml"), 5),
        ("note.invalid", lint("26-unterminated"), 1),
        ("id.duplicate", lint("27-id-duplicate-a"), 2),
        ("id.duplicate", lint("27-id-duplicate-b"), 2),
    ];
    let mut rows = rows
        .map(|(code, path, line)| (code.to_string(), path, line))
        .to_vec();
    rows.sort_by(|a, b| (&a.1, a.2).cmp(&(&b.1, b.2)));
    rows
}

fn severity_of(code: &str) -> &'static str {
    if code == "link.unstable" {
        "warning"
    } else {
        "error"
    }
}

fn findings(output: &Output) -> Vec<Value> {
    let report: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    report["findings"].as_array().unwrap().clone()
}

/// The findings of a `--format json` run, as (code, path, line).
fn places(output: &Output) -> Vec<(String, String, u64)> {
    let rows = findings(output);
    let row = |finding: &Value| {
        (
            finding["code"].as_str().unwrap().to_string(),
            finding["path"].as_str().unwrap().to_string(),
            finding["line"].as_u64().unwrap(),
        )
    };
    rows.iter().map(row).collect()
}

fn codes(output: &Output) -> Vec<String> {
    places(output).into_iter().map(|(code, ..)| code).collect()
}

#[test]
fn prints_every_code_at_its_location_and_exits_1() {
    let vault = Fixture::new();
    let output = vault.kbx(&["lint"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    for (code, path, line) in expected() {
        let severity = severity_of(&code);
        let place = format!("{severity}: {path}:{line}: {code}: ");
        let found = text.lines().any(|line| line.starts_with(&place));
        assert!(found, "{place} is missing from:\n{text}");
    }
    let lines = text.lines().filter(|line| line.contains(": ")).count();
    assert_eq!(lines, expected().len(), "{text}");
    assert_eq!(text.lines().last(), Some("12 errors, 1 warning"), "{text}");
}

#[test]
fn format_json_is_machine_parseable() {
    let vault = Fixture::new();
    let output = vault.kbx(&["lint", "--format", "json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(places(&output), expected());
    for finding in findings(&output) {
        assert!(!finding["message"].as_str().unwrap().is_empty());
        let code = finding["code"].as_str().unwrap();
        assert_eq!(finding["severity"], severity_of(code), "{finding}");
    }
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"], 12);
    assert_eq!(report["warnings"], 1);
}

#[test]
fn format_json_stays_parseable_with_logging_on() {
    let vault = Fixture::new();
    let output = vault.kbx(&["--log", "json", "lint", "--format", "json"]);
    assert_eq!(places(&output), expected());
}

#[test]
fn a_clean_vault_exits_0() {
    let vault = Fixture::new();
    let output = vault.kbx(&["lint", "engineering", "decisions"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(stdout(&output).contains("no findings"));

    let output = vault.kbx(&["lint", "--format", "json", "engineering"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(findings(&output), Vec::<Value>::new());
}

#[test]
fn only_warnings_exit_0() {
    let vault = Fixture::new();
    let given = format!("{LINT}21-link-unstable.md");
    let output = vault.kbx(&["lint", &given]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let text = stdout(&output);
    let prefix = format!("warning: {given}:5: link.unstable: ");
    assert!(text.starts_with(&prefix), "{text}");
    assert_eq!(text.lines().last(), Some("0 errors, 1 warning"), "{text}");

    let output = vault.kbx(&["lint", "--format", "json", &given]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["findings"][0]["severity"], "warning");
    assert_eq!(report["errors"], 0);
    assert_eq!(report["warnings"], 1);
}

#[test]
fn strict_promotes_warnings_to_errors() {
    let vault = Fixture::new();
    let given = format!("{LINT}21-link-unstable.md");
    let output = vault.kbx(&["lint", "--strict", &given]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let text = stdout(&output);
    let prefix = format!("error: {given}:5: link.unstable: ");
    assert!(text.starts_with(&prefix), "{text}");
    assert_eq!(text.lines().last(), Some("1 error, 0 warnings"), "{text}");

    let output = vault.kbx(&["lint", "--strict", "--format", "json", &given]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["findings"][0]["severity"], "error");
    assert_eq!(report["errors"], 1);
    assert_eq!(report["warnings"], 0);
}

#[test]
fn strict_changes_nothing_on_a_clean_vault() {
    let vault = Fixture::new();
    let output = vault.kbx(&["lint", "--strict", "engineering"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(stdout(&output).contains("no findings"));

    let output = vault.kbx(&["lint", "--format", "json", "engineering"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"], 0);
    assert_eq!(report["warnings"], 0);
}

#[test]
fn link_unresolved_is_reported_only_for_the_given_paths() {
    let vault = Fixture::new();
    let elsewhere = format!("{LINT}21-link-unstable.md");
    let output = vault.kbx(&["lint", "--format", "json", &elsewhere]);
    assert_eq!(codes(&output), ["link.unstable"]);

    let given = format!("{LINT}23-link-unresolved.md");
    let output = vault.kbx(&["lint", "--format", "json", &given]);
    assert_eq!(codes(&output), ["link.unresolved", "link.unresolved"]);
}

#[test]
fn a_given_note_is_still_checked_against_the_whole_vault() {
    let vault = Fixture::new();
    let given = format!("{LINT}19-rel-range.md");
    let output = vault.kbx(&["lint", "--format", "json", &given]);
    assert_eq!(
        codes(&output),
        ["rel.range"],
        "the target is outside the path"
    );
}

#[test]
fn a_duplicate_is_found_from_one_side_only() {
    let vault = Fixture::new();
    let given = format!("{LINT}27-id-duplicate-b.md");
    let output = vault.kbx(&["lint", "--format", "json", &given]);
    assert_eq!(codes(&output), ["id.duplicate"]);
    let finding = &findings(&output)[0];
    let message = finding["message"].as_str().unwrap();
    assert!(message.contains("27-id-duplicate-a.md"), "{message}");
}

#[test]
fn a_malformed_note_outside_the_paths_is_not_reported() {
    let vault = Fixture::new();
    let output = vault.kbx(&["lint", "--format", "json", "operations"]);
    assert_eq!(codes(&output), ["schema.gap"]);
}

#[test]
fn a_path_is_relative_to_the_working_directory() {
    let vault = Fixture::new();
    let output = vault.path.kbx(
        &vault.root().join("operations"),
        &["lint", "--format", "json", "."],
    );
    assert_eq!(codes(&output), ["schema.gap"]);
}

#[test]
fn a_bad_path_is_an_error_not_a_finding() {
    let vault = Fixture::new();
    let output = vault.kbx(&["lint", "no/such/note.md"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("does not exist"),
        "{}",
        stderr(&output)
    );

    let outside = tempfile::TempDir::new().unwrap();
    let output = vault.kbx(&["lint", outside.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("outside the vault"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn anything_the_index_fails_on_is_note_invalid() {
    let vault = Fixture::new();
    vault.write("scratch/list.md", "---\n- a\n---\nNot a mapping.\n");
    vault.write("scratch/unclosed.md", "---\nid: X\n");
    fs::write(vault.root().join("scratch/binary.md"), [0xff, 0xfe, 0x00]).unwrap();
    vault.write("scratch/plain.md", "No frontmatter, and still fine.\n");

    let indexed = vault.ok(&["index", "scratch"]);
    assert!(indexed.contains(", 3 failed"), "{indexed}");

    let output = vault.kbx(&["lint", "--format", "json", "scratch"]);
    assert_eq!(output.status.code(), Some(1));
    let row = |path: &str| ("note.invalid".to_string(), path.to_string(), 1);
    assert_eq!(
        places(&output),
        [
            row("scratch/binary.md"),
            row("scratch/list.md"),
            row("scratch/unclosed.md")
        ]
    );
}

#[test]
fn lint_reads_the_files_and_leaves_the_index_alone() {
    let vault = Fixture::new();
    let db = vault.root().join(".kbx/graph.sqlite");
    let before = fs::metadata(&db).unwrap().modified().unwrap();
    let output = vault.kbx(&["lint", "--format", "json"]);
    assert_eq!(places(&output), expected(), "nothing has been indexed");
    assert_eq!(fs::metadata(&db).unwrap().modified().unwrap(), before);
}

#[test]
fn lint_and_the_graph_agree_on_where_a_duplicate_id_points() {
    let vault = Fixture::new();
    let link = format!(
        "---\nid: {}\ntype: Concept\ntitle: Linker\nsee_also: [\"[[{}]]\", \"[[duplicate loser]]\"]\n---\n",
        id(30),
        id(27)
    );
    vault.write("scratch/linker.md", &link);
    vault.ok(&["index"]);

    let output = vault.kbx(&["lint", "--format", "json", "scratch"]);
    let rows = findings(&output);
    assert_eq!(codes(&output), ["link.unresolved"], "{rows:?}");
    assert!(
        rows[0]["message"]
            .as_str()
            .unwrap()
            .contains("duplicate loser")
    );

    let neighbors = vault.ok(&["graph", "neighbors", &id(30)]);
    assert!(
        neighbors.contains("Duplicate id, lowest path"),
        "{neighbors}"
    );
    assert!(!neighbors.contains("other path"), "{neighbors}");
}

#[test]
fn the_help_lists_every_code() {
    let vault = Fixture::new();
    let help = vault.ok(&["lint", "--help"]);
    for code in [
        "type.unknown",
        "rel.unknown",
        "rel.range",
        "rel.domain",
        "schema.gap",
        "rel.cardinality",
        "link.unstable",
        "link.unresolved",
        "note.invalid",
        "id.duplicate",
    ] {
        assert!(help.contains(code), "{code} is missing from:\n{help}");
    }
    let row = help.lines().find(|line| line.contains("link.unstable"));
    assert!(row.unwrap().contains("warning"), "{help}");
    assert!(help.contains("--strict"), "{help}");
}
