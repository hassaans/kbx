//! Drives the qmd integration against a stand-in that remembers its
//! collections and logs every call. The stand-in is a shell script, so this is
//! Unix only; the same paths run on Windows through `suggest.rs`.
#![cfg(unix)]

mod common;

use std::fs;

use common::{FakePath, Fixture, id, stderr, stdout};
use rusqlite::Connection;

/// A qmd that logs each call to `calls`, keeps collection names in
/// `collections`, and answers each search with one hit on `answer`. As the real
/// one does, it records a collection before reading its folder, so a failed add
/// leaves the name behind, and it stops at a mask over 10,000 characters. A file
/// `refuse-add-<folder>`, `refuse-remove-<name>` or `refuse-rename` makes that
/// step fail.
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
    while read -r name; do
        echo "$name (qmd://$name/)"
        echo "  Pattern:  $(/bin/cat "{dir}/mask-$name" 2>/dev/null || echo '**/*.md')"
        echo "  Files: 1"
    done < '{dir}/collections'
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
    echo "$7" > "{dir}/mask-$name"
    if [ "${{#7}}" -gt 10000 ]; then
        echo "Input length: ${{#7}}, exceeds maximum allowed length: 10000" >&2
        exit 1
    fi
    if [ -e "{dir}/refuse-add-$(/usr/bin/basename "$3")" ]; then
        echo 'no room left' >&2
        exit 1
    fi
    ;;
"collection remove")
    if [ -e "{dir}/refuse-remove-$3" ]; then
        echo 'index is locked' >&2
        exit 1
    fi
    /usr/bin/grep -vx "$3" '{dir}/collections' > '{dir}/collections.kept'
    /bin/mv '{dir}/collections.kept' '{dir}/collections'
    ;;
"collection rename")
    if [ -e '{dir}/refuse-rename' ]; then
        echo 'index is locked' >&2
        exit 1
    fi
    /usr/bin/sed "s/^$3\$/$4/" '{dir}/collections' > '{dir}/collections.kept'
    /bin/mv '{dir}/collections.kept' '{dir}/collections'
    /bin/mv "{dir}/mask-$3" "{dir}/mask-$4"
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
    let text = fs::read_to_string(vault.root().join(".kbx/kbx.toml")).unwrap();
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
    let out = vault.ok(&["init", "--search", "qmd"]);
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
    vault.ok(&["init", "--search", "qmd"]);
    let before = fs::read_to_string(vault.root().join(".kbx/kbx.toml")).unwrap();
    forget_calls(&vault);

    let output = vault.kbx(&["init", "--search", "qmd"]);
    assert!(output.status.success());
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    assert!(adds(&vault).is_empty(), "{:?}", calls(&vault));
    let after = fs::read_to_string(vault.root().join(".kbx/kbx.toml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn a_vault_without_scopes_gets_one_collection_at_its_root() {
    let vault = Fixture::bare(fake_qmd("[]"));
    vault.write(&format!("{}.md", id(1)), &note(1, "Flat."));
    vault.ok(&["init", "--search", "qmd"]);
    let config = config(&vault);
    assert_eq!(config["qmd"]["roots"]["vault"].as_str(), Some("."));
    assert_eq!(adds(&vault).len(), 1);
}

#[test]
fn scopes_with_one_slug_get_two_collections() {
    let vault = Fixture::bare(fake_qmd("[]"));
    vault.write("Team X/a.md", "plain");
    vault.write("team_x/b.md", "plain");
    vault.ok(&["init", "--search", "qmd"]);
    let config = config(&vault);
    let roots = config["qmd"]["roots"].as_table().unwrap();
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert_eq!(adds(&vault).len(), 2);
}

#[test]
fn a_scope_added_later_gets_its_collection_and_the_file_keeps_its_comments() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let scopes = "scopes = [\"Team B\", \"team-a\"]";
    assert!(text.contains(scopes), "{text}");
    let text = text.replace(scopes, "scopes = [\"Team B\", \"team-a\", \"team-c\"]");
    fs::write(&file, text).unwrap();
    fs::create_dir(vault.root().join("team-c")).unwrap();
    forget_calls(&vault);

    vault.ok(&["init", "--search", "qmd"]);
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
        ".kbx/kbx.toml",
        "vault = \".\"\nscopes = [\"Team B\", \"team-a\"]\nontology = \".kbx/ontology.yaml\"\n\
         search = \"qmd\"\nqmd = { bin = \"qmd\", collections = [\"Team B\", \"team-a\"] }\n",
    );
    vault.write(
        ".kbx/ontology.yaml",
        include_str!("../../../schema/ontology.example.yaml"),
    );
    vault.ok(&["init", "--search", "qmd"]);
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
    let output = vault.kbx(&["init", "--search", "qmd"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("kbx doctor --fix"),
        "{}",
        stderr(&output)
    );
    assert!(vault.root().join(".kbx/kbx.toml").is_file());
}

#[test]
fn doctor_reports_missing_collections_and_fix_creates_them() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
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
    vault.ok(&["init", "--search", "qmd"]);
    let file = vault.root().join(".kbx/kbx.toml");
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
    vault.ok(&["init", "--search", "qmd"]);
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
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    forget_calls(&vault);
    vault.ok(&["suggest"]);
    let log = calls(&vault);
    let commands: Vec<_> = log.iter().map(|c| c.split(' ').nth(2).unwrap()).collect();
    assert_eq!(commands[..3], ["status", "update", "embed"], "{log:?}");
    assert!(commands[3..].iter().all(|c| *c == "query"), "{log:?}");
    assert!(log[0].starts_with("--index kbx-"), "{log:?}");
}

#[test]
fn a_relative_qmd_bin_is_refreshed_like_one_on_path() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    let tools = vault.root().join("tools");
    fs::create_dir(&tools).unwrap();
    fs::rename(vault.path.program("qmd"), tools.join("qmd")).unwrap();
    let file = vault.root().join(".kbx/kbx.toml");
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
    vault.ok(&["init", "--search", "qmd"]);
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
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    let file = vault.root().join(".kbx/kbx.toml");
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
    vault.ok(&["init", "--search", "qmd"]);
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
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    let file = vault.root().join(".kbx/kbx.toml");
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
    vault.ok(&["init", "--search", "qmd"]);
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
        kbx_gen::MAX_SEARCHES + 3,
        "plus status, update and embed"
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

/// `search = "qmd"` is the path it always was: the same commands in the same
/// order, and nothing of the built-in engine's.
#[test]
fn search_qmd_shells_out_exactly_as_before() {
    let path = FakePath::empty();
    let calls = path.dir().join("calls");
    path.tool(
        "qmd",
        &format!(
            r#"echo "$*" >> "{}"
case "$3" in
query) echo '[]' ;;
esac"#,
            calls.display()
        ),
    );
    let vault = Fixture::bare(path);
    vault.write(
        "team-a/one.md",
        "---\nid: 01KBX000000000000000000001\ntype: Concept\ntitle: One\n---\n# One\n\nWords.\n",
    );
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    fs::remove_file(&calls).unwrap();

    vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);

    let log = fs::read_to_string(&calls).unwrap();
    let index = log.lines().next().unwrap().split(' ').nth(1).unwrap();
    let expected = [
        "status",
        "update",
        "embed",
        "query --json -n 20 -c team-a -- # One Words.",
    ]
    .map(|call| format!("--index {index} {call}"));
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines, expected, "{log}");
    assert!(!vault.root().join(".kbx/search-index.sqlite").exists());
}

fn removes(vault: &Fixture) -> Vec<String> {
    let calls = calls_of(vault, "collection");
    calls
        .into_iter()
        .filter(|c| c.contains(" remove "))
        .collect()
}

/// The mask of the last `collection add` for `name`, which a replacement reads
/// under another name first.
fn mask_of(vault: &Fixture, name: &str) -> String {
    let adds = adds(vault);
    let add = adds.iter().rfind(|call| {
        call.contains(&format!("--name {name} "))
            || call.contains(&format!("--name {name}-kbx-new "))
    });
    let add = add.unwrap_or_else(|| panic!("no collection {name} in {adds:?}"));
    add.rsplit_once("--mask ").unwrap().1.to_string()
}

/// The collections the fake qmd has now.
fn collections(vault: &Fixture) -> Vec<String> {
    let names = fs::read_to_string(vault.path.dir().join("collections")).unwrap();
    names.lines().map(str::to_string).collect()
}

/// Makes the step named by `file` fail from now on.
fn refuse(vault: &Fixture, file: &str) {
    fs::write(vault.path.dir().join(file), "").unwrap();
}

fn allow(vault: &Fixture, file: &str) {
    fs::remove_file(vault.path.dir().join(file)).unwrap();
}

fn row(printed: &str, name: &str) -> String {
    let row = printed.lines().find(|line| line.starts_with(name));
    let row = row.unwrap_or_else(|| panic!("no {name} row in:\n{printed}"));
    row.to_string()
}

#[test]
fn an_excluded_folder_gets_no_collection() {
    let vault = two_scopes("[]");
    vault.write("credentials/keys.md", &note(3, "keys"));
    vault.write("node_modules/pkg/README.md", "# A package\n");

    vault.ok(&["init", "--search", "qmd"]);

    let added = adds(&vault).join("\n");
    assert!(!added.contains("credentials"), "{added}");
    assert!(!added.contains("node_modules"), "{added}");
    let roots = config(&vault)["qmd"]["roots"].clone();
    assert!(roots.get("credentials").is_none(), "{roots}");
}

#[test]
fn a_collection_with_nothing_excluded_under_it_keeps_the_plain_mask() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);

    assert_eq!(mask_of(&vault, "team-a"), "**/*.md");
}

/// qmd has no ignore list of its own, so the mask has to go around the folder.
#[test]
fn a_mask_goes_around_a_folder_excluded_inside_a_scope() {
    let vault = two_scopes("[]");
    vault.write("team-a/credentials/keys.md", &note(3, "keys"));
    vault.write("team-a/sub/credentials/keys.md", &note(4, "keys"));
    vault.write("team-a/sub/deep/five.md", &note(5, "five"));
    vault.write("team-a/other/six.md", &note(6, "six"));

    vault.ok(&["init", "--search", "qmd"]);

    assert_eq!(
        mask_of(&vault, "team-a"),
        "{*.md,other/**/*.md,sub/*.md,sub/deep/**/*.md}"
    );
}

#[test]
fn a_whole_vault_collection_does_not_reach_an_excluded_folder() {
    let vault = Fixture::bare(fake_qmd("[]"));
    vault.write("one.md", &note(1, "one"));
    vault.write("credentials/keys.md", &note(2, "keys"));

    vault.ok(&["init", "--search", "qmd"]);

    assert_eq!(config(&vault)["scopes"].as_array().unwrap().len(), 0);
    assert_eq!(mask_of(&vault, "vault"), "*.md");
}

/// A vault with its scopes set up, then `include` added for a folder that
/// `.gitignore` kept out of them.
fn include_after_init(include: &str) -> Fixture {
    let vault = two_scopes("[]");
    vault.write(".gitignore", "personal/\n");
    vault.write("personal/journal/three.md", &note(3, "three"));
    vault.write("personal/taxes/four.md", &note(4, "four"));
    vault.ok(&["init", "--search", "qmd"]);
    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("include = {include}\n{text}")).unwrap();
    vault
}

#[test]
fn doctor_fix_makes_a_collection_for_a_folder_include_brings_back() {
    let vault = include_after_init(r#"["personal/journal"]"#);

    let printed = vault.ok(&["doctor"]);
    assert!(printed.contains("missing personal"), "{printed}");
    vault.ok(&["doctor", "--fix"]);

    assert_eq!(mask_of(&vault, "personal"), "journal/**/*.md");
    let roots = config(&vault)["qmd"]["roots"].clone();
    assert_eq!(roots["personal"].as_str(), Some("personal"), "{roots}");
    let scopes = config(&vault)["scopes"].clone();
    assert_eq!(scopes.as_array().unwrap().len(), 2, "{scopes}");
}

#[test]
fn a_folder_include_finds_nothing_in_gets_no_collection() {
    let vault = include_after_init(r#"["personal/typo"]"#);

    vault.ok(&["doctor", "--fix"]);

    let added = adds(&vault).join("\n");
    assert!(!added.contains("personal"), "{added}");
}

#[test]
fn a_mask_reaches_into_an_ignored_folder_inside_a_scope_as_far_as_include_does() {
    let vault = include_after_init(r#"["team-a/drafts/keep"]"#);
    vault.write(".gitignore", "personal/\ndrafts/\n");
    vault.write("team-a/drafts/keep/five.md", &note(5, "five"));
    vault.write("team-a/drafts/left/six.md", &note(6, "six"));
    vault.write("team-a/drafts/seven.md", &note(7, "seven"));

    let printed = vault.ok(&["doctor"]);
    assert!(printed.contains("mask out of date for team-a"), "{printed}");
    vault.ok(&["doctor", "--fix"]);

    assert_eq!(mask_of(&vault, "team-a"), "{*.md,drafts/keep/**/*.md}");
}

/// A vault set up before `exclude`, with its collections in qmd.
fn set_up_before_exclude() -> Fixture {
    let vault = two_scopes("[]");
    vault.write("credentials/keys.md", &note(3, "keys"));
    vault.write("team-a/secrets/keys.md", &note(4, "keys"));
    vault.ok(&["init", "--search", "qmd"]);
    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let text = text.replace(
        r#"qmd = { bin = "qmd", collections = ["#,
        r#"qmd = { bin = "qmd", collections = ["credentials", "#,
    );
    let text = text.replace("roots = { ", r#"roots = { credentials = "credentials", "#);
    let text = text.replace(r#"scopes = ["#, r#"scopes = ["credentials", "#);
    fs::write(&file, text).unwrap();
    let names = vault.path.dir().join("collections");
    let mut listed = fs::read_to_string(&names).unwrap();
    listed.push_str("credentials\n");
    fs::write(&names, listed).unwrap();
    fs::remove_file(vault.path.dir().join("mask-team-a")).unwrap();
    forget_calls(&vault);
    vault
}

#[test]
fn doctor_reports_collections_that_read_excluded_folders() {
    let vault = set_up_before_exclude();

    let printed = stdout(&vault.kbx(&["doctor"]));

    let row = printed.lines().find(|line| line.starts_with("collections"));
    let row = row.unwrap_or_else(|| panic!("no collections row in:\n{printed}"));
    assert!(row.contains("warn"), "{row}");
    assert!(row.contains("credentials"), "{row}");
    assert!(row.contains("team-a"), "{row}");
    assert!(row.contains("kbx doctor --fix"), "{row}");
    assert!(removes(&vault).is_empty());
}

#[test]
fn doctor_fix_removes_an_excluded_collection_and_narrows_the_rest() {
    let vault = set_up_before_exclude();

    vault.ok(&["doctor", "--fix"]);

    let removed = removes(&vault).join("\n");
    assert!(
        removed.contains("collection remove credentials"),
        "{removed}"
    );
    assert!(removed.contains("collection remove team-a"), "{removed}");
    assert_eq!(mask_of(&vault, "team-a"), "*.md");
    let qmd = config(&vault)["qmd"].clone();
    assert!(qmd["roots"].get("credentials").is_none(), "{qmd}");
    let names = qmd["collections"].as_array().unwrap();
    assert!(
        !names
            .iter()
            .any(|name| name.as_str() == Some("credentials"))
    );

    forget_calls(&vault);
    let printed = vault.ok(&["doctor", "--fix"]);
    assert!(removes(&vault).is_empty(), "{printed}");
}

#[test]
fn a_named_index_keeps_its_collections_but_kbx_stops_searching_an_excluded_one() {
    let vault = set_up_before_exclude();
    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace(r#"bin = "qmd","#, r#"bin = "qmd", index = "shared","#),
    )
    .unwrap();
    vault.ok(&["index"]);

    vault.ok(&["doctor", "--fix"]);
    assert!(removes(&vault).is_empty());

    forget_calls(&vault);
    vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);
    let queries = calls_of(&vault, "query").join("\n");
    assert!(!queries.contains("-c credentials"), "{queries}");
}

/// `count` folders in `team-a`, each with an excluded folder, as the vault that
/// found this had.
fn many_excluded_folders(count: u32) -> Fixture {
    let vault = two_scopes("[]");
    for n in 0..count {
        let folder = format!("team-a/company-with-a-long-name-{n:04}");
        vault.write(
            &format!("{folder}/secrets/keys.md"),
            &note(1000 + n, "keys"),
        );
        vault.write(
            &format!("{folder}/notes/{}.md", id(5000 + n)),
            &note(5000 + n, "x"),
        );
    }
    vault
}

#[test]
fn a_scope_with_many_excluded_folders_gets_a_collection_and_keeps_the_top_ones_out() {
    let vault = many_excluded_folders(150);
    vault.write("team-a/secrets/keys.md", &note(3, "keys"));

    let output = vault.kbx(&["init", "--search", "qmd"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(collections(&vault).contains(&"team-a".to_string()));
    let mask = mask_of(&vault, "team-a");
    assert!(mask.len() <= 6000, "{}", mask.len());
    assert!(
        mask.contains("company-with-a-long-name-0149/**/*.md"),
        "{mask}"
    );
    assert!(!mask.contains("secrets"), "{mask}");
    let warning = stderr(&output);
    assert!(warning.contains("team-a"), "{warning}");
    assert!(warning.contains("1 level"), "{warning}");
}

#[test]
fn a_scope_too_wide_for_any_narrower_mask_gets_all_its_notes() {
    let vault = many_excluded_folders(400);

    let output = vault.kbx(&["init", "--search", "qmd"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(mask_of(&vault, "team-a"), "**/*.md");
    assert!(stderr(&output).contains("team-a"), "{}", stderr(&output));
    assert_eq!(collections(&vault).len(), 2);
}

#[test]
fn doctor_says_which_mask_stops_short_and_leaves_it_alone() {
    let vault = many_excluded_folders(400);
    vault.ok(&["init", "--search", "qmd"]);
    forget_calls(&vault);

    let printed = vault.ok(&["doctor"]);

    let masks = row(&printed, "masks");
    assert!(masks.contains("warn"), "{masks}");
    assert!(masks.contains("team-a"), "{masks}");
    assert!(!masks.contains("team-b"), "{masks}");
    assert!(row(&printed, "collections").contains(" ok "), "{printed}");

    let printed = vault.ok(&["doctor", "--fix"]);
    assert!(row(&printed, "masks").contains("team-a"), "{printed}");
    assert!(adds(&vault).is_empty(), "the mask is already the right one");
    assert!(removes(&vault).is_empty());
}

#[test]
fn a_vault_with_short_masks_has_no_masks_row() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
    let printed = vault.ok(&["doctor"]);
    assert!(
        !printed.lines().any(|l| l.starts_with("masks")),
        "{printed}"
    );
}

#[test]
fn a_failed_add_goes_on_to_the_rest_and_leaves_nothing_behind() {
    let vault = two_scopes("[]");
    refuse(&vault, "refuse-add-team-a");

    let output = vault.kbx(&["init", "--search", "qmd"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(collections(&vault), ["team-b"]);
    assert!(stdout(&output).contains("created qmd collection team-b"));
    assert!(!stdout(&output).contains("created qmd collection team-a"));
    let warning = stderr(&output);
    assert!(warning.contains("team-a"), "{warning}");
    assert!(warning.contains("no room left"), "{warning}");
    assert!(warning.contains("kbx doctor --fix"), "{warning}");

    allow(&vault, "refuse-add-team-a");
    vault.ok(&["doctor", "--fix"]);
    assert_eq!(collections(&vault), ["team-b", "team-a"]);
}

#[test]
fn every_failure_is_reported_at_the_end() {
    let vault = two_scopes("[]");
    vault.write("team-c/note.md", &note(3, "c"));
    refuse(&vault, "refuse-add-team-a");
    refuse(&vault, "refuse-add-Team B");

    let output = vault.kbx(&["init", "--search", "qmd"]);

    assert_eq!(collections(&vault), ["team-c"]);
    let warning = stderr(&output);
    assert!(warning.contains("team-a"), "{warning}");
    assert!(warning.contains("team-b"), "{warning}");
}

/// `team-a` and `Team B`, both with a mask out of date, and an excluded
/// `credentials` collection.
fn two_masks_out_of_date() -> Fixture {
    let vault = set_up_before_exclude();
    vault.write("Team B/secrets/keys.md", &note(5, "keys"));
    fs::remove_file(vault.path.dir().join("mask-team-b")).unwrap();
    vault
}

#[test]
fn a_new_mask_is_read_before_the_old_collection_goes() {
    let vault = two_masks_out_of_date();

    vault.ok(&["doctor", "--fix"]);

    let log = calls_of(&vault, "collection");
    let at = |wanted: &str| {
        let found = log.iter().position(|call| call.contains(wanted));
        found.unwrap_or_else(|| panic!("no {wanted} in {log:?}"))
    };
    assert!(at("--name team-a-kbx-new") < at("collection remove team-a"));
    assert!(at("collection remove team-a") < at("collection rename team-a-kbx-new team-a"));
    let mut names = collections(&vault);
    names.sort();
    assert_eq!(names, ["team-a", "team-b"]);
    let mask = fs::read_to_string(vault.path.dir().join("mask-team-a")).unwrap();
    assert_eq!(mask.trim(), "*.md");
}

#[test]
fn a_failed_replacement_keeps_the_old_collection_and_the_loop_goes_on() {
    let vault = two_masks_out_of_date();
    refuse(&vault, "refuse-add-team-a");

    let output = vault.kbx(&["doctor", "--fix"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    let collection = row(&stdout(&output), "collections");
    assert!(collection.contains("fail"), "{collection}");
    assert!(collection.contains("team-a"), "{collection}");
    assert!(collection.contains("narrowed team-b"), "{collection}");
    assert!(
        stderr(&output).contains("no room left"),
        "{}",
        stderr(&output)
    );
    let mut names = collections(&vault);
    names.sort();
    assert_eq!(names, ["team-a", "team-b"]);
    assert!(!removes(&vault).iter().any(|c| c.ends_with(" team-a")));
    assert!(!vault.path.dir().join("mask-team-a").exists());
    assert_eq!(mask_of(&vault, "team-b"), "*.md");

    allow(&vault, "refuse-add-team-a");
    let printed = vault.ok(&["doctor", "--fix"]);
    assert!(
        row(&printed, "collections").contains("narrowed team-a"),
        "{printed}"
    );
}

#[test]
fn a_collection_left_with_a_mask_qmd_refused_is_replaced() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
    let mask = vault.path.dir().join("mask-team-a");
    fs::write(&mask, "x".repeat(20_000)).unwrap();
    forget_calls(&vault);

    vault.ok(&["doctor", "--fix"]);

    assert!(adds(&vault).iter().all(|add| add.len() < 10_000));
    assert_eq!(collections(&vault).len(), 2);
}

#[test]
fn a_collection_that_will_not_go_keeps_its_place_and_the_new_one_is_dropped() {
    let vault = two_masks_out_of_date();
    refuse(&vault, "refuse-remove-team-a");

    let output = vault.kbx(&["doctor", "--fix"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("index is locked"),
        "{}",
        stderr(&output)
    );
    let names = collections(&vault);
    assert!(names.contains(&"team-a".to_string()), "{names:?}");
    assert!(!names.contains(&"team-a-kbx-new".to_string()), "{names:?}");
    assert_eq!(mask_of(&vault, "team-b"), "*.md");
}

#[test]
fn an_excluded_collection_that_will_not_go_stays_in_kbx_toml() {
    let vault = two_masks_out_of_date();
    refuse(&vault, "refuse-remove-credentials");

    let output = vault.kbx(&["doctor", "--fix"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert_eq!(mask_of(&vault, "team-a"), "*.md");
    assert_eq!(mask_of(&vault, "team-b"), "*.md");
    let qmd = config(&vault)["qmd"].clone();
    assert!(qmd["roots"].get("credentials").is_some(), "{qmd}");

    allow(&vault, "refuse-remove-credentials");
    vault.ok(&["doctor", "--fix"]);
    assert!(config(&vault)["qmd"]["roots"].get("credentials").is_none());
}

#[test]
fn a_replacement_stranded_by_a_failed_rename_is_cleared_up_next_time() {
    let vault = two_masks_out_of_date();
    refuse(&vault, "refuse-rename");

    let output = vault.kbx(&["doctor", "--fix"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("team-a-kbx-new"),
        "{}",
        stderr(&output)
    );

    allow(&vault, "refuse-rename");
    vault.ok(&["doctor", "--fix"]);
    let mut names = collections(&vault);
    names.sort();
    assert_eq!(names, ["team-a", "team-b"]);
    let printed = vault.ok(&["doctor"]);
    assert!(row(&printed, "collections").contains(" ok "), "{printed}");
}

#[test]
fn doctor_without_fix_still_exits_zero_on_what_it_would_fix() {
    let vault = two_masks_out_of_date();
    refuse(&vault, "refuse-add-team-a");
    let printed = vault.ok(&["doctor"]);
    assert!(row(&printed, "collections").contains("warn"), "{printed}");
}

#[test]
fn doctor_fix_fails_when_qmd_cannot_list_its_collections() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
    vault.path.tool("qmd", "echo 'disk full' >&2; exit 1");

    let output = vault.kbx(&["doctor", "--fix"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(row(&stdout(&output), "collections").contains("fail"));
    assert!(vault.kbx(&["doctor"]).status.success());
}

#[test]
fn a_failed_add_in_a_named_index_leaves_the_rest_of_it_alone() {
    let vault = two_scopes("[]");
    vault.ok(&["init", "--search", "qmd"]);
    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace(r#"bin = "qmd","#, r#"bin = "qmd", index = "shared","#),
    )
    .unwrap();
    fs::write(vault.path.dir().join("collections"), "team-b\n").unwrap();
    refuse(&vault, "refuse-add-team-a");
    forget_calls(&vault);

    let output = vault.kbx(&["doctor", "--fix"]);

    assert!(!output.status.success(), "{}", stdout(&output));
    assert_eq!(collections(&vault), ["team-b"]);
    assert!(!removes(&vault).iter().any(|c| c.ends_with(" team-b")));
}
