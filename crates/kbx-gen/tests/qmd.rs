//! The qmd shell-out: against a stand-in everywhere, and against the real
//! binary when there is one.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

use kbx_core::{Exclusions, Graph, IndexOptions, index_vault};
use kbx_gen::{Hit, QmdSearch, Query, Search, SearchError};
use tempfile::TempDir;

const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
const ID_A: &str = "01KBX000000000000000000001";
const ID_B: &str = "01KBX000000000000000000002";
const INDEX: &str = "kbx-test";
/// The one collection of most tests: a scope whose folder name has a space.
const TEAM_X: &[(&str, &str)] = &[("team-x", "Team X")];

/// A two-note vault, one note with two sections, and its index.
struct Vault {
    dir: TempDir,
    graph: Graph,
}

impl Vault {
    fn new() -> Vault {
        let dir = TempDir::new().unwrap();
        let notes = dir.path().join("vault/Team X");
        fs::create_dir_all(&notes).unwrap();
        fs::write(
            notes.join(format!("{ID_A}-Token Buckets.md")),
            format!(
                "---\nid: {ID_A}\ntype: Concept\ntitle: Token buckets\n---\n\
                 # Refill\nThe bucket refills at a steady rate.\n\n\
                 # Burst\nA full bucket absorbs a burst of callers.\n"
            ),
        )
        .unwrap();
        fs::write(
            notes.join(format!("{ID_B}-other.md")),
            format!("---\nid: {ID_B}\ntype: Concept\ntitle: Other\n---\nUnrelated words.\n"),
        )
        .unwrap();
        let root = dir.path().join("vault").canonicalize().unwrap();
        let mut graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
        index_vault(
            &mut graph,
            &root,
            ONTOLOGY,
            &Exclusions::new(&root, &[]).unwrap(),
            &IndexOptions::default(),
        )
        .unwrap();
        Vault { dir, graph }
    }

    fn section_at(&self, id: &str, heading: &str) -> String {
        self.graph
            .connection()
            .query_row(
                "SELECT key FROM sections WHERE id = ? AND heading_path = ?",
                [id, heading],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// An executable that stands in for qmd. `script` is `sh`, or batch on
    /// Windows.
    fn stand_in(&self, script: &str) -> PathBuf {
        let (name, text) = if cfg!(windows) {
            ("qmd.cmd", format!("@echo off\r\n{script}\r\n"))
        } else {
            ("qmd", format!("#!/bin/sh\n{script}\n"))
        };
        let program = self.dir.path().join(name);
        write_executable(&program, &text);
        program
    }
}

/// Writes a script that is run straight away. On Linux, exec fails with
/// ETXTBSY while anything holds the file open for writing, and a child that
/// another test thread has just forked holds a copy of this process's
/// descriptors until it execs. So this process never opens the file for
/// writing: `cp` does, and has exited by the time this returns.
#[cfg(unix)]
fn write_executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::NamedTempFile;

    let staged = NamedTempFile::new().unwrap();
    fs::write(staged.path(), text).unwrap();
    let copied = Command::new("/bin/cp")
        .arg(staged.path())
        .arg(path)
        .status()
        .unwrap();
    assert!(copied.success(), "cp to {}: {copied}", path.display());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

/// A stand-in that prints `json`, from a file because `echo` treats quotes
/// differently on each platform.
fn answering(vault: &Vault, json: &str) -> QmdSearch {
    answering_in(vault, json, TEAM_X)
}

fn answering_in(vault: &Vault, json: &str, roots: &[(&str, &str)]) -> QmdSearch {
    let file = vault.dir.path().join("answer.json");
    fs::write(&file, json).unwrap();
    let print = if cfg!(windows) { "type" } else { "cat" };
    search(vault, &format!("{print} \"{}\"", file.display()), roots)
}

fn roots(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    let owned = pairs.iter().map(|(c, r)| (c.to_string(), r.to_string()));
    owned.collect()
}

fn search(vault: &Vault, script: &str, pairs: &[(&str, &str)]) -> QmdSearch {
    let program = vault.stand_in(script);
    QmdSearch::new(program, INDEX.to_string(), roots(pairs), &vault.graph).unwrap()
}

#[test]
fn hits_map_handles_back_to_notes_and_lines_to_sections() {
    let vault = Vault::new();
    let handle = format!("qmd://team-x/{}-token-buckets.md", ID_A.to_lowercase());
    let json = format!(
        "[{{\"docid\":\"#a\",\"score\":0.91,\"file\":\"{handle}\",\"line\":10,\"title\":\"T\"}}]"
    );
    let hits = answering(&vault, &json)
        .search(&Query::Text("bucket"), 20)
        .unwrap();
    let want = Hit {
        note: ID_A.to_string(),
        section: vault.section_at(ID_A, "burst"),
        score: 0.91,
    };
    assert_eq!(hits, [want]);
}

#[test]
fn a_collection_rooted_at_the_vault_maps_through_its_root() {
    let vault = Vault::new();
    let rooted = format!("qmd://whole-vault/team-x/{}-other.md", ID_B.to_lowercase());
    let json = format!("[{{\"score\":0.5,\"file\":\"{rooted}\",\"line\":5}}]");
    let hits = answering_in(&vault, &json, &[("whole-vault", ".")])
        .search(&Query::Text("words"), 20)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note, ID_B);
}

#[test]
fn a_hit_maps_through_its_own_root_and_not_by_a_path_that_looks_right() {
    let vault = Vault::new();
    // `team-x/…` is a vault path, but in this collection it is another file.
    let bare = format!("qmd://other/team-x/{}-other.md", ID_B.to_lowercase());
    let scoped = format!("qmd://team-x/team-x/{}-other.md", ID_B.to_lowercase());
    let unprefixed = format!("team-x/{}-other.md", ID_B.to_lowercase());
    for file in [bare, scoped, unprefixed] {
        let json = format!("[{{\"score\":0.5,\"file\":\"{file}\",\"line\":5}}]");
        let hits = answering_in(
            &vault,
            &json,
            &[("team-x", "Team X"), ("other", "Elsewhere")],
        )
        .search(&Query::Text("words"), 20)
        .unwrap();
        assert!(hits.is_empty(), "{file}: {hits:?}");
    }
}

#[test]
fn with_no_collections_nothing_is_searched_and_it_says_why() {
    let vault = Vault::new();
    let marker = vault.dir.path().join("ran");
    let script = if cfg!(windows) {
        format!("echo x > \"{}\"", marker.display())
    } else {
        format!(": > '{}'", marker.display())
    };
    let error = search(&vault, &script, &[])
        .search(&Query::Text("words"), 20)
        .unwrap_err();
    assert!(error.to_string().contains("kbx init"), "{error}");
    assert!(!marker.exists(), "qmd must not run");
}

/// The JSON of real `qmd search --json` runs: `file` is a lowercased handle
/// in 1.0.7, with the line only in the snippet; 2.8.3 keeps the path as
/// written, adds `?index=`, and gives `line`.
#[test]
fn real_output_of_both_qmd_versions_maps_to_the_section() {
    let vault = Vault::new();
    for json in [
        include_str!("fixtures/qmd-1.0.7.json"),
        include_str!("fixtures/qmd-2.8.3.json"),
    ] {
        let hits = answering(&vault, json)
            .search(&Query::Text("burst"), 20)
            .unwrap();
        assert_eq!(hits.len(), 1, "{json}");
        assert_eq!(hits[0].note, ID_A);
        assert_eq!(hits[0].section, vault.section_at(ID_A, "burst"), "{json}");
    }
}

#[test]
fn qmd_1_gives_the_line_only_in_the_snippet_header() {
    let vault = Vault::new();
    let handle = format!("qmd://team-x/{}-token-buckets.md", ID_A.to_lowercase());
    let json = format!(
        "[{{\"score\":0.5,\"file\":\"{handle}\",\"snippet\":\"@@ -10,2 @@ (9 before, 0 after)\\nA full\"}}]"
    );
    let hits = answering(&vault, &json)
        .search(&Query::Text("burst"), 20)
        .unwrap();
    assert_eq!(hits[0].section, vault.section_at(ID_A, "burst"));
}

#[test]
fn a_hit_without_a_line_lands_on_the_first_section() {
    let vault = Vault::new();
    let handle = format!("qmd://team-x/{}-token-buckets.md", ID_A.to_lowercase());
    let json = format!("[{{\"score\":0.5,\"file\":\"{handle}\"}}]");
    let hits = answering(&vault, &json)
        .search(&Query::Text("burst"), 20)
        .unwrap();
    assert_eq!(hits[0].section, vault.section_at(ID_A, "refill"));
}

#[test]
fn hits_outside_the_index_are_dropped() {
    let vault = Vault::new();
    let json = "[{\"score\":0.9,\"file\":\"qmd://elsewhere/some-note.md\",\"line\":1}]";
    let hits = answering(&vault, json)
        .search(&Query::Text("anything"), 20)
        .unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_failing_qmd_reports_its_stderr() {
    let vault = Vault::new();
    let script = if cfg!(windows) {
        "echo Collection not found: nope 1>&2\r\nexit /b 1"
    } else {
        "echo 'Collection not found: nope' >&2; exit 1"
    };
    let error = search(&vault, script, &[("nope", "nope")])
        .search(&Query::Text("anything"), 20)
        .unwrap_err();
    assert!(matches!(error, SearchError::Failed(_)), "{error:?}");
    let error = error.to_string();
    assert!(error.contains("Collection not found"), "{error}");
    assert!(error.contains("kbx doctor --fix"), "{error}");
}

#[test]
fn output_that_is_not_json_is_an_error() {
    let vault = Vault::new();
    let error = search(&vault, "echo oops", TEAM_X)
        .search(&Query::Text("anything"), 20)
        .unwrap_err();
    assert!(matches!(error, SearchError::Output(_)), "{error:?}");
}

#[test]
fn a_missing_binary_is_an_error() {
    let vault = Vault::new();
    let program = vault.dir.path().join("not-here");
    let search = QmdSearch::new(program, INDEX.into(), roots(TEAM_X), &vault.graph).unwrap();
    let error = search.search(&Query::Text("x"), 20).unwrap_err();
    assert!(matches!(error, SearchError::Spawn(..)), "{error:?}");
}

#[cfg(unix)]
#[test]
fn arguments_are_index_query_json_limit_collections_and_one_line_of_text() {
    let vault = Vault::new();
    let log = vault.dir.path().join("args");
    let script = format!(
        "for a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done; echo '[]'",
        log.display()
    );
    let query = Query::Note {
        note: ID_A,
        text: "-leading dash\nand  a second line",
    };
    let hits = search(&vault, &script, &[("one", "One"), ("two", "Two")])
        .search(&query, 20)
        .unwrap();
    assert!(hits.is_empty());
    let args = fs::read_to_string(log).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(
        args,
        [
            "--index",
            INDEX,
            "query",
            "--json",
            "-n",
            "20",
            "-c",
            "one",
            "-c",
            "two",
            "--",
            "-leading dash and a second line"
        ]
    );
}

/// Finds `qmd` on `PATH`, or says why the test can't run.
#[cfg(unix)]
fn real_qmd() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|folder| folder.join("qmd"))
            .find(|program| program.is_file())
    });
    if found.is_none() {
        eprintln!("skipped: qmd is not on PATH");
    }
    found
}

/// `qmd query` needs about 2 GB of models, so this runs the same code path
/// through `qmd search`. Everything qmd keeps is redirected into the temp
/// folder: nothing here reads or writes the user's own index.
#[cfg(unix)]
#[test]
fn real_qmd_finds_a_note_by_its_collection() {
    let Some(qmd) = real_qmd() else { return };
    let vault = Vault::new();
    let state = vault.dir.path().join("qmd-state");
    fs::create_dir_all(&state).unwrap();
    let wrapper = vault.dir.path().join("isolated-qmd");
    write_executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nexport INDEX_PATH='{state}/index.sqlite' QMD_CONFIG_DIR='{state}' \
             XDG_CACHE_HOME='{state}/cache'\nexec '{qmd}' \"$@\"\n",
            state = state.display(),
            qmd = qmd.display()
        ),
    );

    let scope = vault.dir.path().join("vault/Team X");
    let added = Command::new(&wrapper)
        .args(["--index", INDEX, "collection", "add"])
        .arg(&scope)
        .args(["--name", "team-x", "--mask", "**/*.md"])
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");

    let search = QmdSearch::new(wrapper, INDEX.into(), roots(TEAM_X), &vault.graph)
        .unwrap()
        .keyword_only();
    let hits = search.search(&Query::Text("burst"), 20).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(
        hits[0].note, ID_A,
        "the handle maps back through a spaced path"
    );

    let error = QmdSearch::new(
        vault.dir.path().join("isolated-qmd"),
        INDEX.into(),
        roots(&[("no-such-collection", "Team X")]),
        &vault.graph,
    )
    .unwrap()
    .keyword_only()
    .search(&Query::Text("burst"), 20)
    .unwrap_err();
    assert!(error.to_string().contains("no-such-collection"), "{error}");
}
