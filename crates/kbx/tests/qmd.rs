//! Drives the qmd integration against a stand-in that remembers its
//! collections and logs every call. The stand-in is a shell script, so this is
//! Unix only; the same paths run on Windows through `suggest.rs`.
#![cfg(unix)]

mod common;

use std::fs;

use common::{FakePath, Fixture, id, stderr, stdout};
use rusqlite::Connection;

/// A qmd that logs each call to `calls`, keeps collection names in
/// `collections`, and answers each search with one hit on `answer`.
fn fake_qmd(answer: &str) -> FakePath {
    let path = FakePath::empty();
    let dir = path.dir().display().to_string();
    fs::write(path.dir().join("answer.json"), answer).unwrap();
    fs::write(path.dir().join("collections"), "").unwrap();
    path.tool(
        "qmd",
        &format!(
            r#"echo "$*" >> '{dir}/calls'
shift 2
case "$1 $2" in
"collection list")
    while read -r name; do echo "$name (qmd://$name/)"; echo "  Files: 1"; done < '{dir}/collections'
    ;;
"collection add")
    name=$5
    while read -r have; do
        if [ "$have" = "$name" ]; then
            echo "Collection '$name' already exists." >&2
            exit 1
        fi
    done < '{dir}/collections'
    echo "$name" >> '{dir}/collections'
    ;;
query*) /bin/cat '{dir}/answer.json' ;;
esac"#
        ),
    );
    path
}

fn calls(vault: &Fixture) -> Vec<String> {
    let log = fs::read_to_string(vault.path.dir().join("calls")).unwrap_or_default();
    log.lines().map(str::to_string).collect()
}

fn calls_of(vault: &Fixture, command: &str) -> Vec<String> {
    let wanted = |call: &&String| call.split(' ').nth(2) == Some(command);
    calls(vault).iter().filter(wanted).cloned().collect()
}

fn adds(vault: &Fixture) -> Vec<String> {
    let adds = calls_of(vault, "collection");
    adds.into_iter().filter(|c| c.contains(" add ")).collect()
}

fn forget_calls(vault: &Fixture) {
    let _ = fs::remove_file(vault.path.dir().join("calls"));
}

fn config(vault: &Fixture) -> toml::Value {
    let text = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    toml::from_str(&text).unwrap()
}

fn note(n: u32, body: &str) -> String {
    let id = id(n);
    format!("---\nid: {id}\ntype: Concept\ntitle: Note {n}\n---\n{body}\n")
}

/// A vault of `team-a/…1` and `Team B/…2`, not yet initialised.
fn two_scopes(answer: &str) -> Fixture {
    let vault = Fixture::bare(fake_qmd(answer));
    vault.write(&format!("team-a/{}.md", id(1)), &note(1, "Token buckets."));
    vault.write(&format!("Team B/{}.md", id(2)), &note(2, "Rate limits."));
    vault
}

fn hit_on(handle: &str) -> String {
    format!("[{{\"score\":0.75,\"file\":\"qmd://{handle}\",\"line\":6}}]")
}

fn knn_pairs(vault: &Fixture) -> Vec<(String, String)> {
    let db = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let mut statement = db
        .prepare("SELECT src, dst FROM candidates WHERE generators LIKE '%qmd_knn%'")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn candidate_count(vault: &Fixture) -> i64 {
    let db = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    db.query_row("SELECT count(*) FROM candidates", [], |row| row.get(0))
        .unwrap()
}

// --- collections ------------------------------------------------------------

#[test]
fn init_creates_a_collection_per_scope_and_records_its_root() {
    let vault = two_scopes("[]");
    let out = vault.ok(&["init"]);
    assert!(out.contains("created qmd collection team-a"), "{out}");

    let root = vault.root().canonicalize().unwrap();
    let adds = adds(&vault);
    assert_eq!(adds.len(), 2, "{adds:?}");
    let index = adds[0].split(' ').nth(1).unwrap();
    assert!(index.starts_with("kbx-"), "{index}");
    let want = format!(
        "--index {index} collection add {} --name team-a --mask **/*.md",
        root.join("team-a").display()
    );
    assert!(adds.contains(&want), "{adds:?}");
    let spaced = format!("{} --name team-b", root.join("Team B").display());
    assert!(adds.iter().any(|a| a.contains(&spaced)), "{adds:?}");

    let config = config(&vault);
    assert_eq!(config["qmd"]["roots"]["team-a"].as_str(), Some("team-a"));
    assert_eq!(config["qmd"]["roots"]["team-b"].as_str(), Some("Team B"));
    let names = config["qmd"]["collections"].as_array().unwrap();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn rerunning_init_duplicates_nothing() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    let before = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    forget_calls(&vault);

    let output = vault.kbx(&["init"]);
    assert!(output.status.success());
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    assert!(adds(&vault).is_empty(), "{:?}", calls(&vault));
    let after = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn a_vault_without_scopes_gets_one_collection_at_its_root() {
    let vault = Fixture::bare(fake_qmd("[]"));
    vault.write(&format!("{}.md", id(1)), &note(1, "Flat."));
    vault.ok(&["init"]);
    let config = config(&vault);
    assert_eq!(config["qmd"]["roots"]["vault"].as_str(), Some("."));
    assert_eq!(adds(&vault).len(), 1);
}

#[test]
fn scopes_with_one_slug_get_two_collections() {
    let vault = Fixture::bare(fake_qmd("[]"));
    vault.write("Team X/a.md", "plain");
    vault.write("team_x/b.md", "plain");
    vault.ok(&["init"]);
    let config = config(&vault);
    let roots = config["qmd"]["roots"].as_table().unwrap();
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert_eq!(adds(&vault).len(), 2);
}

#[test]
fn a_scope_added_later_gets_its_collection_and_the_file_keeps_its_comments() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let scopes = "scopes = [\"Team B\", \"team-a\"]";
    assert!(text.contains(scopes), "{text}");
    let text = text.replace(scopes, "scopes = [\"Team B\", \"team-a\", \"team-c\"]");
    fs::write(&file, text).unwrap();
    fs::create_dir(vault.root().join("team-c")).unwrap();
    forget_calls(&vault);

    vault.ok(&["init"]);
    assert_eq!(adds(&vault).len(), 1, "{:?}", adds(&vault));
    assert!(adds(&vault)[0].contains("--name team-c"));
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("# top-level folders = ACL scopes"), "{text}");
    assert!(text.contains("[judge.overrides]"), "{text}");
}

#[test]
fn a_kbx_toml_from_before_roots_is_brought_up_to_date() {
    let vault = two_scopes("[]");
    vault.write(
        "kbx.toml",
        "vault = \".\"\nscopes = [\"Team B\", \"team-a\"]\nontology = \".kbx/ontology.yaml\"\n\
         search = \"qmd\"\nqmd = { bin = \"qmd\", collections = [\"Team B\", \"team-a\"] }\n",
    );
    vault.write(
        ".kbx/ontology.yaml",
        include_str!("../../../schema/ontology.example.yaml"),
    );
    vault.ok(&["init"]);
    let config = config(&vault);
    let names: Vec<_> = config["qmd"]["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(names, ["team-a", "team-b"]);
    assert_eq!(config["qmd"]["roots"]["team-b"].as_str(), Some("Team B"));
}

#[test]
fn a_qmd_that_cannot_add_collections_leaves_init_working_and_says_so() {
    let vault = two_scopes("[]");
    vault.path.tool("qmd", "echo 'disk full' >&2; exit 1");
    let output = vault.kbx(&["init"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("kbx doctor --fix"),
        "{}",
        stderr(&output)
    );
    assert!(vault.root().join("kbx.toml").is_file());
}

#[test]
fn doctor_reports_missing_collections_and_fix_creates_them() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    fs::write(vault.path.dir().join("collections"), "team-a\n").unwrap();
    forget_calls(&vault);

    let out = vault.ok(&["doctor"]);
    let row = out.lines().find(|l| l.starts_with("collections")).unwrap();
    assert!(row.contains("warn"), "{row}");
    assert!(row.contains("team-b"), "{row}");
    assert!(!row.contains("team-a"), "{row}");
    assert!(row.contains("kbx doctor --fix"), "{row}");
    assert!(adds(&vault).is_empty(), "plain doctor changes nothing");

    let out = vault.ok(&["doctor", "--fix"]);
    let row = out.lines().find(|l| l.starts_with("collections")).unwrap();
    assert!(row.contains("created team-b"), "{row}");
    assert_eq!(adds(&vault).len(), 1);

    let out = vault.ok(&["doctor"]);
    let row = out.lines().find(|l| l.starts_with("collections")).unwrap();
    assert!(row.contains(" ok "), "{row}");
}

#[test]
fn doctor_warns_about_a_collection_with_no_root() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let text = text.replace("collections = [", "collections = [\"mine\", ");
    fs::write(&file, text).unwrap();
    let out = vault.ok(&["doctor"]);
    let row = out.lines().find(|l| l.starts_with("collections")).unwrap();
    assert!(row.contains("warn") && row.contains("mine"), "{row}");
}

// --- suggest ----------------------------------------------------------------

#[test]
fn a_fresh_vault_gets_qmd_knn_candidates() {
    let handle = format!("team-b/{}.md", id(2).to_lowercase());
    let vault = two_scopes(&hit_on(&handle));
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let output = vault.kbx(&["suggest", "--judge", "none"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stderr(&output).contains("skipped"), "{}", stderr(&output));
    assert!(stdout(&output).contains("qmd_knn 1"), "{}", stdout(&output));
    assert_eq!(knn_pairs(&vault), [(id(1), id(2))]);
}

#[test]
fn suggest_refreshes_the_vaults_own_index_before_searching() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    forget_calls(&vault);
    vault.ok(&["suggest"]);
    let log = calls(&vault);
    let commands: Vec<_> = log.iter().map(|c| c.split(' ').nth(2).unwrap()).collect();
    assert_eq!(commands[..2], ["update", "embed"], "{log:?}");
    assert!(commands[2..].iter().all(|c| *c == "query"), "{log:?}");
    assert!(log[0].starts_with("--index kbx-"), "{log:?}");
}

#[test]
fn a_relative_qmd_bin_is_refreshed_like_one_on_path() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let tools = vault.root().join("tools");
    fs::create_dir(&tools).unwrap();
    fs::rename(vault.path.program("qmd"), tools.join("qmd")).unwrap();
    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace("bin = \"qmd\"", "bin = \"./tools/qmd\""),
    )
    .unwrap();
    forget_calls(&vault);

    vault.ok(&["suggest"]);
    assert_eq!(calls_of(&vault, "update").len(), 1, "{:?}", calls(&vault));
    assert_eq!(calls_of(&vault, "embed").len(), 1, "{:?}", calls(&vault));
}

#[test]
fn a_dry_run_and_other_generators_do_not_touch_the_qmd_index() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    forget_calls(&vault);
    vault.ok(&["suggest", "--dry-run"]);
    vault.ok(&["suggest", "--generators", "schema_gap"]);
    assert!(calls_of(&vault, "update").is_empty());
    assert!(calls_of(&vault, "embed").is_empty());
}

#[test]
fn an_index_named_in_kbx_toml_is_searched_but_never_updated() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let text = text.replace("bin = \"qmd\",", "bin = \"qmd\", index = \"index\",");
    fs::write(&file, text).unwrap();
    forget_calls(&vault);
    vault.ok(&["suggest"]);
    assert!(calls_of(&vault, "update").is_empty());
    assert!(calls_of(&vault, "embed").is_empty());
    let queries = calls_of(&vault, "query");
    assert!(queries[0].starts_with("--index index query"), "{queries:?}");
}

#[test]
fn a_failed_refresh_warns_and_the_search_still_runs() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let dir = vault.path.dir().display().to_string();
    vault.path.tool(
        "qmd",
        &format!(
            "echo \"$*\" >> '{dir}/calls'\n\
             case \"$3\" in update) echo nope >&2; exit 1 ;; query) echo '[]' ;; esac"
        ),
    );
    forget_calls(&vault);
    let output = vault.kbx(&["suggest"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("may be out of date"),
        "{}",
        stderr(&output)
    );
    assert!(!calls_of(&vault, "query").is_empty());
}

#[test]
fn with_no_collections_qmd_knn_is_skipped_without_running_qmd() {
    let vault = two_scopes("[]");
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let file = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let start = text.find("qmd = {").unwrap();
    let end = start + text[start..].find('\n').unwrap();
    let mut text = text;
    text.replace_range(start..end, "qmd = { bin = \"qmd\" }");
    fs::write(&file, text).unwrap();
    forget_calls(&vault);

    let output = vault.kbx(&["suggest"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("qmd_knn skipped"),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("kbx init"), "{}", stderr(&output));
    assert!(calls(&vault).is_empty(), "{:?}", calls(&vault));
}

// --- the process budget -----------------------------------------------------

/// 120 unlinked notes of two sections each in one scope, indexed.
fn hundred_and_twenty(answer: &str) -> Fixture {
    let vault = Fixture::bare(fake_qmd(answer));
    for n in 1..=120 {
        let text = note(
            n,
            &format!("# One\nFirst words {n}.\n\n# Two\nMore words {n}."),
        );
        vault.write(&format!("crowd/{}.md", id(n)), &text);
    }
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    forget_calls(&vault);
    vault
}

#[test]
fn a_120_note_vault_starts_a_bounded_number_of_qmd_processes() {
    let handle = format!("crowd/{}.md", id(120).to_lowercase());
    let vault = hundred_and_twenty(&hit_on(&handle));
    let output = vault.kbx(&["suggest", "--generators", "qmd_knn"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(calls_of(&vault, "query").len(), kbx_gen::MAX_SEARCHES);
    assert_eq!(
        calls(&vault).len(),
        kbx_gen::MAX_SEARCHES + 2,
        "plus update and embed"
    );
    assert!(
        stderr(&output).contains("stopped at"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn budget_limits_the_candidates_and_the_searches() {
    let handle = format!("crowd/{}.md", id(120).to_lowercase());
    let vault = hundred_and_twenty(&hit_on(&handle));
    vault.ok(&["suggest", "--generators", "qmd_knn", "--budget", "5"]);
    assert_eq!(calls_of(&vault, "query").len(), 5);
    assert_eq!(candidate_count(&vault), 5);
}
