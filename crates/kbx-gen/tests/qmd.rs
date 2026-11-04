//! The qmd shell-out: against a stand-in everywhere, and against the real
//! binary when there is one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use kbx_core::{Graph, IndexOptions, index_vault};
use kbx_gen::{Hit, QmdSearch, Query, Search, SearchError};
use tempfile::TempDir;

const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
const ID_A: &str = "01KBX000000000000000000001";
const ID_B: &str = "01KBX000000000000000000002";

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
        index_vault(&mut graph, &root, ONTOLOGY, &IndexOptions::default()).unwrap();
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
        fs::write(&program, text).unwrap();
        make_executable(&program);
        program
    }
}

fn make_executable(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A stand-in that prints `json`, from a file because `echo` treats quotes
/// differently on each platform.
fn answering(vault: &Vault, json: &str) -> QmdSearch {
    let file = vault.dir.path().join("answer.json");
    fs::write(&file, json).unwrap();
    let print = if cfg!(windows) { "type" } else { "cat" };
    search(vault, &format!("{print} \"{}\"", file.display()), &[])
}

fn search(vault: &Vault, script: &str, collections: &[&str]) -> QmdSearch {
    let collections = collections.iter().map(|c| c.to_string()).collect();
    QmdSearch::new(vault.stand_in(script), collections, &vault.graph).unwrap()
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
fn a_collection_rooted_at_a_scope_maps_like_one_rooted_at_the_vault() {
    let vault = Vault::new();
    let rooted = format!("qmd://whole-vault/team-x/{}-other.md", ID_B.to_lowercase());
    let json = format!("[{{\"score\":0.5,\"file\":\"{rooted}\",\"line\":5}}]");
    let hits = answering(&vault, &json)
        .search(&Query::Text("words"), 20)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note, ID_B);
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
    let error = search(&vault, script, &["nope"])
        .search(&Query::Text("anything"), 20)
        .unwrap_err();
    assert!(matches!(error, SearchError::Failed(_)), "{error:?}");
    assert!(
        error.to_string().contains("Collection not found"),
        "{error}"
    );
}

#[test]
fn output_that_is_not_json_is_an_error() {
    let vault = Vault::new();
    let error = search(&vault, "echo oops", &[])
        .search(&Query::Text("anything"), 20)
        .unwrap_err();
    assert!(matches!(error, SearchError::Output(_)), "{error:?}");
}

#[test]
fn a_missing_binary_is_an_error() {
    let vault = Vault::new();
    let program = vault.dir.path().join("not-here");
    let search = QmdSearch::new(program, Vec::new(), &vault.graph).unwrap();
    let error = search.search(&Query::Text("x"), 20).unwrap_err();
    assert!(matches!(error, SearchError::Spawn(..)), "{error:?}");
}

#[cfg(unix)]
#[test]
fn arguments_are_query_json_limit_collections_and_one_line_of_text() {
    let vault = Vault::new();
    let log = vault.dir.path().join("args");
    let script = format!(
        "for a in \"$@\"; do echo \"$a\" >> '{}'; done; echo '[]'",
        log.display()
    );
    let query = Query::Section {
        note: ID_A,
        key: "k",
        text: "-leading dash\nand  a second line",
    };
    let hits = search(&vault, &script, &["one", "two"])
        .search(&query, 20)
        .unwrap();
    assert!(hits.is_empty());
    let args = fs::read_to_string(log).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(
        args,
        [
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
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport INDEX_PATH='{state}/index.sqlite' QMD_CONFIG_DIR='{state}' \
             XDG_CACHE_HOME='{state}/cache'\nexec '{qmd}' \"$@\"\n",
            state = state.display(),
            qmd = qmd.display()
        ),
    )
    .unwrap();
    make_executable(&wrapper);

    let scope = vault.dir.path().join("vault/Team X");
    let added = Command::new(&wrapper)
        .args(["collection", "add"])
        .arg(&scope)
        .args(["--name", "team-x", "--mask", "**/*.md"])
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");

    let search = QmdSearch::new(wrapper, vec!["team-x".to_string()], &vault.graph)
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
        vec!["no-such-collection".to_string()],
        &vault.graph,
    )
    .unwrap()
    .keyword_only()
    .search(&Query::Text("burst"), 20)
    .unwrap_err();
    assert!(error.to_string().contains("no-such-collection"), "{error}");
}
