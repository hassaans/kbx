//! Drives `kbx bakeoff` on the fixture vault with fake judges: a fake Ollama
//! serves two models that answer differently, and on Unix a fake `claude`
//! prices its calls. No test here may reach a real model.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::{FakeModel, Fixture, id, stderr, stdout};
use rusqlite::Connection;
use serde_json::{Value, json};

const LABELS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/bakeoff/synthetic-labels.jsonl"
);
const PAIRS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/bakeoff/pairs.jsonl"
);

const BOTH: &str = "ollama:qwen3:14b,ollama:qwen3:8b";
const BEGIN: &str = "<!-- bakeoff:begin -->";
const END: &str = "<!-- bakeoff:end -->";

/// The pair of two fixture notes, as `kbx_core::cid` has it.
fn cid(a: u32, b: u32) -> String {
    kbx_core::cid(&id(a), &id(b))
}

/// What the fake answers about a pair: every model gets the accepted pairs
/// right, but `qwen3:8b` is unsure of one, names the wrong relation for another
/// and accepts one a person rejected.
fn verdict(model: &str, pair: &str) -> (&'static str, &'static str) {
    let small = model == "qwen3:8b";
    if pair == cid(2, 3) {
        return ("accept", if small { "see_also" } else { "depends_on" });
    }
    if pair == cid(2, 4) {
        return if small {
            ("unsure", "none")
        } else {
            ("accept", "depends_on")
        };
    }
    if pair == cid(1, 5) {
        return ("accept", "part_of");
    }
    if small && pair == cid(6, 14) {
        return ("accept", "see_also");
    }
    ("reject", "none")
}

fn fake_ollama(models: &'static [&'static str]) -> FakeModel {
    FakeModel::start(move |path, body| match path {
        "/api/tags" => {
            let rows: Vec<Value> = models.iter().map(|name| json!({"name": name})).collect();
            (200, json!({"models": rows}).to_string())
        }
        "/api/chat" => {
            let body: Value = serde_json::from_str(body).unwrap();
            let model = body["model"].as_str().unwrap();
            let batch = body["messages"][1]["content"].as_str().unwrap();
            let batch: Vec<Value> = serde_json::from_str(batch).unwrap();
            let rows = batch.iter().map(|pair| {
                let (decision, rel) = verdict(model, pair["cid"].as_str().unwrap());
                json!({"cid": pair["cid"], "rationale": "r", "verdict": decision,
                       "rel": rel, "confidence": 0.5})
            });
            let content = json!({"verdicts": rows.collect::<Vec<_>>()}).to_string();
            let message = json!({"role": "assistant", "content": content});
            let chat = json!({"message": message, "done": true, "done_reason": "stop"});
            (200, chat.to_string())
        }
        _ => (404, String::new()),
    })
}

fn indexed() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault
}

/// A report file in the vault, with something on either side of the markers.
fn doc(vault: &Fixture) -> PathBuf {
    let path = vault.root().join("report.md");
    fs::write(&path, format!("before\n\n{BEGIN}\nold\n{END}\n\nafter\n")).unwrap();
    path
}

fn run(vault: &Fixture, doc: &Path, models: &str, host: &str) -> std::process::Output {
    vault.kbx_with(
        &[
            "bakeoff",
            "--labels",
            LABELS,
            "--doc",
            doc.to_str().unwrap(),
            "--models",
            models,
            "--batch",
            "10",
        ],
        &[("OLLAMA_HOST", host)],
    )
}

fn table_row<'a>(text: &'a str, start: &str) -> Vec<&'a str> {
    let row = text.lines().find(|line| line.starts_with(start));
    let row = row.unwrap_or_else(|| panic!("no `{start}` row in:\n{text}"));
    row.trim_matches('|').split('|').map(str::trim).collect()
}

/// `(table, column)` of a report's first table, by header.
fn cell<'a>(text: &'a str, start: &str, header: &str) -> &'a str {
    let columns = table_row(text, "| Backend");
    let at = columns.iter().position(|name| *name == header).unwrap();
    table_row(text, start)[at]
}

fn database(vault: &Fixture, file: &str) -> Connection {
    Connection::open(vault.root().join(".kbx").join(file)).unwrap()
}

fn count(db: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    db.query_row(&sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn each_model_is_scored_against_the_labels_between_the_markers() {
    let vault = indexed();
    let model = fake_ollama(&["qwen3:14b", "qwen3:8b"]);
    let doc = doc(&vault);
    let output = run(&vault, &doc, BOTH, &model.url);
    assert!(output.status.success(), "{}", stderr(&output));

    let text = fs::read_to_string(&doc).unwrap();
    assert!(text.starts_with("before\n\n"), "{text}");
    assert!(text.ends_with("\n\nafter\n"), "{text}");
    assert!(!text.contains("old"), "{text}");
    // A deferred pair is no label.
    assert!(
        text.contains("6 labelled pairs: 3 accept, 3 reject"),
        "{text}"
    );

    let big = "| ollama | qwen3:14b";
    assert_eq!(cell(&text, big, "Answered"), "6/6");
    assert_eq!(cell(&text, big, "Agreement"), "100.0% (6/6)");
    assert_eq!(cell(&text, big, "Accept"), "100.0% (3/3)");
    assert_eq!(cell(&text, big, "Reject"), "100.0% (3/3)");
    assert_eq!(cell(&text, big, "Relation"), "100.0% (3/3)");
    assert_eq!(cell(&text, big, "Unsure"), "0.0% (0/6)");
    assert_eq!(cell(&text, big, "Cost"), "$0.0000");

    let small = "| ollama | qwen3:8b";
    assert_eq!(cell(&text, small, "Agreement"), "66.7% (4/6)");
    assert_eq!(cell(&text, small, "Accept"), "66.7% (2/3)");
    assert_eq!(cell(&text, small, "Reject"), "66.7% (2/3)");
    assert_eq!(cell(&text, small, "Relation"), "33.3% (1/3)");
    assert_eq!(cell(&text, small, "Unsure"), "16.7% (1/6)");

    assert_eq!(
        table_row(&text, "| depends_on"),
        ["depends_on", "2", "2/2", "0/2"]
    );
    assert_eq!(
        table_row(&text, "| part_of"),
        ["part_of", "1", "1/1", "1/1"]
    );
    assert!(stdout(&output).contains("| ollama | qwen3:8b"));
}

#[test]
fn a_rerun_is_free_and_writes_the_same_report() {
    let vault = indexed();
    let model = fake_ollama(&["qwen3:14b", "qwen3:8b"]);
    let doc = doc(&vault);
    assert!(run(&vault, &doc, BOTH, &model.url).status.success());
    let first = fs::read_to_string(&doc).unwrap();
    let calls = model.bodies("/api/chat").len();
    assert_eq!(calls, 2, "one batch for each model");

    assert!(run(&vault, &doc, BOTH, &model.url).status.success());
    assert_eq!(model.bodies("/api/chat").len(), calls);
    assert_eq!(fs::read_to_string(&doc).unwrap(), first);
}

#[test]
fn the_named_model_is_asked_for_whatever_the_machine_or_the_environment_says() {
    let vault = indexed();
    let model = fake_ollama(&["qwen3:14b", "qwen3:8b"]);
    let doc = doc(&vault);
    let host = ("OLLAMA_HOST", model.url.as_str());
    let output = vault.kbx_with(
        &[
            "bakeoff",
            "--labels",
            LABELS,
            "--doc",
            doc.to_str().unwrap(),
            "--models",
            BOTH,
        ],
        &[host, ("KBX_OLLAMA_MODEL", "something-else")],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let asked: BTreeSet<String> = model
        .bodies("/api/chat")
        .iter()
        .map(|body| {
            let body: Value = serde_json::from_str(body).unwrap();
            body["model"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        asked,
        BTreeSet::from(["qwen3:14b".into(), "qwen3:8b".into()])
    );
}

#[test]
fn the_judges_see_nothing_of_the_labels() {
    let vault = indexed();
    let model = fake_ollama(&["qwen3:14b"]);
    let doc = doc(&vault);
    assert!(
        run(&vault, &doc, "ollama:qwen3:14b", &model.url)
            .status
            .success()
    );
    let body: Value = serde_json::from_str(&model.bodies("/api/chat")[0]).unwrap();
    let batch = body["messages"][1]["content"].as_str().unwrap();
    let batch: Vec<Value> = serde_json::from_str(batch).unwrap();
    assert_eq!(batch.len(), 6);
    let cids: Vec<&str> = batch.iter().map(|p| p["cid"].as_str().unwrap()).collect();
    let mut sorted = cids.clone();
    sorted.sort();
    assert_eq!(cids, sorted, "in cid order, not in label order");
    for pair in &batch {
        assert_eq!(pair["why_proposed"], json!([]));
        let (src, dst) = (pair["src"]["id"].as_str(), pair["dst"]["id"].as_str());
        assert!(
            src < dst,
            "a person's direction would say what they decided"
        );
    }
}

#[test]
fn the_vaults_own_tables_are_left_alone() {
    let vault = indexed();
    vault.ok(&["suggest", "--judge", "none", "--generators", "schema_gap"]);
    let before = count(&database(&vault, "graph.sqlite"), "candidates");
    assert!(before > 0);
    let model = fake_ollama(&["qwen3:14b"]);
    let doc = doc(&vault);
    assert!(
        run(&vault, &doc, "ollama:qwen3:14b", &model.url)
            .status
            .success()
    );

    let graph = database(&vault, "graph.sqlite");
    assert_eq!(count(&graph, "verdicts"), 0, "`kbx review` reads these");
    assert_eq!(count(&graph, "candidates"), before);
    assert_eq!(count(&database(&vault, "bakeoff.sqlite"), "verdicts"), 6);
}

#[test]
fn a_model_that_cant_run_is_named_and_the_rest_carry_on() {
    let vault = indexed();
    let model = fake_ollama(&["qwen3:14b"]);
    let doc = doc(&vault);
    let output = run(&vault, &doc, BOTH, &model.url);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("qwen3:8b"), "{}", stderr(&output));

    let text = fs::read_to_string(&doc).unwrap();
    assert_eq!(
        cell(&text, "| ollama | qwen3:14b", "Agreement"),
        "100.0% (6/6)"
    );
    assert_eq!(cell(&text, "| ollama | qwen3:8b", "Answered"), "0/6");
    assert_eq!(cell(&text, "| ollama | qwen3:8b", "Notes"), "NotInstalled");
}

#[test]
fn nothing_answered_leaves_the_report_alone() {
    let vault = indexed();
    let model = fake_ollama(&[]);
    let doc = doc(&vault);
    let before = fs::read_to_string(&doc).unwrap();
    let output = run(&vault, &doc, BOTH, &model.url);
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&doc).unwrap(), before);
}

#[test]
fn a_report_without_the_markers_is_refused_before_anything_is_spent() {
    let vault = indexed();
    let model = fake_ollama(&["qwen3:14b"]);
    let doc = vault.root().join("report.md");
    fs::write(&doc, "no markers\n").unwrap();
    let output = run(&vault, &doc, "ollama:qwen3:14b", &model.url);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("bakeoff:begin"),
        "{}",
        stderr(&output)
    );
    assert!(model.bodies("/api/chat").is_empty());
    assert_eq!(fs::read_to_string(&doc).unwrap(), "no markers\n");
}

#[test]
fn an_unknown_backend_is_refused() {
    let vault = indexed();
    let doc = doc(&vault);
    let output = run(&vault, &doc, "gpt-cli:whatever", common::CLOSED_PORT);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("gpt-cli"), "{}", stderr(&output));
}

#[test]
fn llama_server_cant_be_given_a_model_it_wouldnt_load() {
    let vault = indexed();
    let doc = doc(&vault);
    let output = run(
        &vault,
        &doc,
        "llama-server:a,llama-server:b",
        common::CLOSED_PORT,
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("llama-server"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_label_with_an_unknown_decision_is_refused_by_line() {
    let vault = indexed();
    let doc = doc(&vault);
    let labels = vault.root().join("labels.jsonl");
    let row = json!({"src": id(2), "dst": id(3), "rel": "none", "decision": "maybe"});
    fs::write(&labels, format!("\n{row}\n")).unwrap();
    let output = vault.kbx(&[
        "bakeoff",
        "--labels",
        labels.to_str().unwrap(),
        "--doc",
        doc.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("line 2"), "{}", stderr(&output));
}

/// The rows of a `--prepare` run on the fixture vault.
fn prepare(vault: &Fixture, extra: &[&str]) -> Vec<Value> {
    let generators = "schema_gap,common_neighbors,adamic_adar,alias_cooccur,two_hop_bridge";
    let mut args = vec!["bakeoff", "--prepare", "--generators", generators];
    args.extend(extra);
    let text = vault.ok(&args);
    let rows = text.lines().map(|line| serde_json::from_str(line).unwrap());
    rows.collect()
}

mod prepare {
    use super::*;

    #[test]
    fn its_rows_read_as_reviews_with_nothing_decided() {
        let vault = indexed();
        let rows = prepare(&vault, &["--pairs", "12"]);
        assert_eq!(rows.len(), 12);
        for row in &rows {
            assert_eq!(row["decision"], Value::Null);
            assert_eq!(row["rel"], Value::Null);
            assert_eq!(row["reason"], Value::Null);
            assert_eq!(row["strikes"], 0);
            assert_eq!(row["decided_at"], Value::Null);
            let (src, dst) = (row["src"].as_str().unwrap(), row["dst"].as_str().unwrap());
            assert_eq!(row["cid"], kbx_core::cid(src, dst));
            assert!(row["src_type"].is_string() && row["dst_type"].is_string());
            assert!(!row["generators"].as_array().unwrap().is_empty());
            assert!(row["score"].is_number());
            for side in ["src_excerpt", "dst_excerpt"] {
                let words = row[side].as_str().unwrap().split_whitespace().count();
                assert!(words <= 20, "{row}");
            }
        }
    }

    #[test]
    fn it_spreads_over_generators_type_pairs_and_notes() {
        let vault = indexed();
        let rows = prepare(&vault, &["--pairs", "12"]);
        let spread = |key: &dyn Fn(&Value) -> String| -> usize {
            rows.iter().map(key).collect::<BTreeSet<String>>().len()
        };
        assert!(spread(&|row| row["generators"].to_string()) >= 2);
        assert!(spread(&|row| format!("{}{}", row["src_type"], row["dst_type"])) >= 2);
        let mut uses = std::collections::BTreeMap::new();
        for row in &rows {
            for side in ["src", "dst"] {
                *uses.entry(row[side].to_string()).or_insert(0) += 1;
            }
        }
        assert!(uses.values().all(|n| *n <= 3), "{uses:?}");
    }

    #[test]
    fn it_gives_the_same_pairs_every_time_and_changes_nothing() {
        let vault = indexed();
        let hash = |path: PathBuf| fs::read(path).unwrap();
        let graph = vault.root().join(".kbx/graph.sqlite");
        let before = hash(graph.clone());
        let first = prepare(&vault, &["--pairs", "12"]);
        assert_eq!(prepare(&vault, &["--pairs", "12"]), first);
        assert_eq!(hash(graph), before);
    }

    #[test]
    fn a_synthetic_excerpt_quotes_no_note() {
        let vault = indexed();
        let rows = prepare(&vault, &["--pairs", "12", "--synthetic"]);
        let real = prepare(&vault, &["--pairs", "12"]);
        for (row, real) in rows.iter().zip(&real) {
            assert_eq!(row["cid"], real["cid"]);
            for side in ["src_excerpt", "dst_excerpt"] {
                let text = row[side].as_str().unwrap();
                assert!(text.starts_with("Synthetic stand-in"), "{text}");
                assert_ne!(row[side], real[side]);
            }
        }
    }

    #[test]
    fn a_pair_pinned_away_from_any_judge_is_left_out() {
        let vault = indexed();
        let config = vault.root().join(".kbx/kbx.toml");
        let text = fs::read_to_string(&config).unwrap();
        fs::write(&config, format!("{text}\"decisions\" = \"ollama\"\n")).unwrap();
        let decisions = [14, 15, 16].map(|n| Value::from(id(n)));
        for row in prepare(&vault, &["--pairs", "100"]) {
            assert!(!decisions.contains(&row["src"]), "{row}");
            assert!(!decisions.contains(&row["dst"]), "{row}");
        }
    }

    #[test]
    fn out_writes_a_file_and_says_what_it_chose() {
        let vault = indexed();
        let file = vault.root().join("pairs.jsonl");
        let args = ["bakeoff", "--prepare", "--pairs", "5", "--out"];
        let mut args = args.to_vec();
        args.push(file.to_str().unwrap());
        args.extend(["--generators", "common_neighbors,alias_cooccur"]);
        let output = vault.kbx(&args);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(stdout(&output), "");
        assert!(stderr(&output).contains("5 pairs"), "{}", stderr(&output));
        assert_eq!(fs::read_to_string(file).unwrap().lines().count(), 5);
    }
}

/// `--prepare`, then labels by way of the queue, as the owner would do it.
#[test]
fn prepared_pairs_go_through_the_queue_and_come_back_as_labels() {
    let vault = indexed();
    let pairs = vault.root().join("pairs.jsonl");
    vault.ok(&[
        "bakeoff",
        "--prepare",
        "--pairs",
        "4",
        "--generators",
        "common_neighbors,alias_cooccur",
        "--out",
        pairs.to_str().unwrap(),
    ]);
    let enqueued = vault.ok(&["bakeoff", "--enqueue", pairs.to_str().unwrap()]);
    assert!(enqueued.contains("4 pairs"), "{enqueued}");

    let queue: Vec<Value> = serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap();
    assert_eq!(queue.len(), 4);
    let cids: Vec<&str> = queue.iter().map(|c| c["cid"].as_str().unwrap()).collect();
    vault.ok(&["accept", cids[0], "--rel", "see_also"]);
    vault.ok(&["reject", cids[1]]);
    vault.ok(&["reject", cids[2]]);
    vault.ok(&["defer", cids[3]]);

    let labels = vault.root().join("labels.jsonl");
    fs::write(&labels, vault.ok(&["export", "reviews", "--jsonl"])).unwrap();
    let model = fake_ollama(&["qwen3:14b"]);
    let doc = doc(&vault);
    let output = vault.kbx_with(
        &[
            "bakeoff",
            "--labels",
            labels.to_str().unwrap(),
            "--doc",
            doc.to_str().unwrap(),
            "--models",
            "ollama:qwen3:14b",
        ],
        &[("OLLAMA_HOST", &model.url)],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = fs::read_to_string(&doc).unwrap();
    assert!(
        text.contains("3 labelled pairs: 1 accept, 2 reject"),
        "{text}"
    );
    assert_eq!(cell(&text, "| ollama | qwen3:14b", "Answered"), "3/3");
}

#[test]
fn a_filled_in_pairs_file_is_a_labels_file() {
    let vault = indexed();
    let mut rows = prepare(&vault, &["--pairs", "3"]);
    rows[0]["decision"] = json!("accept");
    rows[0]["rel"] = json!("see_also");
    rows[1]["decision"] = json!("reject");
    let text: Vec<String> = rows.iter().map(Value::to_string).collect();
    let labels = vault.root().join("labels.jsonl");
    fs::write(&labels, text.join("\n")).unwrap();

    let model = fake_ollama(&["qwen3:14b"]);
    let doc = doc(&vault);
    let output = vault.kbx_with(
        &[
            "bakeoff",
            "--labels",
            labels.to_str().unwrap(),
            "--doc",
            doc.to_str().unwrap(),
            "--models",
            "ollama:qwen3:14b",
        ],
        &[("OLLAMA_HOST", &model.url)],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("1 pair has no decision yet"));
    let text = fs::read_to_string(&doc).unwrap();
    assert!(
        text.contains("2 labelled pairs: 1 accept, 1 reject"),
        "{text}"
    );
}

mod sections {
    use super::*;

    const SECTION_TEXT: &str = "Careful load shedding keeps the queue short";

    /// Two notes the alias generator pairs, by a term in one section of the
    /// second, well past where its excerpt would otherwise start.
    fn vault(anchors: bool) -> Fixture {
        let vault = Fixture::new();
        let a = id(40);
        let b = id(41);
        vault.write(
            &format!("engineering/{a}-shedding.md"),
            &format!(
                "---\nid: {a}\ntype: Concept\ntitle: Shedding\n\
                 aliases: [load shedding]\n---\n\nDropping work on purpose.\n"
            ),
        );
        vault.write(
            &format!("engineering/{b}-queues.md"),
            &format!(
                "---\nid: {b}\ntype: Concept\ntitle: Queues\n---\n\n\
                 The note opens here.\n\n## Overload\n\n{SECTION_TEXT}.\n"
            ),
        );
        if anchors {
            let config = vault.root().join(".kbx/kbx.toml");
            let text = fs::read_to_string(&config).unwrap();
            fs::write(config, format!("{text}\n[write]\nsection_anchors = true\n")).unwrap();
        }
        vault.ok(&["index"]);
        vault
    }

    /// The `--prepare` row for the two notes above.
    fn row(vault: &Fixture) -> Value {
        let rows = prepare(vault, &["--generators", "alias_cooccur", "--pairs", "100"]);
        let ends = [Value::from(id(40)), Value::from(id(41))];
        let mine = rows
            .into_iter()
            .find(|row| ends.contains(&row["src"]) && ends.contains(&row["dst"]));
        mine.expect("the alias generator pairs the two notes")
    }

    /// Runs the bake-off on `row`, decided, and returns what the judge was shown.
    fn judged(vault: &Fixture, row: &Value) -> Value {
        let mut row = row.clone();
        row["decision"] = json!("accept");
        row["rel"] = json!("see_also");
        let labels = vault.root().join("labels.jsonl");
        fs::write(&labels, row.to_string()).unwrap();
        let model = fake_ollama(&["qwen3:14b"]);
        let doc = doc(vault);
        let output = vault.kbx_with(
            &[
                "bakeoff",
                "--labels",
                labels.to_str().unwrap(),
                "--doc",
                doc.to_str().unwrap(),
                "--models",
                "ollama:qwen3:14b",
            ],
            &[("OLLAMA_HOST", &model.url)],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        let text = fs::read_to_string(&doc).unwrap();
        assert_eq!(cell(&text, "| ollama | qwen3:14b", "Answered"), "1/1");
        let body: Value = serde_json::from_str(&model.bodies("/api/chat")[0]).unwrap();
        let batch = body["messages"][1]["content"].as_str().unwrap();
        let batch: Vec<Value> = serde_json::from_str(batch).unwrap();
        batch[0].clone()
    }

    fn queue_of(vault: &Fixture, row: &Value) -> Vec<Value> {
        let file = vault.root().join("pairs.jsonl");
        fs::write(&file, row.to_string()).unwrap();
        vault.ok(&["bakeoff", "--enqueue", file.to_str().unwrap()]);
        serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap()
    }

    #[test]
    fn with_anchors_on_a_pair_keeps_its_section_all_the_way() {
        let vault = vault(true);
        let row = row(&vault);
        let section = |side: &str| row[side].as_str();
        let (src, dst) = (row["src"].as_str().unwrap(), row["dst"].as_str().unwrap());
        assert!(section("src_section").is_some() || section("dst_section").is_some());
        let expected =
            kbx_core::section_cid(src, dst, section("src_section"), section("dst_section"));
        assert_eq!(row["cid"], expected);
        assert_ne!(row["cid"], kbx_core::cid(src, dst));
        let excerpts = format!("{} {}", row["src_excerpt"], row["dst_excerpt"]);
        assert!(excerpts.contains(SECTION_TEXT), "{excerpts}");

        let shown = judged(&vault, &row);
        assert_eq!(shown["cid"], row["cid"]);
        let shown = format!("{} {}", shown["src"]["excerpt"], shown["dst"]["excerpt"]);
        assert!(shown.contains(SECTION_TEXT), "{shown}");
        assert!(!shown.contains("The note opens here"), "{shown}");

        let queue = queue_of(&vault, &row);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0]["cid"], row["cid"]);
        assert!(queue[0].get("src_anchor").is_some() || queue[0].get("dst_anchor").is_some());
    }

    #[test]
    fn with_anchors_off_pairs_are_note_level_whatever_a_file_says() {
        let vault = vault(false);
        let mut row = row(&vault);
        let object = row.as_object().unwrap();
        assert!(!object.contains_key("src_section"), "{row}");
        assert!(!object.contains_key("dst_section"), "{row}");
        let note_level = row["cid"].clone();
        let (src, dst) = (row["src"].as_str().unwrap(), row["dst"].as_str().unwrap());
        assert_eq!(note_level, kbx_core::cid(src, dst));
        let excerpts = format!("{} {}", row["src_excerpt"], row["dst_excerpt"]);
        assert!(excerpts.contains("The note opens here"), "{excerpts}");

        // A file from a vault that had them on.
        row["src_section"] = json!("some-section");
        let shown = judged(&vault, &row);
        assert_eq!(shown["cid"], note_level);
        let queue = queue_of(&vault, &row);
        assert_eq!(queue[0]["cid"], note_level);
        assert!(queue[0].get("src_anchor").is_none(), "{queue:?}");
    }
}

/// The set the owner labels: the reviews format plus what helps a reader, and
/// nothing that quotes a note.
#[test]
fn the_committed_pairs_are_in_the_reviews_format() {
    let text = fs::read_to_string(PAIRS).unwrap();
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!((90..=110).contains(&rows.len()), "{} rows", rows.len());

    let mut cids = BTreeSet::new();
    let fields = [
        "src",
        "dst",
        "rel",
        "decision",
        "reason",
        "strikes",
        "decided_at",
        "cid",
        "generators",
        "score",
        "src_type",
        "dst_type",
        "src_excerpt",
        "dst_excerpt",
    ];
    for row in &rows {
        let keys: BTreeSet<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, BTreeSet::from(fields), "{row}");
        let (src, dst) = (row["src"].as_str().unwrap(), row["dst"].as_str().unwrap());
        assert_eq!(row["cid"], kbx_core::cid(src, dst));
        assert!(cids.insert(row["cid"].to_string()), "repeated: {row}");
        let decision = row["decision"].as_str();
        assert!(
            matches!(decision, None | Some("accept" | "reject")),
            "{row}"
        );
        for side in ["src_excerpt", "dst_excerpt"] {
            let words = row[side].as_str().unwrap().split_whitespace().count();
            assert!(words <= 20, "{row}");
        }
    }
}

#[cfg(unix)]
mod priced {
    use super::*;

    const LOGGED_IN: &str = r#"{"loggedIn": true, "authMethod": "claude.ai"}"#;
    const PROBE_OK: &str = r#"{"type":"result","is_error":false,"structured_output":{"ok":true},"total_cost_usd":0.0008}"#;

    /// A `claude` that rejects everything, at `cost` a call, and logs each
    /// model it's asked for.
    fn answering_claude(vault: &Fixture, cost: f64) {
        vault.path.tool(
            "claude",
            &format!(
                r#"d="${{0%/*}}"
if [ "$1" = auth ]; then
  echo '{LOGGED_IN}'
  exit 0
fi
case "$2" in
  'reply with'*) echo '{PROBE_OK}'; exit 0 ;;
esac
while [ $# -gt 0 ]; do
  if [ "$1" = --model ]; then echo "$2" >> "$d/claude.models"; fi
  shift
done
items=$(/usr/bin/grep -o '"cid":"[0-9a-f]*"' \
  | /usr/bin/sed 's/^/{{/; s/$/,"rationale":"r","verdict":"reject","rel":"none","confidence":0.5}}/' \
  | /usr/bin/paste -s -d , -)
echo "{{\"is_error\":false,\"structured_output\":{{\"verdicts\":[$items]}},\"total_cost_usd\":{cost},\"usage\":{{}}}}""#
            ),
        );
    }

    fn models(vault: &Fixture) -> Vec<String> {
        let log = fs::read_to_string(vault.path.dir().join("claude.models"));
        log.unwrap_or_default().lines().map(String::from).collect()
    }

    fn claude(vault: &Fixture, doc: &Path, models: &str, extra: &[&str]) -> std::process::Output {
        let mut args = vec!["bakeoff", "--labels", LABELS, "--models", models];
        args.extend(["--doc", doc.to_str().unwrap()]);
        args.extend(extra);
        vault.kbx(&args)
    }

    #[test]
    fn cost_is_what_the_calls_came_to_and_stays_after_a_free_rerun() {
        let vault = indexed();
        answering_claude(&vault, 0.06);
        let doc = doc(&vault);
        let models_asked = "claude-cli,claude-cli:sonnet";
        let output = claude(&vault, &doc, models_asked, &[]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(models(&vault), ["haiku", "sonnet"]);

        let text = fs::read_to_string(&doc).unwrap();
        assert_eq!(cell(&text, "| claude-cli | haiku", "Cost"), "$0.0600");
        assert_eq!(cell(&text, "| claude-cli | sonnet", "Cost"), "$0.0600");
        assert_eq!(
            cell(&text, "| claude-cli | haiku", "Reject"),
            "100.0% (3/3)"
        );
        assert_eq!(cell(&text, "| claude-cli | haiku", "Accept"), "0.0% (0/3)");

        let output = claude(&vault, &doc, models_asked, &[]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(models(&vault).len(), 2);
        assert_eq!(fs::read_to_string(&doc).unwrap(), text);
    }

    /// The first batch is charged for and unusable, so it's retried in halves.
    #[test]
    fn a_failed_call_counts_towards_the_cost_though_no_verdict_keeps_it() {
        let vault = indexed();
        answering_claude(&vault, 0.06);
        let program = vault.path.program("claude");
        let script = fs::read_to_string(&program).unwrap();
        let script = script.replacen(
            "items=$(",
            r#"if [ ! -e "$d/failed" ]; then
  : > "$d/failed"
  echo '{"is_error":false,"total_cost_usd":0.05,"usage":{}}'
  exit 0
fi
items=$("#,
            1,
        );
        common::write_executable(&program, &script);
        let doc = doc(&vault);
        let output = claude(&vault, &doc, "claude-cli", &[]);
        assert!(output.status.success(), "{}", stderr(&output));
        let text = fs::read_to_string(&doc).unwrap();
        assert_eq!(cell(&text, "| claude-cli | haiku", "Answered"), "6/6");
        assert_eq!(cell(&text, "| claude-cli | haiku", "Cost"), "$0.1700");
    }

    #[test]
    fn the_budget_is_one_limit_across_the_models() {
        let vault = indexed();
        answering_claude(&vault, 0.06);
        let doc = doc(&vault);
        let output = claude(
            &vault,
            &doc,
            "claude-cli,claude-cli:sonnet",
            &["--budget-usd", "0.05"],
        );
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(models(&vault), ["haiku"]);
        let text = fs::read_to_string(&doc).unwrap();
        assert_eq!(cell(&text, "| claude-cli | sonnet", "Answered"), "0/6");
    }

    #[test]
    fn claudes_environment_model_stands_when_the_list_names_none() {
        let vault = indexed();
        answering_claude(&vault, 0.01);
        let doc = doc(&vault);
        let args = [
            "bakeoff",
            "--labels",
            LABELS,
            "--models",
            "claude-cli",
            "--doc",
            doc.to_str().unwrap(),
        ];
        let output = vault.kbx_with(&args, &[("KBX_CLAUDE_MODEL", "opus")]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(models(&vault), ["opus"]);
    }
}
