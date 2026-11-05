//! What the default keeps in `.kbx/`, and what `[write] frontmatter = true` and
//! `kbx apply` do with it. Nothing outside `.kbx/` changes until they're asked to.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{FIXTURE, FakePath, Fixture, copy_dir, stderr, stdout};
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SUGGEST: [&str; 5] = [
    "suggest",
    "--judge",
    "none",
    "--generators",
    "common_neighbors",
];

/// `a` and `b` are Notes, `k` a Concept, `p` a Procedure, which requires
/// `depends_on`, and `d1` to `d3` Decisions, between which `supersedes` is exclusive.
fn small() -> Fixture {
    let vault = Fixture::bare(FakePath::with_qmd());
    let note = |id: &str, class: &str| {
        let text = format!("---\nid: {id}\ntype: {class}\ntitle: {id}\n---\n\nBody of {id}.\n");
        vault.write(&format!("notes/{id}.md"), &text);
    };
    note("a", "Note");
    note("b", "Note");
    note("k", "Concept");
    note("p", "Procedure");
    for id in ["d1", "d2", "d3"] {
        note(id, "Decision");
    }
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    vault
}

/// The fixture vault with candidates, from the first command on.
fn suggested() -> Fixture {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    vault
}

fn db(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

/// Adds a candidate for the pair, as `kbx suggest` would have stored it.
fn candidate(vault: &Fixture, cid: &str, src: &str, dst: &str) {
    db(vault)
        .execute(
            "INSERT INTO candidates (cid, src, dst, generators, features, score, src_hash, dst_hash)
             SELECT ?1, a.id, b.id, '[]', '{}', 0.5, a.content_hash, b.content_hash
             FROM files a, files b WHERE a.id = ?2 AND b.id = ?3",
            params![cid, src, dst],
        )
        .unwrap();
}

fn first_cid(vault: &Fixture) -> String {
    let query = "SELECT cid FROM candidates ORDER BY score DESC, cid LIMIT 1";
    db(vault).query_row(query, [], |row| row.get(0)).unwrap()
}

/// `(src, dst, rel, origin)` of every edge that isn't a mention.
fn edges(vault: &Fixture) -> Vec<(String, String, String, String)> {
    let db = db(vault);
    let mut statement = db
        .prepare(
            "SELECT src, dst, rel, origin FROM edges WHERE origin <> 'inline'
             ORDER BY src, dst, rel, origin",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn edge(src: &str, dst: &str, rel: &str, origin: &str) -> (String, String, String, String) {
    (src.into(), dst.into(), rel.into(), origin.into())
}

/// What accepting `a -see_also-> b` leaves in the graph.
fn see_also() -> Vec<(String, String, String, String)> {
    vec![
        edge("a", "b", "see_also", "kbx"),
        edge("b", "a", "see_also", "derived"),
    ]
}

/// A hash of every file and the name of every folder outside `.kbx/`.
fn outside_kbx(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if relative == Path::new(".kbx") {
                continue;
            }
            if path.is_dir() {
                found.insert(relative, Vec::new());
                walk(root, &path, found);
            } else {
                let digest = Sha256::digest(fs::read(&path).unwrap());
                found.insert(relative, digest.to_vec());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

fn turn_writing_on(vault: &Fixture) {
    let config = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(config, format!("{text}\n[write]\nfrontmatter = true\n")).unwrap();
}

fn note(vault: &Fixture, id: &str) -> String {
    fs::read_to_string(vault.root().join(format!("notes/{id}.md"))).unwrap()
}

fn accepted_json(vault: &Fixture) -> Vec<Value> {
    let printed = vault.ok(&["review", "--accepted", "--json"]);
    serde_json::from_str(&printed).unwrap()
}

#[test]
fn init_index_suggest_accept_and_review_change_nothing_outside_kbx() {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    let before = outside_kbx(vault.root());

    vault.ok(&["init"]);
    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    let cid = first_cid(&vault);
    let printed = vault.ok(&["accept", &cid, "--rel", "see_also"]);
    assert!(printed.contains("not written"), "{printed}");
    assert!(
        printed.contains("+++ b/"),
        "the diff is still shown: {printed}"
    );
    vault.ok(&["review", "--json"]);
    vault.ok(&["review", "--accepted"]);
    // Without a terminal this stops at the terminal check, or at an empty queue.
    let _ = vault.kbx(&["review"]);

    assert_eq!(outside_kbx(vault.root()), before);
}

#[test]
fn an_accepted_relation_and_its_inverse_are_edges_that_survive_every_reindex() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    let before = outside_kbx(vault.root());

    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    assert_eq!(edges(&vault), see_also());

    vault.ok(&["index"]);
    assert_eq!(edges(&vault), see_also(), "a run that finds nothing new");
    vault.ok(&["index", "--full"]);
    assert_eq!(edges(&vault), see_also(), "a full rebuild");
    db(&vault)
        .execute("UPDATE meta SET v = '0' WHERE k = 'pipeline_version'", [])
        .unwrap();
    vault.ok(&["index"]);
    assert_eq!(edges(&vault), see_also(), "a rebuild for a new pipeline");

    vault.write("notes/c.md", "A new note.\n");
    vault.ok(&["index"]);
    assert_eq!(edges(&vault), see_also(), "a run that relinks");
    fs::remove_file(vault.root().join("notes/c.md")).unwrap();
    vault.ok(&["index"]);

    assert_eq!(outside_kbx(vault.root()), before);
}

#[test]
fn accepting_gives_the_same_edges_as_the_next_relink() {
    let vault = small();
    candidate(&vault, "c1", "k", "p");
    vault.ok(&["accept", "c1", "--rel", "depends_on"]);
    let accepted = edges(&vault);
    assert_eq!(
        accepted,
        vec![
            edge("k", "p", "required_by", "derived"),
            edge("p", "k", "depends_on", "kbx"),
        ],
        "the schema turned it around"
    );
    let rows = |vault: &Fixture| {
        let db = db(vault);
        let sql = "SELECT src, rel, inverse_of, weight, origin FROM edges ORDER BY src, rel";
        let mut statement = db.prepare(sql).unwrap();
        let rows = statement.query_map([], |row| {
            let columns: (String, String, Option<String>, Option<f64>, String) = (
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            );
            Ok(columns)
        });
        rows.unwrap().map(Result::unwrap).collect::<Vec<_>>()
    };
    let direct = rows(&vault);
    vault.ok(&["index", "--full"]);
    assert_eq!(rows(&vault), direct);
}

#[test]
fn review_export_and_stats_show_an_accepted_relation() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);

    let listed = accepted_json(&vault);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["cid"], "c1");
    assert_eq!(listed[0]["src"], "a");
    assert_eq!(listed[0]["dst"], "b");
    assert_eq!(listed[0]["rel"], "see_also");
    assert!(
        vault
            .ok(&["review", "--accepted"])
            .contains("a -see_also-> b")
    );
    let summary = vault.ok(&["review"]);
    assert!(summary.contains("1 accepted relation in .kbx"), "{summary}");
    assert_eq!(vault.ok(&["review", "--json"]).trim(), "[]");

    let exported = vault.ok(&["export", "edges", "--jsonl"]);
    let rows: Vec<Value> = exported
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let found = |src: &str, origin: &str| {
        let hit = |row: &&Value| row["src"] == src && row["origin"] == origin;
        rows.iter().find(hit).is_some()
    };
    assert!(found("a", "kbx"), "{exported}");
    assert!(found("b", "derived"), "{exported}");
    let rdf = vault.ok(&["export", "rdf"]);
    assert!(rdf.contains("<urn:kbx:note:a>"), "{rdf}");
    assert!(rdf.contains("see_also> <urn:kbx:note:b>"), "{rdf}");

    let stats = vault.ok(&["graph", "stats"]);
    assert!(stats.contains("edges 2"), "{stats}");
    assert!(stats.contains("accepted in .kbx 1"), "{stats}");
    assert!(vault.ok(&["graph", "neighbors", "a"]).contains("see_also"));
}

#[test]
fn generators_see_an_accepted_pair_as_linked_and_keep_it_for_taking_back() {
    let vault = suggested();
    let cid = first_cid(&vault);
    vault.ok(&["accept", &cid, "--rel", "see_also"]);
    let pair: (String, String) = db(&vault)
        .query_row(
            "SELECT src, dst FROM candidates WHERE cid = ?",
            [&cid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    let queue = vault.ok(&["review", "--json"]);
    assert!(!queue.contains(&cid), "it's decided: {queue}");

    vault.ok(&SUGGEST);
    let queue = vault.ok(&["review", "--json"]);
    assert!(!queue.contains(&cid), "not proposed again: {queue}");

    // The candidate survived that, so the decision can still be taken back.
    vault.ok(&["reject", &cid]);
    assert!(accepted_json(&vault).is_empty());
    let left = edges(&vault);
    let holds = |row: &(String, String, String, String)| {
        (row.0 == pair.0 && row.1 == pair.1) || (row.0 == pair.1 && row.1 == pair.0)
    };
    assert!(!left.iter().any(holds), "{left:?}");
}

#[test]
fn lint_takes_an_accepted_relation_as_meeting_what_a_class_requires() {
    let vault = small();
    let lint = vault.kbx(&["lint"]);
    assert!(stdout(&lint).contains("schema.gap"), "{}", stdout(&lint));

    candidate(&vault, "c1", "p", "k");
    vault.ok(&["accept", "c1", "--rel", "depends_on"]);

    let lint = vault.kbx(&["lint"]);
    assert!(lint.status.success(), "{}", stdout(&lint));
    assert!(!stdout(&lint).contains("schema.gap"), "{}", stdout(&lint));
}

#[test]
fn an_exclusive_relation_counts_targets_kept_in_kbx() {
    let vault = small();
    candidate(&vault, "c1", "d1", "d2");
    candidate(&vault, "c2", "d1", "d3");
    vault.ok(&["accept", "c1", "--rel", "supersedes"]);

    let second = vault.kbx(&["accept", "c2", "--rel", "supersedes"]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("rel.cardinality"),
        "{}",
        stderr(&second)
    );
    assert_eq!(accepted_json(&vault).len(), 1);
}

#[test]
fn a_note_stating_the_relation_wins_and_owns_it_from_then_on() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);

    let plain = note(&vault, "a");
    let linked = plain.replace("title: a\n", "title: a\nsee_also: [\"[[b]]\"]\n");
    vault.write("notes/a.md", &linked);
    vault.ok(&["index"]);
    assert_eq!(
        edges(&vault),
        vec![
            edge("a", "b", "see_also", "frontmatter"),
            edge("b", "a", "see_also", "derived"),
        ],
        "one relation, from the note"
    );
    assert!(accepted_json(&vault).is_empty());

    vault.write("notes/a.md", &plain);
    vault.ok(&["index"]);
    assert!(edges(&vault).is_empty(), "taking the link out takes it out");
}

#[test]
fn a_note_stating_the_inverse_wins_as_well() {
    let vault = small();
    candidate(&vault, "c1", "p", "k");
    vault.ok(&["accept", "c1", "--rel", "depends_on"]);
    let text = note(&vault, "k").replace("title: k\n", "title: k\nrequired_by: [\"[[p]]\"]\n");
    vault.write("notes/k.md", &text);
    vault.ok(&["index"]);
    assert!(accepted_json(&vault).is_empty());
    assert_eq!(
        edges(&vault),
        vec![
            edge("k", "p", "required_by", "frontmatter"),
            edge("p", "k", "depends_on", "derived"),
        ]
    );
}

#[test]
fn a_relation_whose_note_is_gone_has_no_edge_and_returns_with_the_note() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    let text = note(&vault, "b");
    fs::remove_file(vault.root().join("notes/b.md")).unwrap();
    vault.ok(&["index"]);
    assert!(edges(&vault).is_empty());
    assert_eq!(accepted_json(&vault).len(), 1);

    vault.write("notes/b.md", &text);
    vault.ok(&["index"]);
    assert_eq!(edges(&vault), see_also());
}

#[test]
fn stage_and_sync_stage_refuse_until_the_setting_is_on() {
    let vault = small();
    let before = outside_kbx(vault.root());
    for args in [
        &["suggest", "--stage", "--judge", "none"][..],
        &["sync-stage"][..],
        &["sync-stage", "--mark-reviewed"][..],
    ] {
        let output = vault.kbx(args);
        assert!(!output.status.success(), "{args:?}");
        let error = stderr(&output);
        assert!(error.contains("frontmatter = true"), "{args:?}: {error}");
        assert!(error.contains("kbx.toml"), "{args:?}: {error}");
    }
    assert_eq!(outside_kbx(vault.root()), before);
}

#[test]
fn a_line_staged_earlier_stays_while_the_setting_is_off() {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    vault.ok(&["init"]);
    let config = vault.root().join(".kbx/kbx.toml");
    let off = fs::read_to_string(&config).unwrap();
    turn_writing_on(&vault);
    vault.ok(&["index"]);
    let mut staged = SUGGEST.to_vec();
    staged.push("--stage");
    vault.ok(&staged);
    let cid: String = db(&vault)
        .query_row("SELECT cid FROM staged LIMIT 1", [], |row| row.get(0))
        .unwrap();

    fs::write(&config, off).unwrap();
    let before = outside_kbx(vault.root());
    vault.ok(&["reject", &cid]);
    assert_eq!(outside_kbx(vault.root()), before);
}

#[test]
fn a_lock_can_live_in_kbx_and_the_notes_content_lifts_it() {
    let vault = small();
    let before = outside_kbx(vault.root());
    vault.ok(&["mark-reviewed", "a"]);
    assert_eq!(outside_kbx(vault.root()), before);
    let locked = |vault: &Fixture| -> i64 {
        db(vault)
            .query_row(
                "SELECT COUNT(*) FROM locks WHERE id = 'a' AND baseline = current",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(locked(&vault), 1);

    vault.ok(&["index", "--full"]);
    assert_eq!(locked(&vault), 1, "a rebuild keeps it");

    vault.write("notes/a.md", &format!("{}More.\n", note(&vault, "a")));
    vault.ok(&["index"]);
    assert_eq!(locked(&vault), 0, "an edit lifts it");
}

#[test]
fn mark_reviewed_writes_the_date_with_the_setting_on() {
    let vault = small();
    turn_writing_on(&vault);
    vault.ok(&["mark-reviewed", "a"]);
    assert!(
        note(&vault, "a").contains("reviewed:"),
        "{}",
        note(&vault, "a")
    );
}

#[test]
fn apply_refuses_without_the_setting_and_its_dry_run_changes_nothing() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    let before = outside_kbx(vault.root());

    let refused = vault.kbx(&["apply"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("frontmatter = true"),
        "{}",
        stderr(&refused)
    );

    let printed = vault.ok(&["apply", "--dry-run"]);
    assert!(printed.contains("+see_also: [\"[[b]]\"]"), "{printed}");
    assert!(printed.contains("+see_also: [\"[[a]]\"]"), "{printed}");
    assert!(printed.contains("would write a -see_also-> b"), "{printed}");

    turn_writing_on(&vault);
    assert!(vault.ok(&["apply", "--dry-run"]).contains("would write"));
    assert_eq!(outside_kbx(vault.root()), before);
    assert_eq!(accepted_json(&vault).len(), 1);
    assert_eq!(edges(&vault), see_also());
}

#[test]
fn apply_writes_what_accepting_with_the_setting_on_would_have() {
    let held = small();
    let direct = small();
    turn_writing_on(&direct);
    for vault in [&held, &direct] {
        candidate(vault, "c1", "a", "b");
        candidate(vault, "c2", "a", "p");
    }

    held.ok(&["accept", "c1", "c2", "--rel", "see_also"]);
    turn_writing_on(&held);
    let printed = held.ok(&["apply"]);
    assert!(printed.contains("wrote a -see_also-> b"), "{printed}");

    direct.ok(&["accept", "c1", "c2", "--rel", "see_also"]);

    for id in ["a", "b", "p"] {
        assert_eq!(note(&held, id), note(&direct, id), "{id}");
    }
    assert!(accepted_json(&held).is_empty());
    assert_eq!(edges(&held), edges(&direct));
    assert!(!edges(&held).iter().any(|row| row.3 == "kbx"));
    assert_eq!(held.ok(&["apply"]).trim(), "no note would change");
    let queue = held.ok(&["review", "--json"]);
    assert_eq!(queue.trim(), "[]", "nothing left to write");
}

#[test]
fn apply_follows_an_edit_the_index_has_seen_and_refuses_one_it_hasnt() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    turn_writing_on(&vault);
    vault.write("notes/a.md", &format!("{}Edited.\n", note(&vault, "a")));

    let refused = vault.kbx(&["apply"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("changed since"),
        "{}",
        stderr(&refused)
    );
    assert!(!note(&vault, "a").contains("see_also"));
    assert_eq!(accepted_json(&vault).len(), 1);

    vault.ok(&["index"]);
    vault.ok(&["apply"]);
    assert!(
        note(&vault, "a").contains("see_also"),
        "{}",
        note(&vault, "a")
    );
    assert!(note(&vault, "a").contains("Edited."));
}

#[test]
fn apply_links_a_note_with_an_inferred_id_by_its_path() {
    let vault = Fixture::bare(FakePath::with_qmd());
    vault.write(
        "Projects/Alpha Plan.md",
        "# Alpha Plan\n\nNo frontmatter.\n",
    );
    vault.write(
        "Projects/Beta.md",
        "---\nid: beta\ntype: Note\ntitle: Beta\n---\n\nText.\n",
    );
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let id: String = db(&vault)
        .query_row(
            "SELECT id FROM files WHERE path = 'Projects/Alpha Plan.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    candidate(&vault, "c1", "beta", &id);
    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    turn_writing_on(&vault);
    vault.ok(&["apply"]);

    let beta = fs::read_to_string(vault.root().join("Projects/Beta.md")).unwrap();
    assert!(
        beta.contains("see_also: [\"[[Projects/Alpha Plan]]\"]"),
        "{beta}"
    );
    let alpha = fs::read_to_string(vault.root().join("Projects/Alpha Plan.md")).unwrap();
    assert!(
        alpha.starts_with("---\nsee_also: [\"[[beta]]\"]\n---\n"),
        "{alpha}"
    );
}

#[test]
fn no_write_keeps_one_acceptance_in_kbx_with_the_setting_on() {
    let vault = small();
    turn_writing_on(&vault);
    candidate(&vault, "c1", "a", "b");
    let before = outside_kbx(vault.root());
    vault.ok(&["accept", "c1", "--rel", "see_also", "--no-write"]);
    assert_eq!(outside_kbx(vault.root()), before);
    assert_eq!(edges(&vault), see_also());

    vault.ok(&["accept", "c1"]);
    assert!(
        note(&vault, "a").contains("see_also"),
        "accepting again writes it"
    );
    assert!(accepted_json(&vault).is_empty());
    assert!(!edges(&vault).iter().any(|row| row.3 == "kbx"));
}

#[test]
fn an_edges_table_from_before_origin_kbx_is_upgraded() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    let db = db(&vault);
    db.execute_batch(
        "DROP TABLE edges;
         CREATE TABLE edges (src TEXT, dst TEXT, rel TEXT, inverse_of TEXT, weight REAL,
             origin TEXT CHECK(origin IN ('frontmatter','inline','derived')),
             src_section TEXT, line INTEGER, PRIMARY KEY(src, dst, rel, origin));
         CREATE INDEX edges_dst ON edges(dst);
         INSERT INTO edges (src, dst, rel, origin) VALUES ('a', 'p', 'mentions', 'inline');
         ALTER TABLE locks DROP COLUMN held;",
    )
    .unwrap();
    drop(db);

    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    assert_eq!(edges(&vault), see_also());
    let mentions: i64 = self::db(&vault)
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE origin = 'inline'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mentions, 1, "the rows came along");
}

#[test]
fn a_relation_left_behind_by_a_rename_is_listed_reported_and_can_be_cleared() {
    let vault = Fixture::bare(FakePath::with_qmd());
    vault.write("Projects/Alpha.md", "# Alpha\n\nNo frontmatter.\n");
    vault.write(
        "notes/b.md",
        "---\nid: b\ntype: Note\ntitle: b\n---\n\nText.\n",
    );
    vault.ok(&["init"]);
    vault.ok(&["index"]);
    let id: String = db(&vault)
        .query_row(
            "SELECT id FROM files WHERE path = 'Projects/Alpha.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    candidate(&vault, "c1", "b", &id);
    vault.ok(&["accept", "c1", "--rel", "see_also"]);

    let root = vault.root();
    fs::rename(
        root.join("Projects/Alpha.md"),
        root.join("Projects/Beta.md"),
    )
    .unwrap();
    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    assert!(edges(&vault).is_empty());
    assert_eq!(accepted_json(&vault).len(), 1);

    turn_writing_on(&vault);
    let refused = vault.kbx(&["apply"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("unavailable"),
        "{}",
        stderr(&refused)
    );

    vault.ok(&["reject", "c1"]);
    assert!(accepted_json(&vault).is_empty());
}

#[test]
fn accepting_again_with_another_relation_changes_what_kbx_holds() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    vault.ok(&["accept", "c1", "--rel", "part_of"]);
    assert_eq!(
        edges(&vault),
        vec![
            edge("a", "b", "part_of", "kbx"),
            edge("b", "a", "has_part", "derived"),
        ]
    );
    let reviews: i64 = db(&vault)
        .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
        .unwrap();
    assert_eq!(reviews, 1);
}

#[test]
fn accepting_again_after_an_indexed_edit_is_allowed_and_an_unindexed_one_refused() {
    let vault = small();
    candidate(&vault, "c1", "a", "b");
    vault.ok(&["accept", "c1", "--rel", "see_also"]);
    vault.write("notes/a.md", &format!("{}Edited.\n", note(&vault, "a")));
    let refused = vault.kbx(&["accept", "c1"]);
    assert!(!refused.status.success(), "{}", stdout(&refused));
    vault.ok(&["index"]);
    vault.ok(&["accept", "c1"]);
    assert_eq!(edges(&vault), see_also());
}

#[test]
fn sync_stage_clears_the_lines_of_what_was_decided_with_the_setting_off() {
    let vault = Fixture::bare(FakePath::with_qmd());
    copy_dir(Path::new(FIXTURE), vault.root());
    vault.ok(&["init"]);
    let config = vault.root().join(".kbx/kbx.toml");
    let off = fs::read_to_string(&config).unwrap();
    turn_writing_on(&vault);
    let on = fs::read_to_string(&config).unwrap();
    vault.ok(&["index"]);
    let mut staged = SUGGEST.to_vec();
    staged.push("--stage");
    vault.ok(&staged);
    let cids: Vec<String> = {
        let db = db(&vault);
        let mut statement = db.prepare("SELECT cid FROM staged ORDER BY cid").unwrap();
        let rows = statement.query_map([], |row| row.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    };
    let (rejected, accepted) = (&cids[0], &cids[1]);

    fs::write(&config, off).unwrap();
    vault.ok(&["reject", rejected]);
    vault.ok(&["accept", accepted, "--rel", "see_also"]);

    fs::write(&config, on).unwrap();
    vault.ok(&["sync-stage"]);
    let mut text = String::new();
    for path in outside_kbx(vault.root()).keys() {
        if path.extension().is_some_and(|ext| ext == "md") {
            text.push_str(&fs::read_to_string(vault.root().join(path)).unwrap());
        }
    }
    assert!(!text.contains(rejected.as_str()), "a rejected line goes");
    assert!(text.contains(accepted.as_str()), "an unwritten one waits");
    vault.ok(&["apply"]);
    let mut text = String::new();
    for path in outside_kbx(vault.root()).keys() {
        if path.extension().is_some_and(|ext| ext == "md") {
            text.push_str(&fs::read_to_string(vault.root().join(path)).unwrap());
        }
    }
    assert!(
        !text.contains(accepted.as_str()),
        "writing it takes the line"
    );
}
