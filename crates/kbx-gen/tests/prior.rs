//! `type_pair_prior`: what it learns from `reviews` and how it moves the order.

use std::path::Path;

use kbx_core::{Graph, IndexOptions, Ontology, Queue, index_vault};
use kbx_gen::{
    Budget, Candidate, GenError, Generator, Proposal, Run, Scope, TypePairPrior, generators, store,
    suggest,
};
use rusqlite::params;
use serde_json::json;
use tempfile::TempDir;

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

// Notes of the fixture vault, by class.
const PROCEDURE: [u32; 2] = [8, 9];
const CONCEPT: [u32; 2] = [12, 13];
const DECISION: [u32; 2] = [14, 15];

fn id(n: u32) -> String {
    format!("01KBX{n:021}")
}

struct Fake(Vec<(u32, u32, f64)>);

impl Generator for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn run(&self, _: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let candidate = |(src, dst, score): &(u32, u32, f64)| Candidate {
            src: id(*src),
            dst: id(*dst),
            score: *score,
            features: json!({}),
        };
        Ok(self.0.iter().map(candidate).collect())
    }
}

struct Setup {
    graph: Graph,
    ontology: Ontology,
    _db: TempDir,
}

impl Setup {
    fn new() -> Setup {
        let db = TempDir::new().unwrap();
        let mut graph = Graph::open(&db.path().join("graph.sqlite")).unwrap();
        let vault = Path::new(VAULT).canonicalize().unwrap();
        index_vault(&mut graph, &vault, ONTOLOGY, &IndexOptions::default()).unwrap();
        Setup {
            graph,
            ontology: Ontology::parse(ONTOLOGY).unwrap(),
            _db: db,
        }
    }

    fn review(&self, src: u32, dst: u32, rel: &str, decision: &str) {
        self.graph
            .connection()
            .execute(
                "INSERT INTO reviews (src, dst, rel, decision, strikes) VALUES (?, ?, ?, ?, ?)",
                params![
                    id(src),
                    id(dst),
                    rel,
                    decision,
                    (decision == "reject") as i64
                ],
            )
            .unwrap();
    }

    fn run(&self, pairs: &[(u32, u32, f64)], prior: bool, budget: Budget) -> Run {
        let mut list: Vec<Box<dyn Generator>> = vec![Box::new(Fake(pairs.to_vec()))];
        if prior {
            list.push(Box::new(TypePairPrior));
        }
        let scope = Scope {
            vault: Path::new(VAULT),
            ontology: &self.ontology,
            note: None,
        };
        suggest(&self.graph, &list, &scope, budget).unwrap()
    }

    fn ranked(&self, pairs: &[(u32, u32, f64)], prior: bool) -> Vec<(String, String)> {
        let run = self.run(pairs, prior, Budget::default());
        let ends = |c: &Proposal| (c.src.clone(), c.dst.clone());
        run.candidates.iter().map(ends).collect()
    }
}

fn proposal(run: &Run, src: u32, dst: u32) -> &Proposal {
    let wanted = |c: &&Proposal| c.src == id(src) && c.dst == id(dst);
    run.candidates.iter().find(wanted).unwrap()
}

/// A Procedure-Concept pair behind a Decision-Decision pair, on score alone.
const TWO_PAIRS: [(u32, u32, f64); 2] = [
    (PROCEDURE[0], CONCEPT[0], 0.5),
    (DECISION[0], DECISION[1], 1.0),
];

#[test]
fn past_accepts_move_a_type_pair_up_the_order() {
    let setup = Setup::new();
    let before = setup.ranked(&TWO_PAIRS, true);
    assert_eq!(before[0].0, id(DECISION[0]), "the higher score leads");

    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    setup.review(PROCEDURE[0], CONCEPT[1], "depends_on", "accept");
    setup.review(PROCEDURE[1], CONCEPT[0], "depends_on", "accept");

    let after = setup.ranked(&TWO_PAIRS, true);
    assert_eq!(after[0].0, id(PROCEDURE[0]), "the accepted types lead now");
    assert_eq!(
        setup.ranked(&TWO_PAIRS, false),
        before,
        "the order moved because of the prior"
    );
}

#[test]
fn past_rejects_move_a_type_pair_down_the_order() {
    let setup = Setup::new();
    setup.review(DECISION[1], 16, "see_also", "reject");
    setup.review(DECISION[0], 16, "see_also", "reject");
    setup.review(DECISION[1], 20, "see_also", "reject");

    let after = setup.ranked(&TWO_PAIRS, true);
    assert_eq!(after[0].0, id(PROCEDURE[0]));
}

#[test]
fn with_no_reviews_the_prior_changes_nothing() {
    let setup = Setup::new();
    let without = setup.run(&TWO_PAIRS, false, Budget::default());
    let with = setup.run(&TWO_PAIRS, true, Budget::default());
    assert_eq!(with.candidates, without.candidates);
}

#[test]
fn a_type_pair_nobody_reviewed_is_left_alone() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    let without = setup.run(&TWO_PAIRS, false, Budget::default());
    let with = setup.run(&TWO_PAIRS, true, Budget::default());
    let unseen = proposal(&with, DECISION[0], DECISION[1]);
    assert_eq!(unseen, proposal(&without, DECISION[0], DECISION[1]));
}

#[test]
fn deferrals_teach_it_nothing() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "defer");
    let without = setup.run(&TWO_PAIRS, false, Budget::default());
    let with = setup.run(&TWO_PAIRS, true, Budget::default());
    assert_eq!(with.candidates, without.candidates);
}

#[test]
fn it_explains_itself_in_features_and_generators() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    setup.review(PROCEDURE[0], CONCEPT[1], "see_also", "reject");

    let run = setup.run(&TWO_PAIRS, true, Budget::default());
    let moved = proposal(&run, PROCEDURE[0], CONCEPT[0]);
    assert_eq!(moved.generators, ["fake", "type_pair_prior"]);
    let evidence = &moved.features["type_pair_prior"];
    assert_eq!(evidence["rel"], "depends_on");
    assert_eq!(evidence["accepts"], 1);
    assert_eq!(evidence["rejects"], 0);
    assert!((evidence["score"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9);

    store(&setup.graph, &run).unwrap();
    let queue = Queue::load(&setup.graph, &setup.ontology).unwrap();
    let item = queue.items().iter().find(|i| i.src == moved.src).unwrap();
    assert!(
        item.why_proposed
            .contains(&"type_pair_prior:0.67".to_string()),
        "{:?}",
        item.why_proposed
    );
}

#[test]
fn it_learns_the_type_pair_whichever_way_the_pair_was_found() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    let run = setup.run(&[(CONCEPT[0], PROCEDURE[0], 0.5)], true, Budget::default());
    assert!(run.candidates[0].score > 0.5 / 1.5);
}

#[test]
fn a_rejection_naming_no_relation_counts_against_every_relation() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    setup.review(PROCEDURE[0], CONCEPT[1], "none", "reject");
    setup.review(PROCEDURE[1], CONCEPT[0], "none", "reject");

    let run = setup.run(&TWO_PAIRS, true, Budget::default());
    let evidence = &proposal(&run, PROCEDURE[0], CONCEPT[0]).features["type_pair_prior"];
    assert_eq!(evidence["rejects"], 2);
    assert!((evidence["score"].as_f64().unwrap() - 2.0 / 5.0).abs() < 1e-9);
}

#[test]
fn rejections_alone_name_no_relation() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "none", "reject");
    let run = setup.run(&TWO_PAIRS, true, Budget::default());
    let evidence = &proposal(&run, PROCEDURE[0], CONCEPT[0]).features["type_pair_prior"];
    assert_eq!(evidence["rel"], serde_json::Value::Null);
    assert!((evidence["score"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn a_relation_the_types_cannot_hold_earns_nothing() {
    let setup = Setup::new();
    // No `depends_on` can join two Decisions, whatever an old review says.
    setup.review(DECISION[1], 16, "depends_on", "accept");
    let without = setup.run(&TWO_PAIRS, false, Budget::default());
    let with = setup.run(&TWO_PAIRS, true, Budget::default());
    assert_eq!(with.candidates, without.candidates);
}

#[test]
fn reviews_of_notes_no_longer_here_are_ignored() {
    let setup = Setup::new();
    setup.review(9000, 9001, "depends_on", "accept");
    let without = setup.run(&TWO_PAIRS, false, Budget::default());
    let with = setup.run(&TWO_PAIRS, true, Budget::default());
    assert_eq!(with.candidates, without.candidates);
}

#[test]
fn the_prior_decides_who_survives_the_budget() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    setup.review(PROCEDURE[0], CONCEPT[1], "depends_on", "accept");
    setup.review(PROCEDURE[1], CONCEPT[0], "depends_on", "accept");
    let budget = Budget {
        per_run: 1,
        ..Budget::default()
    };
    let run = setup.run(&TWO_PAIRS, true, budget);
    assert_eq!(run.candidates[0].src, id(PROCEDURE[0]));
    assert_eq!(run.over_budget, 1);
}

#[test]
fn it_proposes_no_pairs_of_its_own() {
    let setup = Setup::new();
    setup.review(PROCEDURE[1], CONCEPT[1], "depends_on", "accept");
    assert!(setup.ranked(&[], true).is_empty());
}

#[test]
fn it_is_chosen_by_name_and_by_default() {
    let names = |list: Vec<Box<dyn Generator>>| -> Vec<&'static str> {
        list.iter().map(|g| g.name()).collect()
    };
    let chosen = ["type_pair_prior".to_string()];
    assert_eq!(
        names(generators(Some(&chosen), None).unwrap()),
        ["type_pair_prior"]
    );
    assert!(names(generators(None, None).unwrap()).contains(&"type_pair_prior"));
}
