//! The review queue: what it holds, in what order, and that a decision taken
//! on it is the decision `kbx accept`, `reject` and `defer` make.

use std::fs;
use std::path::PathBuf;

use kbx_core::{
    Decision, Exclusions, Graph, IndexOptions, Ontology, Queue, ReviewError, cid, excerpt,
    index_vault, ordered, reject, suppressed_pairs, unreject,
};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;

const ONTOLOGY: &str = "
version: 1
classes:
  Concept: {description: c}
  Procedure: {description: p}
relations:
  depends_on: {inverse: required_by, domain: [Procedure], range: [Concept, Procedure], weight: 0.9}
  see_also: {inverse: see_also, domain: [any], range: [any], weight: 0.3, symmetric: true}
  part_of: {inverse: has_part, domain: [any], range: [any], weight: 1.0}
  twin_of: {inverse: twin_of, domain: [any], range: [any], weight: 0.5, symmetric: true}
judge:
  vocabulary: [depends_on, see_also, part_of, none]
  direction: from_schema
";

struct Vault {
    dir: TempDir,
    graph: Graph,
    ontology: Ontology,
}

impl Vault {
    /// Notes `a` to `d`: `a` a Procedure, the rest Concepts.
    fn new() -> Vault {
        let dir = TempDir::new().unwrap();
        for (name, class) in [
            ("a", "Procedure"),
            ("b", "Concept"),
            ("c", "Concept"),
            ("d", "Concept"),
        ] {
            let text =
                format!("---\nid: {name}\ntype: {class}\ntitle: Note {name}\n---\nBody {name}.\n");
            fs::write(dir.path().join(format!("{name}.md")), text).unwrap();
        }
        let mut graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
        let root = dir.path().canonicalize().unwrap();
        index_vault(
            &mut graph,
            &root,
            ONTOLOGY,
            &Exclusions::new(&root, &[]).unwrap(),
            &Default::default(),
            &IndexOptions::default(),
        )
        .unwrap();
        Vault {
            dir,
            graph,
            ontology: Ontology::parse(ONTOLOGY).unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn db(&self) -> &Connection {
        self.graph.connection()
    }

    /// Stores the candidate as `kbx suggest` would.
    fn candidate(&self, src: &str, dst: &str, score: f64) -> String {
        let cid = cid(src, dst);
        self.db()
            .execute(
                "INSERT INTO candidates (cid, src, dst, generators, features, score, src_hash, dst_hash)
                 VALUES (?1, ?2, ?3, '[\"common_neighbors\"]',
                     '{\"common_neighbors\":{\"score\":2.0,\"via\":[\"x\"]}}', ?4,
                     (SELECT content_hash FROM files WHERE id = ?2),
                     (SELECT content_hash FROM files WHERE id = ?3))
                 ON CONFLICT(cid) DO NOTHING",
                params![cid, src, dst, score],
            )
            .unwrap();
        cid
    }

    fn verdict(&self, cid: &str, model: &str, verdict: &str, rel: &str, at: i64) {
        self.db()
            .execute(
                "INSERT INTO verdicts (cid, backend, model, prompt_hash, verdict, rel, confidence,
                     rationale, created_at)
                 VALUES (?1, 'test', ?2, 'p', ?3, ?4, 0.5, ?5, ?6)",
                params![
                    cid,
                    model,
                    verdict,
                    rel,
                    format!("{verdict} from {model}"),
                    at
                ],
            )
            .unwrap();
    }

    fn queue(&self) -> Queue {
        Queue::load(&self.graph, &self.ontology).unwrap()
    }

    fn cids(&self) -> Vec<String> {
        self.queue().items().iter().map(|i| i.cid.clone()).collect()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root().join(format!("{name}.md"))).unwrap()
    }

    fn accept(&self, queue: &mut Queue) -> Result<kbx_core::Accepted, ReviewError> {
        queue.accept(&self.graph, &self.root(), &self.ontology, None)
    }

    fn reviews(&self) -> Vec<(String, String, String, String, Option<String>, i64)> {
        let mut statement = self
            .db()
            .prepare("SELECT src, dst, rel, decision, reason, strikes FROM reviews ORDER BY rowid")
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap();
        rows.map(Result::unwrap).collect()
    }
}

#[test]
fn best_score_comes_first_and_unsure_verdicts_come_last_whatever_their_score() {
    let vault = Vault::new();
    let low = vault.candidate("a", "c", 0.2);
    let high = vault.candidate("b", "c", 0.5);
    let unsure_top = vault.candidate("a", "d", 0.99);
    let unsure_low = vault.candidate("a", "b", 0.01);
    vault.verdict(&unsure_top, "m", "unsure", "none", 1);
    vault.verdict(&unsure_low, "m", "unsure", "see_also", 1);

    let mut unranked = vec![unsure_top, unsure_low];
    unranked.sort();
    let mut expected = vec![high, low];
    expected.extend(unranked);
    assert_eq!(vault.cids(), expected);
}

#[test]
fn only_the_latest_verdict_makes_a_candidate_unsure() {
    let vault = Vault::new();
    let settled = vault.candidate("a", "b", 0.1);
    let top = vault.candidate("a", "c", 0.9);
    vault.verdict(&settled, "old", "unsure", "none", 1);
    vault.verdict(&settled, "new", "accept", "depends_on", 2);
    vault.verdict(&top, "old", "accept", "depends_on", 1);
    vault.verdict(&top, "new", "unsure", "none", 2);
    assert_eq!(vault.cids(), [settled, top]);
}

#[test]
fn an_item_says_what_is_proposed_and_why() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.75);
    vault.verdict(&cid, "m", "accept", "depends_on", 1);

    let queue = vault.queue();
    let item = queue.current().unwrap();
    assert_eq!(item.cid, cid);
    assert_eq!((item.src.as_str(), item.dst.as_str()), ("a", "b"));
    assert_eq!(item.src_title.as_deref(), Some("Note a"));
    assert_eq!(item.dst_title.as_deref(), Some("Note b"));
    assert_eq!(item.rel.as_deref(), Some("depends_on"));
    assert_eq!(item.score, 0.75);
    assert_eq!(item.generators, ["common_neighbors"]);
    assert_eq!(item.why_proposed, ["common_neighbors:2.00"]);
    assert_eq!(item.verdict.as_deref(), Some("accept"));
    assert_eq!(item.rationale.as_deref(), Some("accept from m"));
}

#[test]
fn a_candidate_nobody_judged_has_no_relation_and_no_verdict() {
    let vault = Vault::new();
    vault.candidate("a", "b", 0.75);
    let queue = vault.queue();
    let item = queue.current().unwrap();
    assert_eq!(item.rel, None);
    assert_eq!(item.verdict, None);
    assert_eq!(item.rationale, None);
}

#[test]
fn why_proposed_names_each_generators_evidence_in_the_judges_form() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    let features = json!({
        "adamic_adar": {"score": 0.421, "via": ["x"]},
        "alias_cooccur": {"score": 1.0, "src": "a", "terms": ["token refresh"]},
        "schema_gap": {"score": 1.0, "src": "a", "relations": ["depends_on"]},
    });
    vault
        .db()
        .execute(
            "UPDATE candidates SET features = ?, generators = ? WHERE cid = ?",
            params![
                features.to_string(),
                json!(["adamic_adar", "alias_cooccur", "schema_gap"]).to_string(),
                cid
            ],
        )
        .unwrap();
    assert_eq!(
        vault.queue().current().unwrap().why_proposed,
        [
            "adamic_adar:0.42",
            "alias_cooccur:'token refresh'",
            "schema_gap:depends_on"
        ]
    );
}

#[test]
fn the_relation_shown_is_the_one_accept_would_write_even_under_a_later_none() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "old", "accept", "depends_on", 1);
    vault.verdict(&cid, "new", "reject", "none", 2);

    let mut queue = vault.queue();
    let item = queue.current().unwrap();
    assert_eq!(item.rel.as_deref(), Some("depends_on"));
    assert_eq!(item.verdict.as_deref(), Some("reject"));
    assert_eq!(item.rationale.as_deref(), Some("reject from new"));
    assert_eq!(vault.accept(&mut queue).unwrap().rel, "depends_on");
}

#[test]
fn verdicts_of_the_same_second_resolve_to_the_last_one_stored() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "first", "accept", "see_also", 7);
    vault.verdict(&cid, "second", "accept", "depends_on", 7);

    let mut queue = vault.queue();
    let item = queue.current().unwrap();
    assert_eq!(item.rel.as_deref(), Some("depends_on"));
    assert_eq!(item.rationale.as_deref(), Some("accept from second"));
    let decision = Decision::default();
    let accepted = kbx_core::accept(&vault.graph, &vault.root(), &vault.ontology, &cid, decision);
    assert_eq!(accepted.unwrap().rel, "depends_on");
    assert!(vault.accept(&mut queue).is_err(), "it has left the table");
}

/// Rejects the pair's verdict, then stores the pair again as a later
/// `kbx suggest` does, with the cached verdict still in place.
fn rejected_and_proposed_again(vault: &Vault, rel: &str) -> String {
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "m", "accept", rel, 1);
    let mut queue = vault.queue();
    queue
        .reject(&vault.graph, &vault.ontology, Some("no"))
        .unwrap();
    assert_eq!(vault.candidate("b", "a", 0.5), cid);
    cid
}

#[test]
fn a_relation_already_rejected_stays_out_of_the_queue() {
    let vault = Vault::new();
    rejected_and_proposed_again(&vault, "depends_on");
    let queue = vault.queue();
    assert!(queue.items().is_empty());
    assert_eq!(queue.hidden(), 1);
}

#[test]
fn an_item_reads_the_way_accept_would_write_it() {
    let vault = Vault::new();
    vault.candidate("b", "a", 0.5);
    let mut queue = vault.queue();
    assert!(
        !queue.current().unwrap().swapped(&vault.ontology),
        "no relation yet"
    );

    queue.edit(&vault.ontology, "depends_on").unwrap();
    let item = queue.current().unwrap();
    assert_eq!((item.src.as_str(), item.dst.as_str()), ("b", "a"));
    assert!(item.swapped(&vault.ontology));
    let accepted = vault.accept(&mut queue).unwrap();
    assert_eq!((accepted.src.as_str(), accepted.dst.as_str()), ("a", "b"));
}

#[test]
fn an_inverse_named_by_a_verdict_reads_the_relation_the_other_way() {
    let vault = Vault::new();
    let cid = vault.candidate("b", "a", 0.5);
    vault.verdict(&cid, "m", "accept", "required_by", 1);
    let mut queue = vault.queue();
    assert!(queue.current().unwrap().swapped(&vault.ontology));
    let accepted = vault.accept(&mut queue).unwrap();
    assert_eq!((accepted.src.as_str(), accepted.dst.as_str()), ("a", "b"));
}

#[test]
fn a_relation_that_fits_both_ways_or_neither_keeps_the_candidates_order() {
    let vault = Vault::new();
    vault.candidate("b", "a", 0.5);
    vault.candidate("b", "c", 0.4);
    let mut queue = vault.queue();
    queue.edit(&vault.ontology, "see_also").unwrap();
    assert!(!queue.current().unwrap().swapped(&vault.ontology));
    queue.skip();
    queue.edit(&vault.ontology, "depends_on").unwrap();
    assert!(!queue.current().unwrap().swapped(&vault.ontology));
    let error = vault.accept(&mut queue).unwrap_err();
    assert!(matches!(error, ReviewError::OutOfSchema { .. }), "{error}");
}

#[test]
fn a_new_relation_on_a_rejected_pair_comes_back() {
    let vault = Vault::new();
    let cid = rejected_and_proposed_again(&vault, "depends_on");
    vault.verdict(&cid, "newer", "accept", "see_also", 2);
    assert_eq!(vault.cids(), [cid]);
}

#[test]
fn unreject_brings_a_hidden_relation_back_though_the_row_still_says_reject() {
    let vault = Vault::new();
    let cid = rejected_and_proposed_again(&vault, "depends_on");
    unreject(&vault.graph, &cid).unwrap();
    assert_eq!(vault.reviews()[0].3, "reject");
    assert_eq!(vault.cids(), [cid]);
    assert_eq!(vault.queue().hidden(), 0);
}

#[test]
fn a_rejection_that_named_no_relation_hides_nothing_so_strikes_can_add_up() {
    let vault = Vault::new();
    for strike in 1..=3 {
        vault.candidate("a", "b", 0.5);
        let mut queue = vault.queue();
        assert_eq!(queue.items().len(), 1, "before strike {strike}");
        let rejected = queue.reject(&vault.graph, &vault.ontology, None).unwrap();
        assert_eq!(rejected.strikes, strike);
    }
    let suppressed = suppressed_pairs(vault.db()).unwrap();
    assert!(suppressed.contains(&ordered("a", "b")));
}

#[test]
fn a_verdict_the_ontology_no_longer_knows_can_be_edited_away() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "m", "accept", "retired", 1);

    let mut queue = vault.queue();
    assert_eq!(queue.current().unwrap().rel.as_deref(), Some("retired"));
    let error = vault.accept(&mut queue).unwrap_err();
    assert!(matches!(error, ReviewError::UnknownRelation(_)), "{error}");
    assert_eq!(queue.current().unwrap().cid, cid);

    queue.edit(&vault.ontology, "see_also").unwrap();
    assert_eq!(vault.accept(&mut queue).unwrap().rel, "see_also");
}

#[test]
fn a_note_gone_from_the_index_leaves_its_candidate_in_the_queue_without_a_title() {
    let vault = Vault::new();
    vault.candidate("a", "b", 0.5);
    vault
        .db()
        .execute("DELETE FROM files WHERE id = 'b'", [])
        .unwrap();
    let mut queue = vault.queue();
    assert_eq!(queue.current().unwrap().dst_title, None);
    queue.edit(&vault.ontology, "see_also").unwrap();
    let error = vault.accept(&mut queue).unwrap_err();
    assert!(
        matches!(error, ReviewError::TargetUnavailable(_)),
        "{error}"
    );
}

#[test]
fn a_scripted_session_decides_through_the_queue() {
    let vault = Vault::new();
    let accepted = vault.candidate("a", "b", 0.9);
    let rejected = vault.candidate("a", "c", 0.8);
    let deferred = vault.candidate("a", "d", 0.7);
    let skipped = vault.candidate("b", "c", 0.6);
    let last = vault.candidate("b", "d", 0.5);
    vault.verdict(&accepted, "m", "accept", "depends_on", 1);

    let mut queue = vault.queue();
    assert_eq!(vault.accept(&mut queue).unwrap().rel, "depends_on");
    assert_eq!(queue.current().unwrap().cid, rejected);
    let strike = queue.reject(&vault.graph, &vault.ontology, Some("off topic"));
    assert_eq!(strike.unwrap().strikes, 1);
    assert_eq!(queue.current().unwrap().cid, deferred);
    queue.defer(&vault.graph, &vault.ontology, None).unwrap();
    assert_eq!(queue.current().unwrap().cid, skipped);
    queue.skip();
    assert_eq!(queue.current().unwrap().cid, last);
    queue.edit(&vault.ontology, "part_of").unwrap();
    assert_eq!(vault.accept(&mut queue).unwrap().rel, "part_of");
    assert!(queue.current().is_none());

    let strings = |row: [&str; 4], reason: Option<&str>, strikes| {
        let [src, dst, rel, decision] = row.map(String::from);
        (src, dst, rel, decision, reason.map(String::from), strikes)
    };
    assert_eq!(
        vault.reviews(),
        [
            strings(["a", "b", "depends_on", "accept"], None, 0),
            strings(["a", "c", "none", "reject"], Some("off topic"), 1),
            strings(["a", "d", "none", "defer"], None, 0),
            strings(["b", "d", "part_of", "accept"], None, 0),
        ]
    );
    assert!(vault.read("a").contains("depends_on: [\"[[b]]\"]"));
    assert!(vault.read("b").contains("required_by: [\"[[a]]\"]"));
    assert!(vault.read("b").contains("part_of: [\"[[d]]\"]"));
    assert!(vault.read("d").contains("has_part: [\"[[b]]\"]"));

    // What was put off or passed over is still there next time, and in order.
    assert_eq!(vault.cids(), [deferred, skipped]);
}

#[test]
fn a_session_of_three_rejections_suppresses_the_pair_as_the_cli_would() {
    let vault = Vault::new();
    vault.candidate("a", "b", 0.5);
    let cid = cid("a", "b");
    let decision = Decision::default();
    reject(&vault.graph, &vault.ontology, &cid, decision).unwrap();
    vault.candidate("a", "b", 0.5);
    reject(&vault.graph, &vault.ontology, &cid, decision).unwrap();
    vault.candidate("a", "b", 0.5);

    let mut queue = vault.queue();
    let third = queue.reject(&vault.graph, &vault.ontology, None).unwrap();
    assert!(third.suppressed());
}

#[test]
fn an_edit_is_kept_for_the_session_and_nothing_is_recorded_until_a_decision() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "m", "accept", "depends_on", 1);
    let before = vault.read("a");

    let mut queue = vault.queue();
    queue.edit(&vault.ontology, "see_also").unwrap();
    assert_eq!(queue.current().unwrap().rel.as_deref(), Some("see_also"));
    assert_eq!(vault.read("a"), before);
    assert!(vault.reviews().is_empty());
    let again = vault.queue();
    assert_eq!(again.current().unwrap().rel.as_deref(), Some("depends_on"));
}

#[test]
fn a_rejection_after_an_edit_rejects_the_edited_relation() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "m", "accept", "depends_on", 1);
    let mut queue = vault.queue();
    queue.edit(&vault.ontology, "see_also").unwrap();
    let rejected = queue.reject(&vault.graph, &vault.ontology, None).unwrap();
    assert_eq!(rejected.rel, "see_also");
}

#[test]
fn only_the_vocabulary_can_be_picked() {
    let vault = Vault::new();
    vault.candidate("a", "b", 0.5);
    let mut queue = vault.queue();
    assert_eq!(
        Queue::relations(&vault.ontology),
        ["depends_on", "see_also", "part_of"]
    );
    for word in ["none", "twin_of", "nonsense"] {
        let error = queue.edit(&vault.ontology, word).unwrap_err();
        assert!(matches!(error, ReviewError::UnknownRelation(_)), "{word}");
    }
    assert_eq!(queue.current().unwrap().rel, None);
}

#[test]
fn accepting_with_no_relation_is_refused_and_keeps_the_item() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    let mut queue = vault.queue();
    let error = vault.accept(&mut queue).unwrap_err();
    assert!(matches!(error, ReviewError::NoRelation(_)), "{error}");
    assert_eq!(queue.current().unwrap().cid, cid);
}

#[test]
fn the_hash_guard_refuses_an_edited_note_and_leaves_the_item_current() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    vault.verdict(&cid, "m", "accept", "depends_on", 1);
    let mut queue = vault.queue();

    let edited = vault.read("a") + "A late edit.\n";
    fs::write(vault.root().join("a.md"), &edited).unwrap();
    let error = vault.accept(&mut queue).unwrap_err();
    assert!(matches!(error, ReviewError::Changed(_)), "{error}");

    assert_eq!(queue.current().unwrap().cid, cid);
    assert_eq!(vault.read("a"), edited);
    assert!(!vault.read("b").contains("required_by"));
    assert!(vault.reviews().is_empty());
}

#[test]
fn a_candidate_decided_elsewhere_is_refused() {
    let vault = Vault::new();
    let cid = vault.candidate("a", "b", 0.5);
    let mut queue = vault.queue();
    reject(&vault.graph, &vault.ontology, &cid, Decision::default()).unwrap();
    let error = queue
        .defer(&vault.graph, &vault.ontology, None)
        .unwrap_err();
    assert!(matches!(error, ReviewError::UnknownCandidate(_)), "{error}");
    assert_eq!(vault.reviews().len(), 1);
}

#[test]
fn an_empty_queue_refuses_decisions() {
    let vault = Vault::new();
    let mut queue = vault.queue();
    assert!(queue.current().is_none());
    queue.skip();
    let error = vault.accept(&mut queue).unwrap_err();
    assert!(matches!(error, ReviewError::EmptyQueue), "{error}");
    let error = queue.edit(&vault.ontology, "see_also").unwrap_err();
    assert!(matches!(error, ReviewError::EmptyQueue), "{error}");
}

#[test]
fn an_excerpt_is_the_start_of_the_body() {
    let vault = Vault::new();
    let text = format!(
        "\u{feff}---\r\nid: a\r\n---\r\n\r\nBody é{}\r\n",
        "x".repeat(5000)
    );
    fs::write(vault.root().join("a.md"), text).unwrap();
    let excerpt = excerpt(&vault.graph, &vault.root(), "a").unwrap().unwrap();
    assert!(excerpt.starts_with("Body é"), "{excerpt}");
    assert!(!excerpt.contains("id: a"));
    assert_eq!(excerpt.chars().count(), 2400);
}

#[test]
fn a_note_that_cannot_be_read_has_no_excerpt() {
    let vault = Vault::new();
    fs::remove_file(vault.root().join("b.md")).unwrap();
    assert_eq!(excerpt(&vault.graph, &vault.root(), "b").unwrap(), None);
    assert_eq!(
        excerpt(&vault.graph, &vault.root(), "nobody").unwrap(),
        None
    );
}

#[test]
fn an_excerpt_survives_bytes_that_are_not_utf8() {
    let vault = Vault::new();
    let mut bytes = b"---\nid: a\n---\nBody ".to_vec();
    bytes.push(0xff);
    fs::write(vault.root().join("a.md"), bytes).unwrap();
    let excerpt = excerpt(&vault.graph, &vault.root(), "a").unwrap().unwrap();
    assert!(excerpt.starts_with("Body "), "{excerpt}");
}
