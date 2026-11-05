//! Drives `kbx accept --no-write`, and what `kbx review`, a later `kbx accept`,
//! `kbx reject`, `kbx defer` and `kbx suggest` make of its decisions.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{Fixture, stderr, stdout};
use rusqlite::Connection;
use serde_json::Value;

const SUGGEST: [&str; 5] = [
    "suggest",
    "--judge",
    "none",
    "--generators",
    "common_neighbors",
];

fn suggested(vault: Fixture) -> Fixture {
    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    vault
}

fn db(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

/// `(cid, src, dst)`, best score first.
fn pairs(vault: &Fixture) -> Vec<(String, String, String)> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT cid, src, dst FROM candidates ORDER BY score DESC, cid")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn first(vault: &Fixture) -> String {
    pairs(vault).remove(0).0
}

/// Two candidates with a note in common.
fn sharing(vault: &Fixture) -> (String, String) {
    let all = pairs(vault);
    let shares = |a: &(String, String, String), b: &(String, String, String)| {
        a.0 != b.0 && (a.1 == b.1 || a.1 == b.2 || a.2 == b.1 || a.2 == b.2)
    };
    for a in &all {
        if let Some(b) = all.iter().find(|b| shares(a, b)) {
            return (a.0.clone(), b.0.clone());
        }
    }
    panic!("no two candidates share a note");
}

/// Every note in the vault, by path from it.
fn notes(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if !path.ends_with(".kbx") {
                    walk(root, &path, found);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                found.insert(relative, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

/// Applies one file's unified diff, keeping every byte of its lines.
fn apply(before: &str, diff: &str) -> String {
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut at = 0;
    for line in diff.split_inclusive('\n').skip(2) {
        if let Some(hunk) = line.strip_prefix("@@ -") {
            let start = hunk.split([',', ' ']).next().unwrap();
            let start: usize = start.parse().unwrap();
            let upto = start.saturating_sub(1);
            out.extend(old[at..upto].iter().copied());
            at = upto;
            continue;
        }
        let (sign, text) = line.split_at(1);
        match sign {
            " " | "-" => {
                assert_eq!(old[at], text, "the diff doesn't fit the file");
                at += 1;
                if sign == " " {
                    out.push_str(text);
                }
            }
            "+" => out.push_str(text),
            other => panic!("not a diff line: {other}{text}"),
        }
    }
    out.extend(old[at..].iter().copied());
    out
}

/// What the printed diffs say each changed note will become.
fn applied(printed: &str, before: &BTreeMap<PathBuf, String>) -> BTreeMap<PathBuf, String> {
    let mut result = BTreeMap::new();
    for part in printed.split("--- a/").skip(1) {
        let (name, _) = part.split_once('\n').unwrap();
        let path = PathBuf::from(name.trim_end());
        let diff = format!("--- a/{part}");
        result.insert(path.clone(), apply(&before[&path], &diff));
    }
    result
}

fn changed(
    before: &BTreeMap<PathBuf, String>,
    after: &BTreeMap<PathBuf, String>,
) -> BTreeMap<PathBuf, String> {
    let differs = after.iter().filter(|(path, text)| before[*path] != **text);
    differs.map(|(p, t)| (p.clone(), t.clone())).collect()
}

/// Runs `--no-write` then the real thing, and holds the diffs to what was written.
fn assert_diff_is_the_write(vault: &Fixture, cids: &[&str], least: usize) {
    let before = notes(vault.root());
    let mut args = vec!["accept"];
    args.extend(cids);
    args.extend(["--rel", "see_also"]);
    let mut unwritten = args.clone();
    unwritten.push("--no-write");
    let printed = vault.ok(&unwritten);
    assert_eq!(notes(vault.root()), before, "--no-write touched a note");

    let promised = applied(&printed, &before);
    vault.ok(&args);
    let written = changed(&before, &notes(vault.root()));
    assert!(written.len() >= least, "{written:?}");
    assert_eq!(promised, written);
}

fn queue(vault: &Fixture) -> Vec<Value> {
    serde_json::from_str::<Value>(&vault.ok(&["review", "--json"]))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn item(vault: &Fixture, cid: &str) -> Value {
    let found = queue(vault).into_iter().find(|item| item["cid"] == cid);
    found.unwrap_or_else(|| panic!("{cid} is not in the queue"))
}

/// `(rel, decision, reason, strikes)` of every row in `reviews`.
fn reviews(vault: &Fixture) -> Vec<(String, String, Option<String>, i64)> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT rel, decision, reason, strikes FROM reviews ORDER BY rel")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn the_diff_is_byte_for_byte_what_accepting_writes() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    assert_diff_is_the_write(&vault, &[&cid], 2);
}

#[test]
fn a_crlf_note_keeps_its_line_endings_in_the_diff() {
    let vault = Fixture::new().writing();
    for (path, text) in notes(vault.root()) {
        let path = vault.root().join(path);
        fs::write(path, text.replace('\n', "\r\n")).unwrap();
    }
    let vault = suggested(vault);
    let cid = first(&vault);
    assert_diff_is_the_write(&vault, &[&cid], 2);
}

#[test]
fn the_diff_of_a_staged_candidate_takes_its_line_out() {
    let vault = Fixture::new().writing();
    vault.ok(&["index"]);
    let mut args = SUGGEST.to_vec();
    args.push("--stage");
    vault.ok(&args);
    let cid = first(&vault);
    let line = format!("id: {cid}");
    let held = notes(vault.root());
    assert!(held.values().any(|text| text.contains(&line)));
    assert_diff_is_the_write(&vault, &[&cid], 2);
    let after = notes(vault.root());
    assert!(!after.values().any(|text| text.contains(&line)));
}

#[test]
fn candidates_sharing_a_note_make_one_diff_for_it() {
    let vault = suggested(Fixture::new().writing());
    let (a, b) = sharing(&vault);
    assert_diff_is_the_write(&vault, &[&a, &b], 3);
}

#[test]
fn it_shows_the_inverse_as_the_other_notes_own_diff() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    let printed = vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    assert_eq!(printed.matches("--- a/").count(), 2, "{printed}");
    assert_eq!(printed.matches("+see_also").count(), 2, "{printed}");
}

#[test]
fn a_derived_inverse_leaves_the_other_note_out_of_the_diff() {
    let vault = Fixture::new().writing();
    let path = vault.root().join(".kbx/ontology.yaml");
    let yaml = fs::read_to_string(&path).unwrap();
    let marked = yaml.replace("weight: 1.0,", "weight: 1.0, derived_only: true,");
    assert_ne!(marked, yaml);
    fs::write(path, marked).unwrap();
    let vault = suggested(vault);
    let cid = first(&vault);
    let printed = vault.ok(&["accept", &cid, "--rel", "part_of", "--no-write"]);
    assert_eq!(printed.matches("--- a/").count(), 1, "{printed}");
    assert!(printed.contains("derived by `kbx index`"), "{printed}");
}

#[test]
fn review_shows_it_as_accepted_and_unwritten_and_a_plain_accept_writes_it() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    let other = pairs(&vault)[1].0.clone();
    let reason = "they belong together";
    vault.ok(&[
        "accept",
        &cid,
        "--rel",
        "see_also",
        "--reason",
        reason,
        "--no-write",
    ]);

    let shown = item(&vault, &cid);
    assert_eq!(shown["accepted_unwritten"], true);
    assert_eq!(shown["rel"], "see_also");
    assert!(item(&vault, &other).get("accepted_unwritten").is_none());
    let row = ("see_also".into(), "accept".into(), Some(reason.into()), 0);
    assert_eq!(reviews(&vault), vec![row.clone()]);

    let before = notes(vault.root());
    vault.ok(&["accept", &cid]);
    assert_eq!(changed(&before, &notes(vault.root())).len(), 2);
    assert!(queue(&vault).iter().all(|item| item["cid"] != cid.as_str()));
    assert_eq!(reviews(&vault), [row]);
}

#[test]
fn a_refused_candidate_records_nothing() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    let output = vault.kbx(&["accept", &cid, "--rel", "no_such", "--no-write"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("rel.unknown"),
        "{}",
        stderr(&output)
    );
    assert!(reviews(&vault).is_empty());
    assert!(item(&vault, &cid).get("accepted_unwritten").is_none());
}

#[test]
fn rejecting_takes_the_acceptance_back() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    let before = notes(vault.root());
    vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    vault.ok(&["reject", &cid]);
    let rows = reviews(&vault);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str(), rows[0].3),
        ("see_also", "reject", 1)
    );
    assert_eq!(notes(vault.root()), before);
}

#[test]
fn deferring_puts_it_back_in_the_queue_undecided() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    vault.ok(&["defer", &cid]);
    assert!(item(&vault, &cid).get("accepted_unwritten").is_none());
    let rows = reviews(&vault);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, "defer");
}

#[test]
fn another_relation_replaces_the_one_accepted() {
    let vault = suggested(Fixture::new().writing());
    let accepted = pairs(&vault).into_iter().find_map(|(cid, ..)| {
        let args = ["accept", &cid, "--rel", "depends_on", "--no-write"];
        vault.kbx(&args).status.success().then_some(cid)
    });
    let cid = accepted.expect("a pair that depends_on fits");
    vault.ok(&["accept", &cid, "--rel", "see_also"]);
    let rows = reviews(&vault);
    assert!(rows.iter().all(|row| row.0 != "depends_on"), "{rows:?}");
    assert!(rows.iter().any(|row| row.0 == "see_also"), "{rows:?}");
}

#[test]
fn an_inverse_relation_is_written_the_way_it_was_previewed() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    let before = notes(vault.root());
    let printed = vault.ok(&["accept", &cid, "--rel", "has_part", "--no-write"]);
    let promised = applied(&printed, &before);
    vault.ok(&["accept", &cid]);
    assert_eq!(promised, changed(&before, &notes(vault.root())));
    let rows = reviews(&vault);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "part_of");
}

#[test]
fn an_inverse_relation_is_taken_back_under_its_own_row() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    vault.ok(&["accept", &cid, "--rel", "has_part", "--no-write"]);
    vault.ok(&["accept", &cid, "--rel", "see_also"]);
    let rows = reviews(&vault);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "see_also");
}

#[test]
fn a_withdrawn_acceptance_goes_back_to_the_rejection_it_replaced() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    vault.ok(&["reject", &cid, "--rel", "part_of"]);
    vault.ok(&SUGGEST);
    vault.ok(&["accept", &cid, "--rel", "part_of", "--no-write"]);
    vault.ok(&["accept", &cid, "--rel", "see_also"]);
    let rows = reviews(&vault);
    let part_of = rows.iter().find(|row| row.0 == "part_of").unwrap();
    assert_eq!((part_of.1.as_str(), part_of.3), ("reject", 1), "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row.0 == "see_also" && row.1 == "accept")
    );
}

#[test]
fn suggest_keeps_the_decision_and_does_not_stage_it() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    let mut args = SUGGEST.to_vec();
    args.push("--stage");
    vault.ok(&args);
    assert_eq!(item(&vault, &cid)["accepted_unwritten"], true);
    let line = format!("id: {cid}");
    assert!(!notes(vault.root()).values().any(|t| t.contains(&line)));
    vault.ok(&["accept", &cid]);
}

#[test]
fn a_note_edited_since_is_refused_until_suggest_catches_up() {
    let vault = suggested(Fixture::new().writing());
    let (cid, src, _) = pairs(&vault).remove(0);
    vault.ok(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    let path: String = db(&vault)
        .query_row("SELECT path FROM files WHERE id = ?", [&src], |row| {
            row.get(0)
        })
        .unwrap();
    let path = vault.root().join(path);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}\nEdited.\n")).unwrap();

    let refused = vault.kbx(&["accept", &cid]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("changed since"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("{text}\nEdited.\n")
    );
    assert_eq!(item(&vault, &cid)["accepted_unwritten"], true);

    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    vault.ok(&["accept", &cid]);
    assert!(fs::read_to_string(&path).unwrap().contains("see_also"));
}

#[test]
fn review_json_reads_a_database_from_before_the_column() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    db(&vault)
        .execute_batch("ALTER TABLE candidates DROP COLUMN accepted_rel")
        .unwrap();
    assert!(item(&vault, &cid).get("accepted_unwritten").is_none());
}

#[test]
fn a_terminal_free_run_prints_the_diff_on_stdout() {
    let vault = suggested(Fixture::new().writing());
    let cid = first(&vault);
    let output = vault.kbx(&["accept", &cid, "--rel", "see_also", "--no-write"]);
    assert!(stdout(&output).contains(&format!("accepted {cid}, not written")));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
}
