//! Runs each generator over the fixture vault, whose README names the notes
//! that set up each §5 case, and over small vaults written for one case.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;

use kbx_core::{Graph, IndexOptions, Ontology, index_vault};
use kbx_gen::{
    AdamicAdar, AliasCooccur, Candidate, CommonNeighbors, Generator, Hit, QmdKnn, Query, SchemaGap,
    Scope, Search, SearchError, TwoHopBridge,
};
use serde_json::json;
use tempfile::TempDir;

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");

fn id(n: u32) -> String {
    format!("01KBX{n:021}")
}

/// A vault indexed into a throwaway database, so the fixture stays untouched.
struct Indexed {
    graph: Graph,
    ontology: Ontology,
    root: std::path::PathBuf,
    _db: TempDir,
}

impl Indexed {
    fn at(root: &Path) -> Indexed {
        let db = TempDir::new().unwrap();
        let mut graph = Graph::open(&db.path().join("graph.sqlite")).unwrap();
        let root = root.canonicalize().unwrap();
        index_vault(&mut graph, &root, ONTOLOGY, &IndexOptions::default()).unwrap();
        Indexed {
            graph,
            ontology: Ontology::parse(ONTOLOGY).unwrap(),
            root,
            _db: db,
        }
    }

    fn fixture() -> Indexed {
        Indexed::at(Path::new(VAULT))
    }

    fn run(&self, generator: &dyn Generator) -> BTreeMap<(String, String), Candidate> {
        let scope = Scope {
            vault: &self.root,
            ontology: &self.ontology,
            note: None,
        };
        let candidates = generator.run(&self.graph, &scope).unwrap();
        let count = candidates.len();
        let pairs: BTreeMap<_, _> = candidates
            .into_iter()
            .map(|c| ((c.src.clone(), c.dst.clone()), c))
            .collect();
        assert_eq!(pairs.len(), count, "{} repeated a pair", generator.name());
        pairs
    }
}

/// Notes as `(n, class, scope, frontmatter lines, body)`.
fn write_vault(dir: &Path, notes: &[(u32, &str, &str, &str, &str)]) {
    for (n, class, scope, frontmatter, body) in notes {
        let folder = dir.join(scope);
        fs::create_dir_all(&folder).unwrap();
        let text = format!(
            "---\nid: {}\ntype: {class}\ntitle: Note {n}\n{frontmatter}---\n{body}\n",
            id(*n)
        );
        fs::write(folder.join(format!("{}-note.md", id(*n))), text).unwrap();
    }
}

fn link(n: u32) -> String {
    format!("[[{}]]", id(n))
}

/// The pair as the symmetric generators emit it, lowest id first.
fn pair(a: u32, b: u32) -> (String, String) {
    let (a, b) = (id(a.min(b)), id(a.max(b)));
    (a, b)
}

/// Every pair the fixture links in either direction, by any edge.
fn linked_pairs(indexed: &Indexed) -> BTreeSet<(String, String)> {
    let connection = indexed.graph.connection();
    let mut statement = connection.prepare("SELECT src, dst FROM edges").unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap();
    rows.map(Result::unwrap)
        .flat_map(|(a, b)| [(a.clone(), b.clone()), (b, a)])
        .collect()
}

fn assert_no_linked_pairs(indexed: &Indexed, pairs: &BTreeMap<(String, String), Candidate>) {
    let linked = linked_pairs(indexed);
    assert!(!linked.is_empty());
    for pair in pairs.keys() {
        assert!(!linked.contains(pair), "{pair:?} is already linked");
    }
}

#[test]
fn common_neighbors_counts_the_shared_concepts_of_an_unlinked_pair() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&CommonNeighbors);
    let candidate = &pairs[&pair(8, 9)];
    assert_eq!(candidate.score, 2.0);
    assert_eq!(candidate.features["via"], json!([id(6), id(7)]));
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn adamic_adar_scores_rare_neighbours_above_common_ones() {
    let dir = TempDir::new().unwrap();
    let (one, two) = (link(1), link(2));
    let depends_on_rare = format!("depends_on: [\"{one}\"]\n");
    let depends_on_both = format!("depends_on: [\"{one}\", \"{two}\"]\n");
    let depends_on_common = format!("depends_on: [\"{two}\"]\n");
    write_vault(
        dir.path(),
        &[
            (1, "Concept", "a", "", ""),
            (2, "Concept", "a", "", ""),
            (3, "Procedure", "a", &depends_on_rare, ""),
            (4, "Procedure", "a", &depends_on_rare, ""),
            (5, "Procedure", "a", &depends_on_common, ""),
            (6, "Procedure", "a", &depends_on_common, ""),
            (7, "Procedure", "a", &depends_on_common, ""),
            (8, "Procedure", "a", &depends_on_both, ""),
        ],
    );
    let indexed = Indexed::at(dir.path());
    let pairs = indexed.run(&AdamicAdar);

    // `…01` has degree 3, so 1 / ln 3. `…02`, the top node, is a hub.
    let rare = pairs[&pair(3, 4)].score;
    assert!((rare - 1.0 / 3f64.ln()).abs() < 1e-9, "{rare}");
    assert!(rare > pairs[&pair(5, 6)].score);
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn adamic_adar_proposes_the_fixture_pair_and_no_linked_pair() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&AdamicAdar);
    assert!(pairs[&pair(8, 9)].score > 0.0);
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn adamic_adar_damps_the_hub() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&AdamicAdar);
    // `…10` and `…11` share only the platform, the fixture's one hub.
    let through_hub = pairs[&pair(10, 11)].score;
    let degree: f64 = indexed
        .graph
        .connection()
        .query_row(
            "SELECT COUNT(DISTINCT dst) FROM edges WHERE src = ? AND weight IS NOT NULL",
            [id(1)],
            |row| row.get::<_, i64>(0),
        )
        .map(|degree| degree as f64)
        .unwrap();
    let undamped = 1.0 / degree.ln();
    assert!(
        through_hub < undamped / 2.0,
        "{through_hub} is not damped from {undamped}"
    );
}

#[test]
fn two_hop_bridge_weights_the_pair_by_its_relations() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&TwoHopBridge);
    let candidate = &pairs[&pair(8, 9)];
    // Two bridges, each `depends_on` (0.9) from both ends.
    assert!((candidate.score - 2.0 * 0.9 * 0.9).abs() < 1e-9);
    assert_eq!(candidate.features["via"], json!([id(6), id(7)]));
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn two_hop_bridge_goes_through_a_hub_only_by_part_of() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&TwoHopBridge);
    // Both `part_of` the platform.
    assert_eq!(pairs[&pair(12, 13)].features["via"], json!([id(1)]));
    // `see_also` and `depends_on` the platform, which doesn't count.
    assert!(!pairs.contains_key(&pair(10, 11)));
}

#[test]
fn schema_gap_offers_the_notes_in_range_and_in_scope() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&SchemaGap);
    let from_gap: BTreeSet<&String> = pairs
        .keys()
        .filter(|(src, _)| *src == id(10))
        .map(|(_, dst)| dst)
        .collect();
    let want = [id(8), id(9), id(11), id(12), id(13)];
    assert_eq!(from_gap, want.iter().collect());
    let candidate = &pairs[&(id(10), id(8))];
    assert_eq!(candidate.features["relations"], json!(["depends_on"]));
    // Only `…10` has the gap, and it already links the platform.
    assert!(pairs.keys().all(|(src, _)| *src == id(10)));
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn schema_gap_leaves_out_classes_outside_the_range() {
    let dir = TempDir::new().unwrap();
    write_vault(
        dir.path(),
        &[
            (1, "Procedure", "a", "", ""),
            (2, "Concept", "a", "", ""),
            (3, "Decision", "a", "", ""),
            (4, "Concept", "b", "", ""),
        ],
    );
    let pairs = Indexed::at(dir.path()).run(&SchemaGap);
    assert_eq!(
        pairs.keys().cloned().collect::<Vec<_>>(),
        [(id(1), id(2))],
        "a Decision is out of range and `…04` is in another scope"
    );
}

#[test]
fn schema_gap_ignores_a_requirement_met_by_a_hidden_link() {
    let dir = TempDir::new().unwrap();
    write_vault(
        dir.path(),
        &[
            (1, "Procedure", "a", "depends_on: [\"[[ghost]]\"]\n", ""),
            (2, "Concept", "a", "", ""),
        ],
    );
    assert!(Indexed::at(dir.path()).run(&SchemaGap).is_empty());
}

#[test]
fn alias_cooccur_finds_an_alias_in_a_body_without_a_link() {
    let indexed = Indexed::fixture();
    let pairs = indexed.run(&AliasCooccur);
    let candidate = &pairs[&(id(11), id(6))];
    assert_eq!(candidate.score, 1.0);
    assert_eq!(candidate.features["terms"], json!(["token bucket"]));
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn alias_cooccur_matches_whole_words_in_any_case_outside_frontmatter() {
    let dir = TempDir::new().unwrap();
    let linking = format!("See {} on the token bucket.", link(1));
    write_vault(
        dir.path(),
        &[
            (1, "Concept", "a", "aliases: [token bucket]\n", ""),
            (
                2,
                "Concept",
                "a",
                "",
                "Empty TOKEN  BUCKET again, and a token bucket.",
            ),
            (3, "Concept", "a", "", "A token buckets list is not one."),
            (4, "Concept", "a", "tags: [token bucket]\n", "Nothing here."),
            (5, "Concept", "a", "", &linking),
        ],
    );
    let pairs = Indexed::at(dir.path()).run(&AliasCooccur);
    let found: Vec<_> = pairs.keys().cloned().collect();
    assert_eq!(found, [(id(2), id(1))]);
    assert_eq!(pairs[&(id(2), id(1))].score, 2.0);
}

/// Answers each section with `hits`, and remembers what it was asked.
struct FakeSearch {
    hits: Vec<Hit>,
    asked: RefCell<Vec<(String, String)>>,
}

impl Search for FakeSearch {
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError> {
        assert_eq!(limit, 20);
        let Query::Section { note, text, .. } = query else {
            panic!("qmd_knn searches by section");
        };
        self.asked
            .borrow_mut()
            .push((note.to_string(), text.to_string()));
        Ok(self.hits.clone())
    }
}

/// Lets a test keep hold of the search it gives away.
struct Shared(Rc<FakeSearch>);

impl Search for Shared {
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError> {
        self.0.search(query, limit)
    }
}

fn hit(n: u32, score: f64) -> Hit {
    Hit {
        note: id(n),
        section: format!("section-of-{n}"),
        score,
    }
}

#[test]
fn qmd_knn_turns_hits_into_pairs_and_keeps_the_best_score() {
    let indexed = Indexed::fixture();
    let search = FakeSearch {
        hits: vec![hit(13, 0.8), hit(12, 0.5), hit(1, 0.4)],
        asked: RefCell::default(),
    };
    let knn = QmdKnn::new(Box::new(search));
    let pairs = indexed.run(&knn);

    let candidate = &pairs[&pair(12, 13)];
    assert_eq!(candidate.score, 0.8);
    assert_eq!(
        candidate.features["sections"][id(13)],
        json!("section-of-13")
    );
    assert!(
        !pairs.keys().any(|(a, b)| a == b),
        "a note is not its own pair"
    );
    assert_no_linked_pairs(&indexed, &pairs);
}

#[test]
fn qmd_knn_searches_every_section_as_one_line_of_its_text() {
    let indexed = Indexed::fixture();
    let scope = Scope {
        vault: &indexed.root,
        ontology: &indexed.ontology,
        note: Some(&id(12)),
    };
    let search = Rc::new(FakeSearch {
        hits: Vec::new(),
        asked: RefCell::default(),
    });
    let knn = QmdKnn::new(Box::new(Shared(search.clone())));
    knn.run(&indexed.graph, &scope).unwrap();

    let asked = search.asked.borrow();
    assert_eq!(asked.len(), 1, "`--note` limits the searches to that note");
    assert_eq!(asked[0].0, id(12));
    assert!(
        asked[0].1.starts_with("A browser session ends"),
        "{asked:?}"
    );
    assert!(!asked[0].1.contains('\n'));
}

#[test]
fn qmd_knn_passes_a_search_failure_on() {
    struct Broken;
    impl Search for Broken {
        fn search(&self, _: &Query<'_>, _: usize) -> Result<Vec<Hit>, SearchError> {
            Err(SearchError::Failed("no index".into()))
        }
    }
    let indexed = Indexed::fixture();
    let scope = Scope {
        vault: &indexed.root,
        ontology: &indexed.ontology,
        note: None,
    };
    let error = QmdKnn::new(Box::new(Broken))
        .run(&indexed.graph, &scope)
        .unwrap_err();
    assert!(error.to_string().contains("no index"), "{error}");
}
