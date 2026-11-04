//! Section-level pairs from end to end: the fake qmd finds them, a fake model
//! judges what it's shown, and `kbx accept` writes the anchors. Then the other
//! half of `docs/sections.md`: with no anchors in play, nothing changes.
//!
//! `note_level_output_matches_the_recorded_baseline` compares with a file that
//! a kbx from before this feature wrote. To record it again, put that kbx at
//! the `kbx` this test runs and set `KBX_RECORD_BASELINE`.
//!
//! The fakes are `sh`, so this file is Unix only.
#![cfg(unix)]

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use common::{FakeModel, Fixture, copy_dir, id, stderr};
use kbx_core::{cid, parse_note, split_frontmatter};
use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use serde_json::{Map, Value, json};

const OVERLAY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/sections");
const BASELINE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/note-level-baseline.json"
);

const SHED: u32 = 28;
const SESSION: u32 = 12;
const OVERLOAD: u32 = 29;
const MODEL: (&str, &str) = ("KBX_OLLAMA_MODEL", "qwen3:8b");

/// A model that accepts every pair under `rel`.
fn fake_judge(rel: &'static str) -> FakeModel {
    FakeModel::start(move |path, body| match path {
        "/api/tags" => (200, json!({"models": [{"name": "qwen3:8b"}]}).to_string()),
        "/api/chat" => {
            let body: Value = serde_json::from_str(body).unwrap();
            let batch = body["messages"][1]["content"].as_str().unwrap();
            let batch: Vec<Value> = serde_json::from_str(batch).unwrap();
            let rows = batch.iter().map(|pair| {
                json!({"cid": pair["cid"], "rationale": "Related.", "verdict": "accept",
                       "rel": rel, "confidence": 0.9})
            });
            let content = json!({"verdicts": rows.collect::<Vec<_>>()}).to_string();
            let message = json!({"role": "assistant", "content": content});
            let chat = json!({"message": message, "done": true, "done_reason": "stop"});
            (200, chat.to_string())
        }
        _ => (404, String::new()),
    })
}

/// What `judge` was shown, pair by pair.
fn pairs_shown(judge: &FakeModel) -> Vec<Value> {
    let mut pairs = Vec::new();
    for body in judge.bodies("/api/chat") {
        let body: Value = serde_json::from_str(&body).unwrap();
        let batch = body["messages"][1]["content"].as_str().unwrap();
        let batch: Vec<Value> = serde_json::from_str(batch).unwrap();
        pairs.extend(batch);
    }
    pairs
}

/// The fixture vault with the note holding `^shed-order` added.
fn vault() -> Fixture {
    let vault = Fixture::new();
    copy_dir(Path::new(OVERLAY), vault.root());
    vault
}

fn anchors_on(vault: &Fixture) {
    let config = vault.root().join("kbx.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str("\n[write]\nsection_anchors = true\n");
    fs::write(config, text).unwrap();
}

/// Answers every search with a hit on the line holding `^shed-order`.
fn qmd_finds_the_block(vault: &Fixture) {
    let handle = format!("qmd://operations/{}-shed-load.md", id(SHED).to_lowercase());
    let answer = vault.root().join(".kbx").join("qmd-answer.json");
    let json = format!("[{{\"score\":0.75,\"file\":\"{handle}\",\"line\":12}}]");
    fs::write(&answer, json).unwrap();
    vault
        .path
        .tool("qmd", &format!("/bin/cat \"{}\"", answer.display()));
}

/// A Concept whose `Response` section says the alias of the block's note.
fn write_overload(vault: &Fixture) {
    let text = format!(
        "---\nid: {}\ntype: Concept\ntitle: Overload\n---\nDemand above capacity.\n\n\
         ## Response\n\nUnder overload, load shedding protects the rest.\n",
        id(OVERLOAD)
    );
    vault.write(&format!("engineering/{}-overload.md", id(OVERLOAD)), &text);
}

fn suggest(vault: &Fixture, judge: &FakeModel, generator: &str, note: u32) {
    let note = id(note);
    let host = ("OLLAMA_HOST", judge.url.as_str());
    let args = [
        "suggest",
        "--judge",
        "ollama",
        "--generators",
        generator,
        "--note",
        &note,
    ];
    let output = vault.kbx_with(&args, &[host, MODEL]);
    assert!(output.status.success(), "{}", stderr(&output));
}

fn database(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

/// The `(cid, src_section, dst_section)` of the one candidate for a pair.
fn row(vault: &Fixture, a: u32, b: u32) -> (String, Option<String>, Option<String>) {
    database(vault)
        .query_row(
            "SELECT cid, src_section, dst_section FROM candidates
             WHERE (src = ?1 AND dst = ?2) OR (src = ?2 AND dst = ?1)",
            [id(a), id(b)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn path_of(vault: &Fixture, note: u32) -> PathBuf {
    let path: String = database(vault)
        .query_row("SELECT path FROM files WHERE id = ?", [id(note)], |row| {
            row.get(0)
        })
        .unwrap();
    vault.root().join(path)
}

fn text_of(vault: &Fixture, note: u32) -> String {
    fs::read_to_string(path_of(vault, note)).unwrap()
}

/// The links under `key` in a note's frontmatter, as written.
fn written(vault: &Fixture, note: u32, key: &str) -> Vec<String> {
    let note = parse_note("n.md", &text_of(vault, note)).unwrap();
    let links = note.relation(key).map_or(&[][..], |entry| &entry.links);
    let link = |link: &kbx_core::Link| match &link.fragment {
        Some(fragment) => format!("[[{}#{fragment}]]", link.target),
        None => format!("[[{}]]", link.target),
    };
    links.iter().map(link).collect()
}

fn body_of(text: &str) -> &str {
    &text[split_frontmatter(text).unwrap().unwrap().body_start..]
}

fn review_json(vault: &Fixture) -> Vec<Value> {
    serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap()
}

#[test]
fn a_qmd_hit_on_a_block_is_judged_and_written_with_its_anchor() {
    let vault = vault();
    anchors_on(&vault);
    vault.ok(&["index"]);
    qmd_finds_the_block(&vault);
    let judge = fake_judge("see_also");
    suggest(&vault, &judge, "qmd_knn", SESSION);

    let (cid_now, ..) = row(&vault, SESSION, SHED);
    assert_ne!(cid_now, cid(&id(SESSION), &id(SHED)));

    let shown = pairs_shown(&judge);
    assert_eq!(shown.len(), 1);
    let side = |note: u32| {
        let wanted = json!(id(note));
        let sides = [&shown[0]["src"], &shown[0]["dst"]];
        sides
            .into_iter()
            .find(|s| s["id"] == wanted)
            .unwrap()
            .clone()
    };
    let block_side = side(SHED);
    let excerpt = block_side["excerpt"].as_str().unwrap();
    assert!(
        excerpt.contains("Load shedding drops batch traffic"),
        "{excerpt}"
    );
    assert!(!excerpt.contains("What to do when"), "{excerpt}");
    assert!(!excerpt.contains("Admit traffic"), "{excerpt}");
    assert_eq!(block_side["title"], "Shed load");

    let queue = review_json(&vault);
    assert_eq!(queue.len(), 1);
    let anchors = [&queue[0]["src_anchor"], &queue[0]["dst_anchor"]];
    let address = format!("{}#^shed-order", id(SHED));
    assert!(anchors.iter().any(|a| **a == json!(address)), "{queue:?}");

    let bodies = (text_of(&vault, SESSION), text_of(&vault, SHED));
    vault.ok(&["accept", &cid_now]);

    let link = format!("[[{}-shed-load#^shed-order]]", id(SHED));
    assert_eq!(written(&vault, SESSION, "see_also"), [link]);
    assert_eq!(body_of(&text_of(&vault, SESSION)), body_of(&bodies.0));
    assert_eq!(body_of(&text_of(&vault, SHED)), body_of(&bodies.1));

    vault.ok(&["index"]);
    let fragment: String = database(&vault)
        .query_row(
            "SELECT fragment FROM links WHERE src = ? AND key = 'see_also' AND dst = ?",
            [id(SESSION), id(SHED)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fragment, "^shed-order");
    let neighbors = vault.ok(&["graph", "neighbors", &id(SESSION)]);
    assert!(neighbors.contains(&id(SHED)), "{neighbors}");
    assert!(!neighbors.contains("shed-order"), "{neighbors}");
    let lint = vault.kbx(&["lint", path_of(&vault, SESSION).to_str().unwrap()]);
    assert!(lint.status.success(), "{}", common::stdout(&lint));
    let again = vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);
    assert!(!row_exists(&vault, SESSION, SHED), "{again}");
}

fn row_exists(vault: &Fixture, a: u32, b: u32) -> bool {
    database(vault)
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM candidates
                 WHERE (src = ?1 AND dst = ?2) OR (src = ?2 AND dst = ?1))",
            [id(a), id(b)],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn an_alias_pair_anchors_each_note_at_the_section_of_the_other() {
    let vault = vault();
    anchors_on(&vault);
    write_overload(&vault);
    vault.ok(&["index"]);
    let judge = fake_judge("depends_on");
    suggest(&vault, &judge, "alias_cooccur", OVERLOAD);

    let (cid_now, src, dst) = row(&vault, OVERLOAD, SHED);
    assert!(src.is_some() && dst.is_some(), "{src:?} {dst:?}");
    let shown = pairs_shown(&judge);
    let text = shown[0].to_string();
    assert!(text.contains("Under overload"), "{text}");
    assert!(!text.contains("Demand above capacity"), "{text}");

    let queue = review_json(&vault);
    let heading = format!("{}#response", id(OVERLOAD));
    assert_eq!(queue[0]["src_anchor"], json!(heading), "{queue:?}");

    vault.ok(&["accept", &cid_now]);
    // The schema turns the pair round: only a Procedure can depend on anything.
    assert_eq!(
        written(&vault, SHED, "depends_on"),
        [
            format!("[[{}-platform]]", id(1)),
            format!("[[{}-overload#Response]]", id(OVERLOAD))
        ]
    );
    assert_eq!(
        written(&vault, OVERLOAD, "required_by"),
        [format!("[[{}-shed-load#^shed-order]]", id(SHED))]
    );
    vault.ok(&["index"]);
    let stats = vault.kbx(&["lint", path_of(&vault, SHED).to_str().unwrap()]);
    assert!(stats.status.success(), "{}", common::stdout(&stats));
}

#[test]
fn a_staged_line_keeps_its_anchors_through_sync_stage() {
    let vault = vault();
    anchors_on(&vault);
    write_overload(&vault);
    vault.ok(&["index"]);
    let judge = fake_judge("depends_on");
    let host = ("OLLAMA_HOST", judge.url.as_str());
    let note = id(OVERLOAD);
    let args = [
        "suggest",
        "--judge",
        "ollama",
        "--generators",
        "alias_cooccur",
        "--note",
        &note,
        "--stage",
    ];
    let output = vault.kbx_with(&args, &[host, MODEL]);
    assert!(output.status.success(), "{}", stderr(&output));

    let (cid_now, ..) = row(&vault, OVERLOAD, SHED);
    let link = format!("[[{}-overload#Response]]", id(OVERLOAD));
    let text = text_of(&vault, SHED);
    let line = text.lines().find(|l| l.contains(&cid_now)).unwrap();
    assert!(line.contains(&link), "{line}");

    // A person moves it: the link goes under the key and the line goes.
    let platform = format!("\"[[{}-platform]]\"", id(1));
    let moved: String = text
        .lines()
        .filter(|l| !l.contains(&cid_now))
        .map(|l| match l.contains(&platform) {
            true => format!("{}, \"{link}\"]\n", l.trim_end_matches(']')),
            false => format!("{l}\n"),
        })
        .collect();
    fs::write(path_of(&vault, SHED), moved).unwrap();

    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("1 accepted"), "{printed}");
    assert_eq!(
        written(&vault, SHED, "depends_on"),
        [format!("[[{}-platform]]", id(1)), link]
    );
    assert_eq!(
        written(&vault, OVERLOAD, "required_by"),
        [format!("[[{}-shed-load#^shed-order]]", id(SHED))]
    );
}

#[test]
fn a_verdict_is_about_its_sections() {
    let vault = vault();
    anchors_on(&vault);
    vault.ok(&["index"]);
    qmd_finds_the_block(&vault);
    vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);
    let (first, ..) = row(&vault, SESSION, SHED);

    let handle = format!("qmd://operations/{}-shed-load.md", id(SHED).to_lowercase());
    let json = format!("[{{\"score\":0.75,\"file\":\"{handle}\",\"line\":16}}]");
    fs::write(vault.root().join(".kbx/qmd-answer.json"), json).unwrap();
    vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);

    // One row for the pair, now about `## Recovery`.
    let (second, ..) = row(&vault, SESSION, SHED);
    assert_ne!(first, second);
}

#[test]
fn turning_anchors_off_takes_the_next_suggest_back_to_note_level() {
    let vault = vault();
    anchors_on(&vault);
    vault.ok(&["index"]);
    qmd_finds_the_block(&vault);
    vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);
    assert_ne!(row(&vault, SESSION, SHED).0, cid(&id(SESSION), &id(SHED)));

    let config = vault.root().join("kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        text.replace("section_anchors = true", "section_anchors = false"),
    )
    .unwrap();
    vault.ok(&["suggest", "--judge", "none", "--generators", "qmd_knn"]);

    assert_eq!(
        row(&vault, SESSION, SHED),
        (cid(&id(SESSION), &id(SHED)), None, None)
    );
}

#[test]
fn by_default_a_block_changes_nothing() {
    let vault = vault();
    write_overload(&vault);
    vault.ok(&["index"]);
    let judge = fake_judge("depends_on");
    suggest(&vault, &judge, "alias_cooccur", OVERLOAD);

    let pair = cid(&id(OVERLOAD), &id(SHED));
    assert_eq!(row(&vault, OVERLOAD, SHED), (pair.clone(), None, None));
    let text = pairs_shown(&judge)[0].to_string();
    assert!(text.contains("Demand above capacity"), "{text}");
    let queue = review_json(&vault);
    assert!(queue[0].get("src_anchor").is_none(), "{queue:?}");
    assert!(queue[0].get("dst_anchor").is_none(), "{queue:?}");

    vault.ok(&["accept", &pair]);
    let plain = format!("[[{}]]", id(OVERLOAD));
    assert_eq!(written(&vault, SHED, "depends_on")[1], plain);
    assert_eq!(
        written(&vault, OVERLOAD, "required_by"),
        [format!("[[{}]]", id(SHED))]
    );
}

#[test]
fn anchors_on_a_note_with_nothing_to_anchor_changes_nothing() {
    let vault = Fixture::new();
    anchors_on(&vault);
    vault.ok(&["index"]);
    // `…11` says an alias of `…06`, and neither has a heading or a block id.
    vault.ok(&[
        "suggest",
        "--judge",
        "none",
        "--generators",
        "alias_cooccur",
    ]);
    assert_eq!(row(&vault, 11, 6), (cid(&id(11), &id(6)), None, None));
    assert!(
        parse_note("n.md", &text_of(&vault, 6)).unwrap().sections[0]
            .heading_path
            .is_empty()
    );
}

#[test]
fn review_json_reads_a_database_from_before_the_section_columns() {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault.ok(&[
        "suggest",
        "--judge",
        "none",
        "--generators",
        "alias_cooccur",
    ]);
    let before = vault.ok(&["review", "--json"]);
    let db = database(&vault);
    db.execute_batch(
        "ALTER TABLE candidates DROP COLUMN src_section;
         ALTER TABLE candidates DROP COLUMN dst_section;",
    )
    .unwrap();
    drop(db);
    assert_eq!(vault.ok(&["review", "--json"]), before);
}

/// `sql`'s rows as JSON, one array per row.
fn dump(db: &Connection, sql: &str) -> Value {
    let mut statement = db.prepare(sql).unwrap();
    let width = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            let cells = (0..width).map(|at| match row.get::<_, Sql>(at).unwrap() {
                Sql::Null => Value::Null,
                Sql::Integer(number) => json!(number),
                Sql::Real(number) => json!(number),
                Sql::Text(text) => json!(text),
                Sql::Blob(_) => panic!("no table here holds blobs"),
            });
            Ok(Value::Array(cells.collect()))
        })
        .unwrap();
    Value::Array(rows.map(Result::unwrap).collect())
}

/// Everything a run with no anchors can be seen to produce, on the fixture
/// vault: the index, `kbx graph`, the candidates, the verdicts, the queue and
/// what accepting writes. Timestamps are left out.
fn note_level_run() -> Value {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    let handle = format!(
        "qmd://operations/{}-cookie-lifetime.md",
        id(13).to_lowercase()
    );
    let answer = vault.root().join(".kbx/qmd-answer.json");
    let json = format!("[{{\"score\":0.75,\"file\":\"{handle}\",\"line\":7}}]");
    fs::write(&answer, json).unwrap();
    vault
        .path
        .tool("qmd", &format!("/bin/cat \"{}\"", answer.display()));

    let judge = fake_judge("see_also");
    let host = ("OLLAMA_HOST", judge.url.as_str());
    let output = vault.kbx_with(&["suggest", "--judge", "ollama"], &[host, MODEL]);
    assert!(output.status.success(), "{}", stderr(&output));

    let mut run = Map::new();
    let db = database(&vault);
    let tables = [
        (
            "files",
            "SELECT path, id, type, title, content_hash, fm_hash, scope FROM files ORDER BY path",
        ),
        (
            "aliases",
            "SELECT alias, id FROM aliases ORDER BY alias, id",
        ),
        (
            "sections",
            "SELECT key, id, heading_path, block_id, start_line, end_line, text_hash, tokens FROM sections ORDER BY key",
        ),
        (
            "blocks",
            "SELECT id, block_id, section_key, start_line, end_line FROM blocks ORDER BY id, block_id",
        ),
        (
            "links",
            "SELECT src, key, target, fragment, origin, src_section, line, dst FROM links ORDER BY src, key, target, line",
        ),
        (
            "edges",
            "SELECT src, dst, rel, inverse_of, weight, origin, src_section, line FROM edges ORDER BY src, dst, rel, origin",
        ),
        (
            "candidates",
            "SELECT cid, src, dst, generators, features, score, src_hash, dst_hash FROM candidates ORDER BY cid",
        ),
        (
            "verdicts",
            "SELECT cid, backend, model, prompt_hash, verdict, rel, confidence, rationale, cost_usd, cached_tokens FROM verdicts ORDER BY cid",
        ),
    ];
    for (name, sql) in tables {
        run.insert(name.to_string(), dump(&db, sql));
    }
    drop(db);
    let review: Value = serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap();
    run.insert("review".to_string(), review);

    let accepted = cid(&id(11), &id(6));
    vault.ok(&["accept", &accepted, "--rel", "depends_on"]);
    vault.ok(&["index"]);
    for note in [6, 11] {
        run.insert(format!("note {note}"), json!(text_of(&vault, note)));
    }
    let queries: [&[&str]; 3] = [
        &["graph", "stats"],
        &["graph", "neighbors", &id(6)],
        &["graph", "path", &id(11), &id(1)],
    ];
    for args in queries {
        run.insert(args.join(" "), json!(vault.ok(args)));
    }
    Value::Object(run)
}

#[test]
fn note_level_output_matches_the_recorded_baseline() {
    let run = serde_json::to_string_pretty(&note_level_run()).unwrap() + "\n";
    if env::var_os("KBX_RECORD_BASELINE").is_some() {
        fs::write(BASELINE, run).unwrap();
        return;
    }
    let recorded = fs::read_to_string(BASELINE).unwrap();
    assert!(run == recorded, "the note-level output has changed");
}
