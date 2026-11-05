//! `.kbx/search-cache.sqlite` against a qmd stand-in that logs every call, so
//! that embedding work can be counted. See docs/embedding-cache.md. The
//! stand-in is a shell script, so this is Unix only.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{FakePath, Fixture, id, stderr};
use rusqlite::Connection;

const STATUS: &str = "\
QMD Status

Documents
  Total:    2 files indexed
  Vectors:  2 embedded
  Updated:  1s ago

Models
  Embedding:   https://huggingface.co/ggml-org/embeddinggemma-300M-GGUF
  Reranking:   https://huggingface.co/ggml-org/Qwen3-Reranker-0.6B-Q8_0-GGUF
  Generation:  https://huggingface.co/tobil/qmd-query-expansion-1.7B-gguf
";

/// A qmd that logs each call to `calls`, prints the file `status` if there is
/// one, answers each search from `answer.json`, and keeps its index under
/// `cache/qmd`, as the real one does under `XDG_CACHE_HOME`.
fn fake_qmd() -> FakePath {
    let path = FakePath::empty();
    let dir = path.dir().display().to_string();
    fs::write(path.dir().join("status"), STATUS).unwrap();
    fs::create_dir_all(path.dir().join("cache/qmd")).unwrap();
    fs::write(path.dir().join("cache/qmd/index.sqlite"), "qmd's own").unwrap();
    path.tool(
        "qmd",
        &format!(
            r#"echo "$*" >> '{dir}/calls'
shift 2
case "$1" in
status) /bin/cat '{dir}/status' ;;
query) /bin/cat '{dir}/answer.json' ;;
update) [ ! -f '{dir}/update-fails' ] ;;
esac"#
        ),
    );
    path
}

/// `team-a/…1` and `team-b/…2`, indexed, every search answering with `…2`.
fn vault() -> Fixture {
    let vault = Fixture::bare(fake_qmd());
    vault.write(&note_path(1), &note(1, "Token buckets."));
    vault.write(&note_path(2), &note(2, "Rate limits."));
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    answer_with_note_2(&vault);
    forget_calls(&vault);
    vault
}

/// The same folders with nothing kbx can index in them, as on `~/HQ`.
fn unindexed_vault() -> Fixture {
    let vault = Fixture::bare(fake_qmd());
    vault.write(&note_path(1), "No frontmatter.\n");
    vault.write(&note_path(2), "None here either.\n");
    vault.ok(&["init", "--search", "qmd"]);
    vault.ok(&["index"]);
    forget_calls(&vault);
    vault
}

fn answer_with_note_2(vault: &Fixture) {
    let hit = format!(
        "[{{\"score\":0.75,\"file\":\"qmd://team-b/{}.md\",\"line\":6}}]",
        id(2).to_lowercase()
    );
    fs::write(vault.path.dir().join("answer.json"), hit).unwrap();
}

fn note_path(n: u32) -> String {
    let folder = if n == 1 { "team-a" } else { "team-b" };
    format!("{folder}/{}.md", id(n))
}

fn note(n: u32, body: &str) -> String {
    let id = id(n);
    format!("---\nid: {id}\ntype: Concept\ntitle: Note {n}\n---\n{body}\n")
}

fn suggest(vault: &Fixture) {
    vault.ok(&["suggest", "--judge", "none"]);
}

fn count(vault: &Fixture, command: &str) -> usize {
    let log = fs::read_to_string(vault.path.dir().join("calls")).unwrap_or_default();
    let of = |call: &&str| call.split(' ').nth(2) == Some(command);
    log.lines().filter(of).count()
}

/// Everything that makes qmd do embedding work: `embed` over the notes,
/// `query` over a query, and `update`, which is what gives `embed` something.
fn embedding_calls(vault: &Fixture) -> usize {
    ["update", "embed", "query"]
        .iter()
        .map(|command| count(vault, command))
        .sum()
}

fn forget_calls(vault: &Fixture) {
    let _ = fs::remove_file(vault.path.dir().join("calls"));
}

fn knn_pairs(vault: &Fixture) -> Vec<(String, String, f64)> {
    let db = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let mut statement = db
        .prepare(
            "SELECT src, dst, score FROM candidates
             WHERE generators LIKE '%qmd_knn%' ORDER BY src, dst",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn cache_file(vault: &Fixture) -> std::path::PathBuf {
    vault.root().join(".kbx/search-cache.sqlite")
}

#[test]
fn a_second_suggest_on_an_unchanged_vault_issues_no_embedding_work() {
    let vault = vault();
    suggest(&vault);
    assert_eq!(count(&vault, "query"), 2);
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "embed"), 1);
    let first = knn_pairs(&vault);
    assert_eq!(first.len(), 1, "{first:?}");

    forget_calls(&vault);
    suggest(&vault);
    assert_eq!(embedding_calls(&vault), 0);
    assert_eq!(knn_pairs(&vault), first, "the cache changes no candidate");
}

#[test]
fn an_empty_graph_issues_no_embedding_work_until_it_has_notes() {
    let vault = unindexed_vault();
    let said = vault.ok(&["suggest", "--judge", "none"]);
    assert!(said.contains("the graph has no notes"), "{said}");
    vault.ok(&["suggest", "--judge", "none", "--dry-run"]);
    assert_eq!(embedding_calls(&vault), 0);

    vault.write(&note_path(1), &note(1, "Token buckets."));
    vault.write(&note_path(2), &note(2, "Rate limits."));
    vault.ok(&["index"]);
    answer_with_note_2(&vault);
    let said = vault.ok(&["suggest", "--judge", "none"]);
    assert!(!said.contains("the graph has no notes"), "{said}");
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "embed"), 1);
    assert_eq!(count(&vault, "query"), 2);
}

#[test]
fn notes_outside_every_collection_issue_no_embedding_work() {
    let vault = vault();
    let config = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    let moved = config
        .replace("= \"team-a\"", "= \"archive-a\"")
        .replace("= \"team-b\"", "= \"archive-b\"");
    assert_ne!(moved, config);
    vault.write("kbx.toml", &moved);

    let said = vault.ok(&["suggest", "--judge", "none"]);
    assert!(said.contains("no note is in a searched folder"), "{said}");
    assert_eq!(embedding_calls(&vault), 0);
}

#[test]
fn a_run_that_searches_nothing_does_not_mention_it() {
    let vault = unindexed_vault();
    let said = vault.ok(&["suggest", "--judge", "none", "--generators", "adamic_adar"]);
    assert!(!said.contains("skipped the search"), "{said}");
}

#[test]
fn editing_the_note_that_searched_repeats_only_its_search() {
    let vault = vault();
    suggest(&vault);
    forget_calls(&vault);

    vault.write(&note_path(1), &note(1, "Leaky buckets now."));
    vault.ok(&["index"]);
    suggest(&vault);
    assert_eq!(count(&vault, "query"), 1);
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "embed"), 1);
}

#[test]
fn editing_a_note_that_was_hit_repeats_every_search_that_found_it() {
    let vault = vault();
    suggest(&vault);
    forget_calls(&vault);

    vault.write(&note_path(2), &note(2, "Rate limits, rewritten."));
    vault.ok(&["index"]);
    suggest(&vault);
    assert_eq!(count(&vault, "query"), 2);
}

#[test]
fn a_frontmatter_edit_refreshes_qmd_but_repeats_no_search() {
    let vault = vault();
    suggest(&vault);
    forget_calls(&vault);

    let text = note(2, "Rate limits.").replace("title:", "tags: [limits]\ntitle:");
    vault.write(&note_path(2), &text);
    vault.ok(&["index"]);
    suggest(&vault);
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "query"), 0);
}

#[test]
fn another_qmd_model_repeats_the_searches() {
    let vault = vault();
    suggest(&vault);
    forget_calls(&vault);

    let status = STATUS.replace("embeddinggemma-300M-GGUF", "Qwen3-Embedding-0.6B-GGUF");
    fs::write(vault.path.dir().join("status"), status).unwrap();
    suggest(&vault);
    assert_eq!(count(&vault, "query"), 2);
}

#[test]
fn a_qmd_that_names_no_model_is_never_cached() {
    let vault = vault();
    fs::remove_file(vault.path.dir().join("status")).unwrap();
    suggest(&vault);
    forget_calls(&vault);

    suggest(&vault);
    assert_eq!(count(&vault, "query"), 2);
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "embed"), 1);
    assert!(!cache_file(&vault).exists());
}

#[test]
fn an_index_qmd_lost_is_refreshed_even_on_an_unchanged_vault() {
    let vault = vault();
    suggest(&vault);
    forget_calls(&vault);

    let status = STATUS.replace("2 embedded", "0 embedded");
    fs::write(vault.path.dir().join("status"), status).unwrap();
    suggest(&vault);
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "embed"), 1);
}

#[test]
fn a_named_index_that_changed_repeats_the_searches() {
    let vault = vault();
    let config = fs::read_to_string(vault.root().join("kbx.toml")).unwrap();
    let named = config.replace(
        "qmd = { bin = \"qmd\",",
        "qmd = { index = \"shared\", bin = \"qmd\",",
    );
    assert_ne!(named, config);
    vault.write("kbx.toml", &named);
    suggest(&vault);
    forget_calls(&vault);

    suggest(&vault);
    assert_eq!(count(&vault, "query"), 0);

    let status = STATUS.replace("2 embedded", "0 embedded");
    fs::write(vault.path.dir().join("status"), status).unwrap();
    suggest(&vault);
    assert_eq!(count(&vault, "query"), 2);
    assert_eq!(count(&vault, "update"), 0, "a named index is never updated");
}

#[test]
fn a_failed_refresh_is_tried_again_and_its_searches_are_not_kept() {
    let vault = vault();
    fs::write(vault.path.dir().join("update-fails"), "").unwrap();
    let output = vault.kbx(&["suggest", "--judge", "none"]);
    assert!(stderr(&output).contains("may be out of date"));
    fs::remove_file(vault.path.dir().join("update-fails")).unwrap();
    forget_calls(&vault);

    suggest(&vault);
    assert_eq!(count(&vault, "update"), 1);
    assert_eq!(count(&vault, "embed"), 1);
    assert_eq!(count(&vault, "query"), 2, "what it found then isn't kept");
}

#[test]
fn a_dry_run_reads_the_cache_and_creates_nothing() {
    let vault = vault();
    vault.ok(&["suggest", "--judge", "none", "--dry-run"]);
    assert_eq!(count(&vault, "query"), 2);
    assert!(!cache_file(&vault).exists());

    suggest(&vault);
    forget_calls(&vault);
    vault.ok(&["suggest", "--judge", "none", "--dry-run"]);
    assert_eq!(embedding_calls(&vault), 0);
}

#[test]
fn a_damaged_cache_is_replaced() {
    let vault = vault();
    fs::write(cache_file(&vault), "not a database, not even close").unwrap();
    suggest(&vault);
    assert_eq!(knn_pairs(&vault).len(), 1);
    forget_calls(&vault);

    suggest(&vault);
    assert_eq!(embedding_calls(&vault), 0);
}

/// Every file under `dir` and its bytes.
fn contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(contents(&path));
        } else {
            files.insert(path.display().to_string(), fs::read(&path).unwrap());
        }
    }
    files
}

#[test]
fn kbx_writes_nothing_into_qmds_index_directory() {
    let vault = vault();
    let cache = vault.path.dir().join("cache");
    let before = contents(&cache);
    let env = [("XDG_CACHE_HOME", cache.to_str().unwrap())];

    let args = ["suggest", "--judge", "none"];
    for _ in 0..2 {
        let output = vault.kbx_with(&args, &env);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    vault.write(&note_path(1), &note(1, "Leaky buckets now."));
    vault.ok(&["index"]);
    let output = vault.kbx_with(&args, &env);
    assert!(output.status.success(), "{}", stderr(&output));

    assert_eq!(contents(&cache), before);
}
