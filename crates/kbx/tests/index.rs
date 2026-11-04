//! Drives `kbx init` and `kbx index` over a copy of the fixture vault.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{FIXTURE, FakePath, copy_dir, id};
use rusqlite::Connection;
use tempfile::TempDir;

const EXAMPLE_ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

/// Tables that `kbx index` fills from the files.
const FILLED: [&str; 6] = ["files", "aliases", "sections", "blocks", "links", "edges"];

/// The column that says which note or file a row in each table belongs to.
const OWNERS: [(&str, &str); 7] = [
    ("files", "path"),
    ("aliases", "id"),
    ("sections", "id"),
    ("blocks", "id"),
    ("links", "src"),
    ("edges", "src"),
    ("meta", "k"),
];

struct Vault {
    dir: TempDir,
    /// `kbx init` installs a missing qmd, so it must never find the real npm.
    path: FakePath,
}

impl Vault {
    /// An empty folder.
    fn empty() -> Vault {
        Vault {
            dir: TempDir::new().unwrap(),
            path: FakePath::with_qmd(),
        }
    }

    /// A copy of the fixture vault, after `kbx init`.
    fn fixture() -> Vault {
        let vault = Vault::empty();
        copy_dir(Path::new(FIXTURE), vault.root());
        vault.ok(&["init"]);
        vault
    }

    /// A folder with only `kbx init` run in it.
    fn initialised() -> Vault {
        let vault = Vault::empty();
        vault.ok(&["init"]);
        vault
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn append(&self, relative: &str, text: &str) {
        let before = fs::read_to_string(self.path(relative)).unwrap();
        self.write(relative, &format!("{before}{text}"));
    }

    fn kbx(&self, args: &[&str]) -> Output {
        self.kbx_in(self.root(), args)
    }

    fn kbx_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.path.kbx(dir, args)
    }

    /// Runs kbx, requires success, and returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.kbx(args);
        assert!(
            output.status.success(),
            "kbx {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn db(&self) -> Connection {
        Connection::open(self.path(".kbx/graph.sqlite")).unwrap()
    }

    fn count(&self, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        self.db().query_row(&sql, [], |row| row.get(0)).unwrap()
    }

    fn counts(&self) -> BTreeMap<&'static str, i64> {
        FILLED.map(|table| (table, self.count(table))).into()
    }

    /// Every row of the filled tables as text, leaving out the two columns
    /// that record when, not what.
    fn dump(&self) -> Vec<String> {
        let db = self.db();
        let mut rows = Vec::new();
        for table in FILLED {
            let columns: Vec<String> = db
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .filter(|name: &String| name != "mtime" && name != "indexed_at")
                .collect();
            let quoted: Vec<String> = columns.iter().map(|c| format!("quote({c})")).collect();
            let sql = format!("SELECT {} FROM {table}", quoted.join(" || '|' || "));
            let mut statement = db.prepare(&sql).unwrap();
            let lines = statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap();
            rows.extend(lines.map(|line| format!("{table}: {}", line.unwrap())));
        }
        rows.sort();
        rows
    }

    /// Records every later write to a filled table, or to `meta`, in `audit`.
    /// The triggers live in the file, so they see what the kbx process does.
    fn audit(&self) {
        let db = self.db();
        db.execute_batch("CREATE TABLE audit (tbl TEXT, op TEXT, owner TEXT)")
            .unwrap();
        for (table, owner) in OWNERS {
            for (op, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
                db.execute_batch(&format!(
                    "CREATE TRIGGER audit_{table}_{op} AFTER {op} ON {table} BEGIN
                         INSERT INTO audit VALUES ('{table}', '{op}', {row}.{owner});
                     END"
                ))
                .unwrap();
            }
        }
    }

    /// What the triggers saw, as `table op owner`.
    fn audited(&self) -> Vec<String> {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT tbl || ' ' || op || ' ' || owner FROM audit")
            .unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    /// `(dst, rel, origin)` of the edges leaving a note.
    fn edges_from(&self, src: &str) -> Vec<(String, String, String)> {
        let db = self.db();
        let mut statement = db
            .prepare("SELECT dst, rel, origin FROM edges WHERE src = ? ORDER BY dst, rel, origin")
            .unwrap();
        let rows = statement
            .query_map([src], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    fn has_edge(&self, src: &str, dst: &str, rel: &str, origin: &str) -> bool {
        let edge = (dst.to_string(), rel.to_string(), origin.to_string());
        self.edges_from(src).contains(&edge)
    }
}

fn note(n: u32, title: &str, extra_frontmatter: &str, body: &str) -> String {
    let id = id(n);
    format!("---\nid: {id}\ntype: Concept\ntitle: {title}\n{extra_frontmatter}---\n{body}\n")
}

const RATE_LIMITS: &str = "engineering/01KBX000000000000000000006-rate-limits.md";
const OAUTH_FLOW: &str = "engineering/01KBX000000000000000000002-oauth-flow.md";
const LINK_UNRESOLVED: &str = "lint/01KBX000000000000000000023-link-unresolved.md";

// --- kbx init ---------------------------------------------------------------

#[test]
fn init_creates_the_config_the_example_ontology_and_the_database() {
    let vault = Vault::empty();
    fs::create_dir(vault.path("team-y")).unwrap();
    fs::create_dir(vault.path("team-x")).unwrap();
    fs::create_dir(vault.path(".obsidian")).unwrap();
    vault.ok(&["init"]);

    let text = fs::read_to_string(vault.path("kbx.toml")).unwrap();
    let config: toml::Table = text.parse().unwrap();
    assert_eq!(config["vault"].as_str(), Some("."));
    assert_eq!(config["ontology"].as_str(), Some(".kbx/ontology.yaml"));
    assert_eq!(config["search"].as_str(), Some("qmd"));
    let scopes = toml::Value::try_from(["team-x", "team-y"]).unwrap();
    assert_eq!(config["scopes"], scopes);
    assert_eq!(config["qmd"]["bin"].as_str(), Some("qmd"));
    assert_eq!(config["qmd"]["collections"], scopes);
    assert_eq!(config["judge"]["batch"].as_integer(), Some(20));
    assert!(config["judge"]["overrides"].is_table());
    assert!(
        !config.contains_key("mcp"),
        "this project has no MCP server"
    );

    assert_eq!(
        fs::read_to_string(vault.path(".kbx/ontology.yaml")).unwrap(),
        EXAMPLE_ONTOLOGY
    );
    assert!(vault.path(".kbx/graph.sqlite").is_file());
}

#[test]
fn init_with_an_ontology_points_the_config_at_that_file() {
    let vault = Vault::empty();
    let custom = EXAMPLE_ONTOLOGY.replace("weight: 1.0", "weight: 0.5");
    vault.write("schema.yaml", &custom);
    vault.write(
        "a/note.md",
        &note(
            1,
            "One",
            "part_of: [\"[[01KBX000000000000000000002]]\"]\n",
            "x",
        ),
    );
    vault.write("a/other.md", &note(2, "Two", "", "y"));
    vault.ok(&["init", "--ontology", "schema.yaml"]);
    vault.ok(&["index"]);

    let config: toml::Table = fs::read_to_string(vault.path("kbx.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(config["ontology"].as_str(), Some("schema.yaml"));
    assert!(!vault.path(".kbx/ontology.yaml").exists());
    let weight: f64 = vault
        .db()
        .query_row(
            "SELECT weight FROM edges WHERE rel = 'part_of'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(weight, 0.5);
}

#[test]
fn init_refuses_an_invalid_ontology_and_writes_nothing() {
    let vault = Vault::empty();
    vault.write("schema.yaml", "version: 99\n");
    let output = vault.kbx(&["init", "--ontology", "schema.yaml"]);
    assert!(!output.status.success());
    assert!(!vault.path("kbx.toml").exists());
    assert!(!vault.path(".kbx").exists());
}

#[test]
fn init_leaves_an_existing_config_and_ontology_alone() {
    let vault = Vault::initialised();
    vault.append("kbx.toml", "# mine\n");
    vault.append(".kbx/ontology.yaml", "# mine\n");
    vault.ok(&["init"]);
    let config = fs::read_to_string(vault.path("kbx.toml")).unwrap();
    assert!(config.ends_with("# mine\n"));
    let ontology = fs::read_to_string(vault.path(".kbx/ontology.yaml")).unwrap();
    assert!(ontology.ends_with("# mine\n"));

    let output = vault.kbx(&["init", "--ontology", "elsewhere.yaml"]);
    assert!(!output.status.success(), "the flag would be ignored");
}

#[test]
fn index_needs_a_config_and_finds_it_from_a_subfolder() {
    let bare = Vault::empty();
    let output = bare.kbx(&["index"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("kbx init"));

    let vault = Vault::fixture();
    let output = vault.kbx_in(&vault.path("engineering"), &["index"]);
    assert!(output.status.success());
    assert_eq!(vault.count("files"), 25);
}

// --- schema -----------------------------------------------------------------

#[test]
fn the_database_has_the_section_4_schema_in_wal_mode() {
    let vault = Vault::initialised();
    let db = vault.db();
    let columns = |table: &str| -> Vec<String> {
        db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let expected: [(&str, &[&str]); 8] = [
        (
            "files",
            &[
                "path",
                "id",
                "type",
                "title",
                "content_hash",
                "fm_hash",
                "mtime",
                "scope",
                "indexed_at",
            ],
        ),
        ("aliases", &["alias", "id"]),
        (
            "sections",
            &[
                "key",
                "id",
                "heading_path",
                "block_id",
                "start_line",
                "end_line",
                "text_hash",
                "tokens",
            ],
        ),
        (
            "edges",
            &[
                "src",
                "dst",
                "rel",
                "inverse_of",
                "weight",
                "origin",
                "src_section",
                "line",
            ],
        ),
        (
            "candidates",
            &[
                "cid",
                "src",
                "dst",
                "generators",
                "features",
                "score",
                "created_at",
                "src_hash",
                "dst_hash",
                "src_section",
                "dst_section",
            ],
        ),
        (
            "verdicts",
            &[
                "cid",
                "backend",
                "model",
                "prompt_hash",
                "verdict",
                "rel",
                "confidence",
                "rationale",
                "cost_usd",
                "cached_tokens",
                "created_at",
            ],
        ),
        (
            "reviews",
            &[
                "src",
                "dst",
                "rel",
                "decision",
                "reason",
                "strikes",
                "decided_at",
            ],
        ),
        ("meta", &["k", "v"]),
    ];
    for (table, wanted) in expected {
        assert_eq!(columns(table), wanted, "columns of {table}");
    }

    let index: String = db
        .query_row(
            "SELECT tbl_name FROM sqlite_master WHERE type = 'index' AND name = 'edges_dst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, "edges");
    let mode: String = db
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

// --- AC1: fill, then touch nothing ------------------------------------------

#[test]
fn index_fills_files_aliases_sections_and_edges() {
    let vault = Vault::fixture();
    let stdout = vault.ok(&["index"]);
    assert!(stdout.contains("indexed 25,"), "{stdout}");

    assert_eq!(vault.count("files"), 25);
    assert!(vault.count("aliases") > 0);
    assert!(vault.count("sections") >= 25);
    assert!(vault.count("edges") > 0);

    let db = vault.db();
    let (id_, class, title, scope): (String, String, String, String) = db
        .query_row(
            "SELECT id, type, title, scope FROM files WHERE path = ?",
            [OAUTH_FLOW],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(id_, id(2));
    assert_eq!(class, "Procedure");
    assert_eq!(title, "OAuth 2.0 authorization flow");
    assert_eq!(scope, "engineering");

    let aliases: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM aliases WHERE id = ? AND alias IN ('oauth flow', 'authorization code grant')",
            [id(2)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(aliases, 2);

    let (lines, tokens): ((i64, i64), i64) = db
        .query_row(
            "SELECT start_line, end_line, tokens FROM sections WHERE id = ? AND heading_path = 'flow/steps' ORDER BY start_line",
            [id(2)],
            |row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)),
        )
        .unwrap();
    assert!(lines.0 < lines.1);
    assert!(tokens > 0);
}

#[test]
fn a_file_at_the_vault_root_has_no_scope() {
    let vault = Vault::initialised();
    vault.write("top.md", &note(1, "Top", "", "x"));
    vault.ok(&["index"]);
    let scope: Option<String> = vault
        .db()
        .query_row("SELECT scope FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(scope, None);
}

#[test]
fn a_second_index_touches_no_rows() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.audit();

    let stdout = vault.ok(&["index"]);

    assert!(stdout.contains("indexed 0,"), "{stdout}");
    assert!(stdout.contains(", 25 unchanged"), "{stdout}");
    assert_eq!(vault.audited(), Vec::<String>::new());
}

#[test]
fn rewriting_a_file_with_the_same_bytes_touches_no_rows() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.audit();

    let same = fs::read_to_string(vault.path(RATE_LIMITS)).unwrap();
    vault.write(RATE_LIMITS, &same);
    vault.ok(&["index"]);

    assert_eq!(vault.audited(), Vec::<String>::new());
}

// --- AC2: one file, and --full ----------------------------------------------

#[test]
fn editing_one_note_reindexes_only_that_file() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault
        .db()
        .execute("UPDATE files SET indexed_at = 0", [])
        .unwrap();
    vault.audit();

    vault.append(RATE_LIMITS, "\nA new line about limits.\n");
    let stdout = vault.ok(&["index"]);

    assert!(stdout.contains("indexed 1,"), "{stdout}");
    assert!(stdout.contains(", 24 unchanged"), "{stdout}");
    let db = vault.db();
    let mut statement = db
        .prepare("SELECT path FROM files WHERE indexed_at <> 0")
        .unwrap();
    let touched: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(touched, [RATE_LIMITS]);

    let owner = id(6);
    for write in vault.audited() {
        assert!(
            write.ends_with(RATE_LIMITS) || write.ends_with(&owner),
            "wrote to another note: {write}"
        );
    }
}

#[test]
fn a_body_edit_leaves_aliases_and_frontmatter_links_alone() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.audit();

    vault.append(OAUTH_FLOW, "\nMore text.\n");
    vault.ok(&["index"]);

    let writes = vault.audited();
    assert!(writes.iter().any(|w| w.starts_with("files UPDATE")));
    assert!(writes.iter().any(|w| w.starts_with("sections")));
    for write in &writes {
        assert!(
            !write.starts_with("aliases")
                && !write.starts_with("links")
                && !write.starts_with("edges"),
            "unexpected write: {write}"
        );
    }
    // The appended text lands in the last section, so the others keep their rows.
    let sections: Vec<&String> = writes
        .iter()
        .filter(|w| w.starts_with("sections"))
        .collect();
    assert_eq!(sections.len(), 1, "{sections:?}");
}

#[test]
fn a_frontmatter_edit_updates_aliases_and_edges() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);

    let before = fs::read_to_string(vault.path(RATE_LIMITS)).unwrap();
    let after = before
        .replace("[token bucket, request quota]", "[request quota, throttle]")
        .replace(
            "part_of: [\"[[01KBX000000000000000000001-platform]]\"]",
            "see_also: [\"[[01KBX000000000000000000002-oauth-flow]]\"]",
        );
    vault.write(RATE_LIMITS, &after);
    vault.ok(&["index"]);

    let db = vault.db();
    let mut statement = db
        .prepare("SELECT alias FROM aliases WHERE id = ? ORDER BY alias")
        .unwrap();
    let aliases: Vec<String> = statement
        .query_map([id(6)], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(aliases, ["request quota", "throttle"]);
    assert!(!vault.has_edge(&id(6), &id(1), "part_of", "frontmatter"));
    assert!(!vault.has_edge(&id(1), &id(6), "has_part", "derived"));
    assert!(vault.has_edge(&id(6), &id(2), "see_also", "frontmatter"));
    assert!(vault.has_edge(&id(2), &id(6), "see_also", "derived"));
    // `…07-retries` linked through the alias that just went away.
    assert!(!vault.has_edge(&id(7), &id(6), "see_also", "frontmatter"));
}

#[test]
fn full_rebuilds_everything() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    let before = vault.dump();
    let db = vault.db();
    db.execute("UPDATE files SET indexed_at = 0", []).unwrap();
    db.execute_batch(
        "INSERT INTO edges (src, dst, rel, origin) VALUES ('stale', 'stale', 'see_also', 'inline');
         UPDATE sections SET tokens = -1;
         DELETE FROM aliases;",
    )
    .unwrap();
    drop(db);

    let stdout = vault.ok(&["index", "--full"]);

    assert!(stdout.contains("indexed 25,"), "{stdout}");
    assert_eq!(vault.dump(), before);
    let untouched: i64 = vault
        .db()
        .query_row(
            "SELECT COUNT(*) FROM files WHERE indexed_at = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(untouched, 0);
}

#[test]
fn full_keeps_what_cannot_be_rebuilt_from_the_files() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault
        .db()
        .execute_batch(
            "INSERT INTO reviews (src, dst, rel, decision) VALUES ('a', 'b', 'see_also', 'reject');
             INSERT INTO candidates (cid, src, dst) VALUES ('c', 'a', 'b');
             INSERT INTO verdicts (cid, backend, model, prompt_hash) VALUES ('c', 'b', 'm', 'h');",
        )
        .unwrap();

    vault.ok(&["index", "--full"]);

    assert_eq!(vault.count("reviews"), 1);
    assert_eq!(vault.count("candidates"), 1);
    assert_eq!(vault.count("verdicts"), 1);
}

#[test]
fn paths_limit_the_run_to_those_files() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);

    vault.append(RATE_LIMITS, "\nEdited.\n");
    vault.append(OAUTH_FLOW, "\nEdited.\n");
    let stdout = vault.ok(&["index", RATE_LIMITS]);
    assert!(stdout.contains("indexed 1,"), "{stdout}");
    assert!(stdout.contains(", 0 unchanged"), "{stdout}");

    let stdout = vault.ok(&["index", "engineering"]);
    assert!(stdout.contains("indexed 1,"), "{stdout}");
    assert!(stdout.contains(", 6 unchanged"), "{stdout}");
}

#[test]
fn a_path_is_relative_to_the_working_directory() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.append(RATE_LIMITS, "\nEdited.\n");
    let file = "01KBX000000000000000000006-rate-limits.md";
    let output = vault.kbx_in(&vault.path("engineering"), &["index", file]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("indexed 1,"));
}

#[test]
fn a_deleted_path_removes_its_rows_and_an_unknown_one_is_an_error() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    fs::remove_file(vault.path(RATE_LIMITS)).unwrap();

    let stdout = vault.ok(&["index", RATE_LIMITS]);
    assert!(stdout.contains("removed 1,"), "{stdout}");
    assert_eq!(vault.count("files"), 24);

    assert!(!vault.kbx(&["index", "no/such/file.md"]).status.success());
    let outside = TempDir::new().unwrap();
    let outside = outside.path().to_str().unwrap().to_string();
    assert!(!vault.kbx(&["index", &outside]).status.success());
}

#[test]
fn full_with_paths_reindexes_only_those_paths() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    let stdout = vault.ok(&["index", "--full", "decisions"]);
    assert!(stdout.contains("indexed 3,"), "{stdout}");
    assert_eq!(vault.count("files"), 25);
}

// --- AC3: the files are the only source of truth ----------------------------

#[test]
fn deleting_the_database_reproduces_the_row_counts() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    let counts = vault.counts();
    let dump = vault.dump();

    fs::remove_file(vault.path(".kbx/graph.sqlite")).unwrap();
    vault.ok(&["index"]);

    assert_eq!(vault.counts(), counts);
    assert_eq!(vault.dump(), dump);
}

#[test]
fn incremental_runs_end_where_a_rebuild_would() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);

    // One of each kind of change, indexed separately.
    vault.append(RATE_LIMITS, "\nSee [[01KBX000000000000000000099]].\n");
    vault.ok(&["index"]);
    vault.write(
        "lint/new.md",
        &note(99, "Target", "aliases: [ghost note]\n", "New."),
    );
    vault.ok(&["index"]);
    fs::rename(
        vault.path(OAUTH_FLOW),
        vault.path("operations/01KBX000000000000000000002-moved.md"),
    )
    .unwrap();
    vault.ok(&["index"]);
    fs::remove_file(vault.path("engineering/01KBX000000000000000000003-tokens.md")).unwrap();
    vault.ok(&["index"]);
    vault.write(
        "engineering/plain.md",
        "No frontmatter, so this is not a note.\n",
    );
    vault.ok(&["index"]);

    let incremental = vault.dump();
    fs::remove_file(vault.path(".kbx/graph.sqlite")).unwrap();
    vault.ok(&["index"]);
    assert_eq!(vault.dump(), incremental);
}

// --- edges ------------------------------------------------------------------

#[test]
fn a_relation_gets_a_derived_inverse() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);

    // …08-deploy-service depends on …06-rate-limits; nothing states the inverse.
    assert!(vault.has_edge(&id(8), &id(6), "depends_on", "frontmatter"));
    assert!(vault.has_edge(&id(6), &id(8), "required_by", "derived"));

    let db = vault.db();
    let row = |src: String, rel: &str| -> (Option<String>, Option<f64>, Option<i64>) {
        db.query_row(
            "SELECT inverse_of, weight, line FROM edges WHERE src = ? AND dst = ? AND rel = ?",
            [
                src.clone(),
                if src == id(8) { id(6) } else { id(8) },
                rel.to_string(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };
    let (inverse_of, weight, line) = row(id(8), "depends_on");
    assert_eq!(inverse_of, None);
    assert_eq!(weight, Some(0.9));
    assert!(line.is_some());
    let (inverse_of, weight, line) = row(id(6), "required_by");
    assert_eq!(inverse_of.as_deref(), Some("depends_on"));
    assert_eq!(weight, Some(0.9));
    assert_eq!(line, None);
}

#[test]
fn an_inverse_written_by_hand_is_not_derived_again() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);

    // …02 says depends_on …03 and …03 says required_by …02.
    assert!(vault.has_edge(&id(2), &id(3), "depends_on", "frontmatter"));
    assert!(vault.has_edge(&id(3), &id(2), "required_by", "frontmatter"));
    assert!(!vault.has_edge(&id(2), &id(3), "depends_on", "derived"));
    assert!(!vault.has_edge(&id(3), &id(2), "required_by", "derived"));
}

#[test]
fn an_inverse_key_derives_the_forward_relation() {
    let vault = Vault::initialised();
    vault.write(
        "a/whole.md",
        &note(
            1,
            "Whole",
            "has_part: [\"[[01KBX000000000000000000002]]\"]\n",
            "x",
        ),
    );
    vault.write("a/piece.md", &note(2, "Piece", "", "y"));
    vault.ok(&["index"]);

    assert!(vault.has_edge(&id(1), &id(2), "has_part", "frontmatter"));
    assert!(vault.has_edge(&id(2), &id(1), "part_of", "derived"));
    let db = vault.db();
    let inverse_of = |rel: &str| -> Option<String> {
        db.query_row("SELECT inverse_of FROM edges WHERE rel = ?", [rel], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert_eq!(inverse_of("has_part").as_deref(), Some("part_of"));
    assert_eq!(inverse_of("part_of"), None);
}

#[test]
fn a_symmetric_relation_mirrors_itself() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    assert!(vault.has_edge(&id(14), &id(1), "see_also", "frontmatter"));
    assert!(vault.has_edge(&id(1), &id(14), "see_also", "derived"));
}

#[test]
fn inline_links_become_untyped_mentions() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    assert!(vault.has_edge(&id(2), &id(3), "mentions", "inline"));
    assert!(!vault.has_edge(&id(3), &id(2), "mentions", "inline"));

    let db = vault.db();
    let (inverse_of, weight, section, line): (Option<String>, Option<f64>, String, i64) = db
        .query_row(
            "SELECT inverse_of, weight, src_section, line FROM edges WHERE src = ? AND rel = 'mentions'",
            [id(2)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(inverse_of, None);
    assert_eq!(weight, None);
    assert!(line > 0);
    let owner: String = db
        .query_row("SELECT id FROM sections WHERE key = ?", [section], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(owner, id(2));
}

#[test]
fn repeated_links_are_one_edge_at_the_first_line_and_all_stay_in_links() {
    let vault = Vault::initialised();
    let body = "[[01KBX000000000000000000002]] and [[01KBX000000000000000000002]]\n\n# Later\n\n[[01KBX000000000000000000002]]";
    vault.write("a/one.md", &note(1, "One", "", body));
    vault.write("a/two.md", &note(2, "Two", "", "x"));
    vault.ok(&["index"]);

    assert_eq!(vault.count("links"), 3);
    assert_eq!(vault.count("edges"), 1);
    let line: i64 = vault
        .db()
        .query_row("SELECT line FROM edges", [], |row| row.get(0))
        .unwrap();
    assert_eq!(line, 6);
}

#[test]
fn a_key_outside_the_ontology_is_kept_as_an_edge_without_an_inverse() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    let edges = vault.edges_from(&id(18));
    let unknown: Vec<_> = edges
        .iter()
        .filter(|(_, rel, _)| rel == "relates_to")
        .collect();
    assert_eq!(unknown.len(), 1, "{edges:?}");
    let dst = &unknown[0].0;
    assert!(!vault.edges_from(dst).iter().any(|(to, _, _)| *to == id(18)));
}

#[test]
fn an_ontology_change_rederives_edges_without_reparsing() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.audit();

    let ontology = fs::read_to_string(vault.path(".kbx/ontology.yaml")).unwrap();
    vault.write(
        ".kbx/ontology.yaml",
        &ontology.replace("inverse: required_by", "inverse: needed_by"),
    );
    let stdout = vault.ok(&["index"]);

    assert!(stdout.contains("indexed 0,"), "{stdout}");
    assert!(vault.has_edge(&id(6), &id(8), "needed_by", "derived"));
    assert!(!vault.has_edge(&id(6), &id(8), "required_by", "derived"));
    let writes = vault.audited();
    assert!(writes.iter().any(|w| w.starts_with("edges")));
    assert!(writes.iter().any(|w| w == "meta UPDATE ontology_hash"));
    assert!(!writes.iter().any(|w| w.starts_with("files")));
}

#[test]
fn a_new_pipeline_version_rebuilds() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault
        .db()
        .execute("UPDATE meta SET v = 'old' WHERE k = 'pipeline_version'", [])
        .unwrap();

    let stdout = vault.ok(&["index"]);

    assert!(stdout.contains("indexed 25,"), "{stdout}");
    let version: String = vault
        .db()
        .query_row(
            "SELECT v FROM meta WHERE k = 'pipeline_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(version, "old");
}

// --- §8: unresolved links are hidden ----------------------------------------

#[test]
fn unresolved_links_are_hidden_not_errors() {
    let vault = Vault::fixture();
    let stdout = vault.ok(&["index"]);
    assert!(stdout.contains(", 2 hidden"), "{stdout}");

    let db = vault.db();
    let mut statement = db
        .prepare("SELECT target, origin FROM links WHERE dst IS NULL ORDER BY target")
        .unwrap();
    let hidden: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        hidden,
        [
            (
                "01KBX000000000000000000099-missing".to_string(),
                "frontmatter".to_string()
            ),
            ("ghost note".to_string(), "inline".to_string()),
        ]
    );
    assert_eq!(vault.edges_from(&id(23)), []);
}

#[test]
fn a_hidden_link_surfaces_when_its_target_appears_and_hides_when_it_goes() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.audit();

    let target = "lint/ghost.md";
    vault.write(
        target,
        &note(99, "Ghost", "aliases: [ghost note]\n", "Boo."),
    );
    let stdout = vault.ok(&["index"]);

    assert!(stdout.contains("indexed 1,"), "{stdout}");
    assert!(vault.has_edge(&id(23), &id(99), "see_also", "frontmatter"));
    assert!(vault.has_edge(&id(23), &id(99), "mentions", "inline"));
    assert!(vault.has_edge(&id(99), &id(23), "see_also", "derived"));
    let source_row = format!("files UPDATE {LINK_UNRESOLVED}");
    assert!(
        !vault.audited().contains(&source_row),
        "source was re-indexed"
    );

    fs::remove_file(vault.path(target)).unwrap();
    let stdout = vault.ok(&["index"]);
    assert!(stdout.contains("removed 1,"), "{stdout}");
    assert_eq!(vault.edges_from(&id(23)), []);
    assert_eq!(vault.edges_from(&id(99)), []);
    assert!(stdout.contains(", 2 hidden"), "{stdout}");
}

#[test]
fn renaming_a_file_keeps_links_by_id_and_hides_links_by_filename() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    assert!(vault.has_edge(&id(21), &id(22), "see_also", "frontmatter"));

    fs::rename(
        vault.path("lint/legacy-runbook.md"),
        vault.path("lint/renamed-runbook.md"),
    )
    .unwrap();
    fs::rename(
        vault.path(RATE_LIMITS),
        vault.path("engineering/01KBX000000000000000000006-quotas.md"),
    )
    .unwrap();
    vault.ok(&["index"]);

    assert!(!vault.has_edge(&id(21), &id(22), "see_also", "frontmatter"));
    assert!(vault.has_edge(&id(8), &id(6), "depends_on", "frontmatter"));
    assert_eq!(vault.count("files"), 25);
}

// --- what isn't a note ------------------------------------------------------

#[test]
fn a_file_without_frontmatter_is_skipped_and_logged_at_debug() {
    let vault = Vault::fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
        .arg("index")
        .env("KBX_LOG", "debug")
        .current_dir(vault.root())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(", 2 skipped"));
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(log.contains("DEBUG") && log.contains("README.md"), "{log}");
    assert_eq!(vault.count("files"), 25);

    let quiet = vault.kbx(&["index"]);
    let log = String::from_utf8_lossy(&quiet.stderr);
    assert!(!log.contains("README.md"), "the skip is debug only: {log}");
}

#[test]
fn a_note_that_stops_having_frontmatter_loses_its_rows() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.write(RATE_LIMITS, "Plain text now.\n");
    vault.ok(&["index"]);
    assert_eq!(vault.count("files"), 24);
    let sections: i64 = vault
        .db()
        .query_row(
            "SELECT COUNT(*) FROM sections WHERE id = ?",
            [id(6)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sections, 0);
    assert_eq!(vault.edges_from(&id(6)), []);
}

#[test]
fn a_malformed_note_warns_and_does_not_stop_the_run() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    vault.write(RATE_LIMITS, "---\nid: [unclosed\n---\nBody.\n");
    vault.write("lint/binary.md", "");
    fs::write(vault.path("lint/binary.md"), [0xff, 0xfe, 0x00]).unwrap();

    let output = vault.kbx(&["index"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(", 4 failed"), "{stdout}");
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        log.contains(RATE_LIMITS) && log.contains("lint/binary.md"),
        "{log}"
    );
    assert_eq!(vault.count("files"), 24, "files are the source of truth");
}

#[test]
fn hidden_folders_and_other_file_types_are_not_walked() {
    let vault = Vault::initialised();
    vault.write("a/note.md", &note(1, "One", "", "x"));
    vault.write("a/NOTE2.MD", &note(2, "Two", "", "x"));
    vault.write("a/data.txt", &note(3, "Text", "", "x"));
    vault.write(".obsidian/note.md", &note(4, "Hidden", "", "x"));
    vault.write(".trash/note.md", &note(5, "Trashed", "", "x"));
    vault.ok(&["index"]);
    assert_eq!(vault.count("files"), 2);
}

#[test]
fn the_first_path_keeps_an_id_that_two_files_claim() {
    let vault = Vault::initialised();
    vault.write("b/note.md", &note(1, "From b", "", "x"));
    vault.ok(&["index"]);

    // Sorts first, so it takes the id over, as a rebuild would decide.
    vault.write("a/copy.md", &note(1, "From a", "", "x"));
    let output = vault.kbx(&["index"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("b/note.md"));
    let title = |vault: &Vault| -> String {
        vault
            .db()
            .query_row("SELECT title FROM files", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(title(&vault), "From a");
    assert_eq!(vault.count("files"), 1);
    let dump = vault.dump();

    fs::remove_file(vault.path(".kbx/graph.sqlite")).unwrap();
    vault.ok(&["index"]);
    assert_eq!(vault.dump(), dump);

    // Once the winner goes, the other file comes back.
    fs::remove_file(vault.path("a/copy.md")).unwrap();
    vault.ok(&["index"]);
    assert_eq!(title(&vault), "From b");
}

#[test]
fn indexing_only_the_new_path_of_a_renamed_note_moves_its_rows() {
    let vault = Vault::initialised();
    vault.write("a/note.md", &note(1, "One", "", "x"));
    vault.ok(&["index"]);
    fs::rename(vault.path("a/note.md"), vault.path("b.md")).unwrap();

    let output = vault.kbx(&["index", "b.md"]);

    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "the old path is no conflict");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("indexed 1,"), "{stdout}");
    assert!(stdout.contains("removed 1,"), "{stdout}");
    let path: String = vault
        .db()
        .query_row("SELECT path FROM files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(path, "b.md");
    assert_eq!(vault.count("files"), 1);
}

#[test]
fn a_changed_id_moves_the_rows_to_the_new_id() {
    let vault = Vault::initialised();
    vault.write(
        "a/note.md",
        &note(1, "One", "aliases: [first]\n", "# Head\n"),
    );
    vault.ok(&["index"]);
    vault.write(
        "a/note.md",
        &note(2, "One", "aliases: [first]\n", "# Head\n"),
    );
    vault.ok(&["index"]);

    let owned = |table: &str, n: u32| -> i64 {
        vault
            .db()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE id = ?"),
                [id(n)],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!((owned("files", 1), owned("files", 2)), (0, 1));
    assert_eq!((owned("aliases", 1), owned("aliases", 2)), (0, 1));
    assert_eq!((owned("sections", 1), owned("sections", 2)), (0, 1));
}

// --- block anchors ----------------------------------------------------------

#[test]
fn every_block_id_stays_resolvable() {
    let vault = Vault::initialised();
    let body = "# Head ^head\n\nOne. ^one\n\nTwo. ^two\n\n# Plain\n\nThree. ^three\n";
    vault.write("a/note.md", &note(1, "One", "", body));
    vault.ok(&["index"]);

    let db = vault.db();
    let mut statement = db
        .prepare(
            "SELECT b.block_id, s.heading_path, s.block_id FROM blocks b
             JOIN sections s ON s.key = b.section_key
             WHERE b.id = ? ORDER BY b.start_line",
        )
        .unwrap();
    let blocks: Vec<(String, String, Option<String>)> = statement
        .query_map([id(1)], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let head = Some("head".to_string());
    assert_eq!(
        blocks,
        [
            ("head".to_string(), "head".to_string(), head.clone()),
            ("one".to_string(), "head".to_string(), head.clone()),
            ("two".to_string(), "head".to_string(), head),
            ("three".to_string(), "plain".to_string(), None),
        ]
    );
}

#[test]
fn the_first_block_keeps_a_repeated_id() {
    let vault = Vault::initialised();
    vault.write(
        "a/note.md",
        &note(1, "One", "", "First. ^same\n\nSecond. ^same"),
    );
    vault.ok(&["index"]);
    let line: i64 = vault
        .db()
        .query_row("SELECT start_line FROM blocks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(line, 6);
    assert_eq!(vault.count("blocks"), 1);
}

#[test]
fn the_fixture_block_ids_are_indexed() {
    let vault = Vault::fixture();
    vault.ok(&["index"]);
    let db = vault.db();
    let mut statement = db
        .prepare("SELECT block_id FROM blocks WHERE id = ? ORDER BY block_id")
        .unwrap();
    let blocks: Vec<String> = statement
        .query_map([id(2)], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(blocks, ["register", "send-user", "swap-table"]);
}
