//! The union, filters and budget of a run, and what it stores.

use std::path::Path;

use kbx_core::{Graph, IndexOptions, Ontology, index_vault};
use kbx_gen::{
    Budget, Candidate, GenError, Generator, Hit, Query, Scope, Search, SearchError, cid,
    estimate_tokens, generators, store, suggest,
};
use rusqlite::params;
use serde_json::{Value, json};
use tempfile::TempDir;

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

fn id(n: u32) -> String {
    format!("01KBX{n:021}")
}

struct Fake {
    name: &'static str,
    pairs: Vec<(u32, u32, f64)>,
}

impl Generator for Fake {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&self, _: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let candidate = |(src, dst, score): &(u32, u32, f64)| Candidate {
            src: id(*src),
            dst: id(*dst),
            score: *score,
            features: json!({}),
        };
        Ok(self.pairs.iter().map(candidate).collect())
    }
}

struct Failing;

impl Generator for Failing {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn run(&self, _: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        Err(SearchError::Failed("qmd is gone".into()).into())
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

    fn scope<'a>(&'a self, note: Option<&'a str>) -> Scope<'a> {
        Scope {
            vault: Path::new(VAULT),
            ontology: &self.ontology,
            note,
        }
    }

    fn reject(&self, src: u32, dst: u32) {
        self.graph
            .connection()
            .execute(
                "INSERT INTO reviews (src, dst, rel, decision) VALUES (?, ?, 'see_also', 'reject')",
                params![id(src), id(dst)],
            )
            .unwrap();
    }
}

fn boxed(generators: Vec<Fake>) -> Vec<Box<dyn Generator>> {
    generators
        .into_iter()
        .map(|g| Box::new(g) as Box<dyn Generator>)
        .collect()
}

#[test]
fn cid_depends_only_on_the_pair() {
    assert_eq!(cid("a", "b"), cid("a", "b"));
    assert_eq!(
        cid("a", "b"),
        cid("b", "a"),
        "one pair, whichever way round"
    );
    assert_ne!(cid("a", "b"), cid("a", "c"));
    assert_ne!(cid("ab", "c"), cid("a", "bc"));
    // Pinned: changing it orphans every stored verdict.
    assert_eq!(
        cid("01KBX000000000000000000008", "01KBX000000000000000000009"),
        "f84a4c2a2f5aa9819ce9db0f9a027c2f"
    );
}

#[test]
fn union_merges_a_pair_found_both_ways_and_keeps_the_first_direction() {
    let setup = Setup::new();
    let generators = boxed(vec![
        Fake {
            name: "directed",
            pairs: vec![(9, 8, 3.0)],
        },
        Fake {
            name: "symmetric",
            pairs: vec![(8, 9, 1.0)],
        },
    ]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();

    assert_eq!(run.candidates.len(), 1);
    let candidate = &run.candidates[0];
    assert_eq!((&*candidate.src, &*candidate.dst), (&*id(9), &*id(8)));
    assert_eq!(candidate.generators, ["directed", "symmetric"]);
    assert_eq!(candidate.features["directed"]["score"], json!(3.0));
    assert_eq!(candidate.features["symmetric"]["score"], json!(1.0));
    // Two signals beat either alone: 1 - (1 - 3/4)(1 - 1/2).
    assert!((candidate.score - 0.875).abs() < 1e-9);
}

/// Proposes `…08` to `…09` and back, each with its own evidence.
struct Reciprocal {
    order: [(u32, u32); 2],
}

impl Generator for Reciprocal {
    fn name(&self) -> &'static str {
        "reciprocal"
    }

    fn run(&self, _: &Graph, _: &Scope<'_>) -> Result<Vec<Candidate>, GenError> {
        let candidate = |(src, dst): &(u32, u32)| Candidate {
            src: id(*src),
            dst: id(*dst),
            score: f64::from(*src),
            features: json!({ "src": id(*src) }),
        };
        Ok(self.order.iter().map(candidate).collect())
    }
}

#[test]
fn reciprocal_evidence_is_kept_and_stays_with_its_direction() {
    let setup = Setup::new();
    for order in [[(8, 9), (9, 8)], [(9, 8), (8, 9)]] {
        let first = order[0].0;
        let generators: Vec<Box<dyn Generator>> = vec![Box::new(Reciprocal { order })];
        let run = suggest(
            &setup.graph,
            &generators,
            &setup.scope(None),
            Budget::default(),
        )
        .unwrap();
        assert_eq!(run.candidates.len(), 1);
        let candidate = &run.candidates[0];
        assert_eq!(candidate.src, id(first));
        let evidence = &candidate.features["reciprocal"];
        assert_eq!(evidence["src"], json!(candidate.src));
        assert_eq!(evidence["reverse"]["src"], json!(candidate.dst));
    }
}

#[test]
fn a_direction_set_by_another_generator_leaves_evidence_saying_its_own() {
    let setup = Setup::new();
    let first = Fake {
        name: "first",
        pairs: vec![(8, 9, 1.0)],
    };
    let second = Reciprocal {
        order: [(9, 8), (9, 8)],
    };
    let generators: Vec<Box<dyn Generator>> = vec![Box::new(first), Box::new(second)];
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(run.candidates[0].src, id(8));
    assert_eq!(
        run.candidates[0].features["reciprocal"]["src"],
        json!(id(9))
    );
}

#[test]
fn linked_and_rejected_pairs_are_dropped_from_either_direction() {
    let setup = Setup::new();
    setup.reject(13, 12);
    let generators = boxed(vec![Fake {
        name: "fake",
        // `…08` depends on `…06`, seen here from the far end.
        pairs: vec![(6, 8, 1.0), (12, 13, 1.0), (8, 9, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();

    let found: Vec<_> = run.candidates.iter().map(|c| c.src.clone()).collect();
    assert_eq!(found, [id(8)]);
    assert_eq!(run.dropped, 2);
}

#[test]
fn a_deferred_pair_is_kept() {
    let setup = Setup::new();
    setup
        .graph
        .connection()
        .execute(
            "INSERT INTO reviews (src, dst, rel, decision) VALUES (?, ?, 'see_also', 'defer')",
            params![id(8), id(9)],
        )
        .unwrap();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(run.candidates.len(), 1);
}

#[test]
fn note_keeps_only_pairs_touching_it() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0), (10, 8, 1.0), (12, 13, 1.0)],
    }]);
    let note = id(8);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(Some(&note)),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(run.candidates.len(), 2);
    assert!(
        run.candidates
            .iter()
            .all(|c| c.src == note || c.dst == note)
    );
}

#[test]
fn budget_keeps_the_best_and_counts_both_ends_of_a_pair() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 4.0), (9, 10, 3.0), (10, 11, 2.0), (12, 13, 1.0)],
    }]);
    let budget = Budget {
        per_note: 1,
        per_run: 10,
    };
    let run = suggest(&setup.graph, &generators, &setup.scope(None), budget).unwrap();
    let found: Vec<_> = run.candidates.iter().map(|c| c.src.clone()).collect();
    assert_eq!(found, [id(8), id(10), id(12)], "…09 already spent its one");
    assert_eq!(run.over_budget, 1);

    let budget = Budget {
        per_note: 40,
        per_run: 2,
    };
    let run = suggest(&setup.graph, &generators, &setup.scope(None), budget).unwrap();
    let found: Vec<_> = run.candidates.iter().map(|c| c.src.clone()).collect();
    assert_eq!(found, [id(8), id(9)]);
    assert_eq!(run.over_budget, 2);
}

#[test]
fn a_failing_generator_is_reported_and_the_rest_still_run() {
    let setup = Setup::new();
    let fake = Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0)],
    };
    let generators: Vec<Box<dyn Generator>> = vec![Box::new(Failing), Box::new(fake)];
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    assert_eq!(run.candidates.len(), 1);
    assert_eq!(run.skipped.len(), 1);
    assert_eq!(run.skipped[0].0, "failing");
    assert!(run.skipped[0].1.to_string().contains("qmd is gone"));
}

fn rows(graph: &Graph) -> Vec<(String, String, String, Value, Value, i64)> {
    let connection = graph.connection();
    let mut statement = connection
        .prepare(
            "SELECT cid, src, dst, generators, features, created_at FROM candidates ORDER BY cid",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            let generators: String = row.get(3)?;
            let features: String = row.get(4)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                serde_json::from_str(&generators).unwrap(),
                serde_json::from_str(&features).unwrap(),
                row.get(5)?,
            ))
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn store_writes_provenance_and_a_rerun_keeps_cid_and_created_at() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    store(&setup.graph, &run).unwrap();

    let first = rows(&setup.graph);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, cid(&id(8), &id(9)));
    assert_eq!(first[0].3, json!(["fake"]));
    assert_eq!(first[0].4["fake"]["score"], json!(1.0));

    setup
        .graph
        .connection()
        .execute("UPDATE candidates SET created_at = 7", [])
        .unwrap();
    store(&setup.graph, &run).unwrap();
    let second = rows(&setup.graph);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].0, first[0].0);
    assert_eq!(second[0].5, 7, "a pair keeps the time it was first found");
}

#[test]
fn store_prunes_stored_pairs_that_are_now_rejected() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0), (12, 13, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    store(&setup.graph, &run).unwrap();

    setup.reject(9, 8);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    store(&setup.graph, &run).unwrap();
    let left: Vec<_> = rows(&setup.graph).into_iter().map(|row| row.1).collect();
    assert_eq!(left, [id(12)]);
}

#[test]
fn store_refuses_a_pair_rejected_since_the_run() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0), (12, 13, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    setup.reject(9, 8);
    store(&setup.graph, &run).unwrap();
    let left: Vec<_> = rows(&setup.graph).into_iter().map(|row| row.1).collect();
    assert_eq!(left, [id(12)]);
}

#[test]
fn store_refuses_a_pair_whose_note_has_gone_since_the_run() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0), (12, 13, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    setup
        .graph
        .connection()
        .execute("DELETE FROM files WHERE id = ?", [id(9)])
        .unwrap();
    store(&setup.graph, &run).unwrap();
    let left: Vec<_> = rows(&setup.graph).into_iter().map(|row| row.1).collect();
    assert_eq!(left, [id(12)]);
}

#[test]
fn tokens_are_estimated_from_excerpts_capped_at_600_each() {
    let setup = Setup::new();
    let generators = boxed(vec![Fake {
        name: "fake",
        pairs: vec![(8, 9, 1.0)],
    }]);
    let run = suggest(
        &setup.graph,
        &generators,
        &setup.scope(None),
        Budget::default(),
    )
    .unwrap();
    let tokens = estimate_tokens(&setup.graph, &run.candidates).unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(tokens[0] > 0 && tokens[0] < 2 * 600 + 100, "{}", tokens[0]);
}

#[test]
fn generators_selects_by_name_and_refuses_unknown_names() {
    let names = |list: Vec<Box<dyn Generator>>| -> Vec<&'static str> {
        list.iter().map(|g| g.name()).collect()
    };
    struct NoHits;
    impl Search for NoHits {
        fn search(&self, _: &Query<'_>, _: usize) -> Result<Vec<Hit>, SearchError> {
            Ok(Vec::new())
        }
    }
    let every = names(generators(None, Some(Box::new(NoHits))).unwrap());
    assert_eq!(
        every,
        [
            "schema_gap",
            "alias_cooccur",
            "common_neighbors",
            "adamic_adar",
            "two_hop_bridge",
            "qmd_knn"
        ]
    );
    assert!(!names(generators(None, None).unwrap()).contains(&"qmd_knn"));
    let some = vec!["adamic_adar".to_string(), "schema_gap".to_string()];
    assert_eq!(
        names(generators(Some(&some), None).unwrap()),
        ["schema_gap", "adamic_adar"],
        "directed generators stay first, whatever the order asked for"
    );
    let error = generators(Some(&["nope".to_string()]), None).err().unwrap();
    assert!(error.to_string().contains("nope"), "{error}");
}
