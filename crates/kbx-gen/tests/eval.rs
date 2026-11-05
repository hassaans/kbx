//! `kbx suggest --eval`: scoring generators against `reviews`.

use std::collections::HashMap;
use std::path::Path;

use kbx_core::{
    DEFAULT_MENTION_WEIGHT, Exclusions, Graph, IndexOptions, Ontology, Review, cid, index_vault,
};
use kbx_gen::{Candidate, GenError, Generator, Proposal, Scope, TypePairPrior, evaluate, score};
use rusqlite::params;
use serde_json::{Map, json};
use tempfile::TempDir;

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

fn proposal(src: &str, dst: &str, generators: &[&str]) -> Proposal {
    Proposal {
        cid: cid(src, dst),
        src: src.to_string(),
        dst: dst.to_string(),
        generators: generators.iter().map(|name| name.to_string()).collect(),
        features: Map::new(),
        score: 0.5,
        src_section: None,
        dst_section: None,
    }
}

fn review(src: &str, dst: &str, rel: &str, decision: &str) -> Review {
    Review {
        src: src.to_string(),
        dst: dst.to_string(),
        rel: rel.to_string(),
        decision: decision.to_string(),
        reason: None,
        strikes: 0,
        decided_at: Some(0),
    }
}

#[test]
fn precision_is_of_the_reviewed_and_recall_of_the_accepted() {
    let proposals = [
        proposal("a", "b", &["good"]),
        proposal("c", "d", &["good", "poor"]),
        proposal("e", "f", &["good"]),
        proposal("x", "y", &["good", "poor"]),
    ];
    let reviews = [
        review("a", "b", "depends_on", "accept"),
        review("c", "d", "see_also", "reject"),
        review("g", "h", "part_of", "accept"),
        review("i", "j", "part_of", "defer"),
    ];
    let scores = score(
        &["good", "poor", "idle"],
        &proposals,
        &HashMap::new(),
        &reviews,
    );

    let good = &scores[0];
    assert_eq!(good.name, "good");
    assert_eq!((good.proposed, good.reviewed, good.hits), (4, 2, 1));
    assert_eq!(good.precision, Some(0.5));
    assert_eq!(good.recall, Some(0.5));

    let poor = &scores[1];
    assert_eq!((poor.proposed, poor.reviewed, poor.hits), (2, 1, 0));
    assert_eq!(poor.precision, Some(0.0));
    assert_eq!(poor.recall, Some(0.0));

    let idle = &scores[2];
    assert_eq!((idle.proposed, idle.reviewed, idle.hits), (0, 0, 0));
    assert_eq!(idle.precision, None, "nothing reviewed, so nothing to say");
    assert_eq!(idle.recall, Some(0.0));
}

#[test]
fn a_pair_counts_whichever_way_it_was_reviewed() {
    let proposals = [proposal("b", "a", &["g"])];
    let reviews = [review("a", "b", "depends_on", "accept")];
    let scores = score(&["g"], &proposals, &HashMap::new(), &reviews);
    assert_eq!(scores[0].hits, 1);
}

#[test]
fn a_judged_relation_has_to_match_the_accepted_one() {
    let proposals = [proposal("a", "b", &["g"]), proposal("c", "d", &["g"])];
    let reviews = [
        review("a", "b", "depends_on", "accept"),
        review("c", "d", "depends_on", "accept"),
    ];
    let judged = HashMap::from([
        (cid("a", "b"), "depends_on".to_string()),
        (cid("c", "d"), "see_also".to_string()),
    ]);
    let scores = score(&["g"], &proposals, &judged, &reviews);
    assert_eq!((scores[0].reviewed, scores[0].hits), (2, 1));
    assert_eq!(scores[0].recall, Some(0.5));
}

#[test]
fn a_pair_accepted_under_two_relations_is_one_pair_to_find() {
    let proposals = [proposal("a", "b", &["g"])];
    let reviews = [
        review("a", "b", "depends_on", "accept"),
        review("a", "b", "see_also", "accept"),
    ];
    let scores = score(&["g"], &proposals, &HashMap::new(), &reviews);
    assert_eq!(scores[0].recall, Some(1.0));
}

#[test]
fn with_no_reviews_there_is_nothing_to_say() {
    let proposals = [proposal("a", "b", &["g"])];
    let scores = score(&["g"], &proposals, &HashMap::new(), &[]);
    assert_eq!(scores[0].precision, None);
    assert_eq!(scores[0].recall, None);
}

/// Proposes the pair unless the index says it's linked, as generators must.
struct Linkable(String, String);

impl Generator for Linkable {
    fn name(&self) -> &'static str {
        "linkable"
    }

    fn run(&self, g: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let linked: bool = g
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM edges WHERE src = ?1 AND dst = ?2)",
                params![self.0, self.1],
                |row| row.get(0),
            )
            .unwrap();
        Ok(match linked {
            true => Vec::new(),
            false => vec![Candidate {
                src: self.0.clone(),
                dst: self.1.clone(),
                score: 1.0,
                features: json!({}),
            }],
        })
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
        index_vault(
            &mut graph,
            &vault,
            ONTOLOGY,
            &Exclusions::new(&vault, &[]).unwrap(),
            &Default::default(),
            &IndexOptions::default(),
        )
        .unwrap();
        Setup {
            graph,
            ontology: Ontology::parse(ONTOLOGY).unwrap(),
            _db: db,
        }
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            vault: Path::new(VAULT),
            ontology: &self.ontology,
            note: None,
            mention_weight: DEFAULT_MENTION_WEIGHT,
        }
    }

    /// Any edge of the fixture that reads forwards, as an accepted pair would.
    fn an_edge(&self) -> (String, String, String) {
        self.graph
            .connection()
            .query_row(
                "SELECT src, dst, rel FROM edges
                 WHERE weight IS NOT NULL AND inverse_of IS NULL AND origin = 'frontmatter'
                 ORDER BY src, dst LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        let connection = self.graph.connection();
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

#[test]
fn an_accepted_pair_is_found_again_though_the_vault_now_links_it() {
    let setup = Setup::new();
    let (src, dst, rel) = setup.an_edge();
    setup
        .graph
        .connection()
        .execute(
            "INSERT INTO reviews (src, dst, rel, decision, strikes) VALUES (?, ?, ?, 'accept', 0)",
            params![src, dst, rel],
        )
        .unwrap();
    let edges = setup.count("SELECT COUNT(*) FROM edges");

    let generators: Vec<Box<dyn Generator>> = vec![Box::new(Linkable(src, dst))];
    let evaluation = evaluate(&setup.graph, generators, &setup.scope()).unwrap();

    assert_eq!(evaluation.scores[0].hits, 1);
    assert_eq!(evaluation.scores[0].recall, Some(1.0));
    assert_eq!(evaluation.accepted, 1);
    assert_eq!(setup.count("SELECT COUNT(*) FROM edges"), edges);
}

#[test]
fn a_lock_does_not_hide_a_pair_from_the_score() {
    let setup = Setup::new();
    let (src, dst, rel) = setup.an_edge();
    let connection = setup.graph.connection();
    connection
        .execute(
            "INSERT INTO reviews (src, dst, rel, decision, strikes) VALUES (?, ?, ?, 'accept', 0)",
            params![src, dst, rel],
        )
        .unwrap();
    connection
        .execute(
            "INSERT OR REPLACE INTO locks (id, reviewed, baseline, current)
             VALUES (?, '2026-01-01', 'same', 'same')",
            params![dst],
        )
        .unwrap();
    let locks = setup.count("SELECT COUNT(*) FROM locks");

    let generators: Vec<Box<dyn Generator>> = vec![Box::new(Linkable(src, dst))];
    let evaluation = evaluate(&setup.graph, generators, &setup.scope()).unwrap();

    assert_eq!(evaluation.scores[0].hits, 1);
    assert_eq!(setup.count("SELECT COUNT(*) FROM locks"), locks);
}

#[test]
fn a_suppressed_pair_still_counts_as_a_miss() {
    let setup = Setup::new();
    let (src, dst) = (format!("01KBX{:021}", 8), format!("01KBX{:021}", 12));
    setup
        .graph
        .connection()
        .execute(
            "INSERT INTO reviews (src, dst, rel, decision, strikes)
             VALUES (?, ?, 'see_also', 'reject', 3)",
            params![src, dst],
        )
        .unwrap();

    let generators: Vec<Box<dyn Generator>> = vec![Box::new(Linkable(src, dst))];
    let evaluation = evaluate(&setup.graph, generators, &setup.scope()).unwrap();

    assert_eq!(evaluation.scores[0].reviewed, 1);
    assert_eq!(evaluation.scores[0].precision, Some(0.0));
    assert_eq!(evaluation.rejected, 1);
    assert_eq!(
        setup.count("SELECT SUM(strikes) FROM reviews"),
        3,
        "the strikes stand in the real table"
    );
}

#[test]
fn reviews_of_notes_not_on_this_disk_are_not_there_to_find() {
    let setup = Setup::new();
    setup
        .graph
        .connection()
        .execute(
            "INSERT INTO reviews (src, dst, rel, decision, strikes)
             VALUES ('gone-a', 'gone-b', 'see_also', 'accept', 0)",
            [],
        )
        .unwrap();
    let evaluation = evaluate(&setup.graph, Vec::new(), &setup.scope()).unwrap();
    assert_eq!(evaluation.accepted, 0);
}

#[test]
fn the_prior_is_left_out_because_it_finds_no_pairs_and_learned_from_the_answers() {
    let setup = Setup::new();
    let generators: Vec<Box<dyn Generator>> = vec![Box::new(TypePairPrior)];
    let evaluation = evaluate(&setup.graph, generators, &setup.scope()).unwrap();
    assert!(evaluation.scores.is_empty());
}
