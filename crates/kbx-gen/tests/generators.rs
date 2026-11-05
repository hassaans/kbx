//! Runs each generator over the fixture vault, whose README names the notes
//! that set up each §5 case, and over small vaults written for one case.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;

use kbx_core::{DEFAULT_MENTION_WEIGHT, Exclusions, Graph, IndexOptions, Ontology, index_vault};
use kbx_gen::{
    AdamicAdar, AliasCooccur, Budget, Candidate, CommonNeighbors, Generator, Hit, MAX_SEARCHES,
    QmdKnn, Query, SchemaGap, Scope, Search, SearchError, TwoHopBridge,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
/// What the structural generators found in the fixture vault from typed edges,
/// recorded before they read inline links.
const TYPED: &str = include_str!("fixtures/typed-structure.json");
const STRUCTURAL: [&dyn Generator; 3] = [&CommonNeighbors, &AdamicAdar, &TwoHopBridge];

fn id(n: u32) -> String {
    format!("01KBX{n:021}")
}

/// A vault indexed into a throwaway database, so the fixture stays untouched.
struct Indexed {
    graph: Graph,
    ontology: Ontology,
    root: std::path::PathBuf,
    _db: TempDir,
    _notes: Option<TempDir>,
}

impl Indexed {
    fn at(root: &Path) -> Indexed {
        let db = TempDir::new().unwrap();
        let mut graph = Graph::open(&db.path().join("graph.sqlite")).unwrap();
        let root = root.canonicalize().unwrap();
        index_vault(
            &mut graph,
            &root,
            ONTOLOGY,
            &Exclusions::new(&root, &[]).unwrap(),
            &Default::default(),
            &IndexOptions::default(),
        )
        .unwrap();
        Indexed {
            graph,
            ontology: Ontology::parse(ONTOLOGY).unwrap(),
            root,
            _db: db,
            _notes: None,
        }
    }

    /// Indexes the notes written to `dir`, and keeps them.
    fn written(dir: TempDir) -> Indexed {
        let mut indexed = Indexed::at(dir.path());
        indexed._notes = Some(dir);
        indexed
    }

    fn fixture() -> Indexed {
        Indexed::at(Path::new(VAULT))
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            vault: &self.root,
            ontology: &self.ontology,
            note: None,
            mention_weight: DEFAULT_MENTION_WEIGHT,
        }
    }

    fn run(&self, generator: &dyn Generator) -> BTreeMap<(String, String), Candidate> {
        self.run_at(generator, DEFAULT_MENTION_WEIGHT)
    }

    fn run_at(
        &self,
        generator: &dyn Generator,
        mention_weight: f64,
    ) -> BTreeMap<(String, String), Candidate> {
        let scope = Scope {
            mention_weight,
            ..self.scope()
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
    let pairs = indexed.run_at(&CommonNeighbors, 0.0);
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
    assert_eq!(candidate.features["src"], json!(id(10)));
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
    assert_eq!(candidate.features["src"], json!(id(11)));
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

/// Answers each note with `hits`, and remembers what it was asked.
struct FakeSearch {
    hits: Vec<Hit>,
    asked: RefCell<Vec<(String, String)>>,
}

impl Search for FakeSearch {
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError> {
        assert_eq!(limit, 20);
        let Query::Note { note, text } = query else {
            panic!("qmd_knn searches by note");
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
fn qmd_knn_searches_a_note_once_as_one_line_of_its_text() {
    let indexed = Indexed::fixture();
    let scope = Scope {
        vault: &indexed.root,
        ontology: &indexed.ontology,
        note: Some(&id(12)),
        mention_weight: DEFAULT_MENTION_WEIGHT,
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

/// `n` notes of two sections each: the first is a hub the others link to, and
/// the last link nothing, so they are the least linked.
fn sectioned_vault(n: u32) -> Indexed {
    let dir = TempDir::new().unwrap();
    let hub = id(1);
    for number in 1..=n {
        let note = id(number);
        let link = if (2..=n / 2).contains(&number) {
            format!("depends_on: [\"[[{hub}]]\"]\n")
        } else {
            String::new()
        };
        fs::write(
            dir.path().join(format!("{note}.md")),
            format!(
                "---\nid: {note}\ntype: Procedure\ntitle: Note {number}\n{link}---\n\
                 # One\nWords of the first section.\n\n\
                 # Two\nThe second section is the longer of the two.\n"
            ),
        )
        .unwrap();
    }
    Indexed::written(dir)
}

#[test]
fn qmd_knn_starts_one_search_per_note_not_per_section() {
    let indexed = sectioned_vault(6);
    let scope = indexed.scope();
    let search = Rc::new(FakeSearch {
        hits: Vec::new(),
        asked: RefCell::default(),
    });
    QmdKnn::new(Box::new(Shared(search.clone())))
        .run(&indexed.graph, &scope)
        .unwrap();
    let asked = search.asked.borrow();
    assert_eq!(asked.len(), 6);
    for (_, text) in asked.iter() {
        assert!(text.contains("first section"), "{text}");
        assert!(text.contains("second section"), "{text}");
    }
}

#[test]
fn a_long_section_cannot_crowd_the_others_out_of_a_note_query() {
    let dir = TempDir::new().unwrap();
    let note = id(1);
    let long = "padding ".repeat(1000);
    fs::write(
        dir.path().join(format!("{note}.md")),
        format!(
            "---\nid: {note}\ntype: Procedure\ntitle: T\n---\n\
             # One\n{long}\n\n# Two\nThe closing words.\n"
        ),
    )
    .unwrap();
    let indexed = Indexed::written(dir);
    let search = Rc::new(FakeSearch {
        hits: Vec::new(),
        asked: RefCell::default(),
    });
    QmdKnn::new(Box::new(Shared(search.clone())))
        .run(&indexed.graph, &indexed.scope())
        .unwrap();
    let asked = search.asked.borrow();
    assert!(asked[0].1.chars().count() <= 1001, "{}", asked[0].1.len());
    assert!(asked[0].1.contains("closing words"), "{}", asked[0].1);
}

#[test]
fn a_candidate_names_the_longest_section_of_the_note_that_asked() {
    let indexed = sectioned_vault(2);
    let scope = indexed.scope();
    let search = FakeSearch {
        hits: vec![hit(2, 0.8)],
        asked: RefCell::default(),
    };
    let found = QmdKnn::new(Box::new(search))
        .run(&indexed.graph, &scope)
        .unwrap();
    let key: String = indexed
        .graph
        .connection()
        .query_row(
            "SELECT key FROM sections WHERE id = ? AND heading_path = 'two'",
            [id(1)],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found[0].features["sections"][id(1)], json!(key));
}

#[test]
fn qmd_knn_stops_at_its_hard_cap_taking_the_least_linked_notes_first() {
    let indexed = sectioned_vault(MAX_SEARCHES as u32 + 20);
    let search = Rc::new(FakeSearch {
        hits: Vec::new(),
        asked: RefCell::default(),
    });
    QmdKnn::new(Box::new(Shared(search.clone())))
        .run(&indexed.graph, &indexed.scope())
        .unwrap();
    let asked = search.asked.borrow();
    assert_eq!(asked.len(), MAX_SEARCHES);
    assert!(
        !asked.iter().any(|(note, _)| *note == id(1)),
        "the hub is the most linked, so it waits"
    );
    assert_eq!(asked[0].0, id(61), "the first note with no links");
}

#[test]
fn a_run_budget_stops_the_searches_once_it_holds_that_many_pairs() {
    let indexed = sectioned_vault(30);
    let search = Rc::new(FakeSearch {
        hits: vec![hit(1, 0.8)],
        asked: RefCell::default(),
    });
    let budget = Budget {
        per_run: 3,
        ..Budget::default()
    };
    let knn = QmdKnn::new(Box::new(Shared(search.clone())));
    let found = knn
        .run_within(&indexed.graph, &indexed.scope(), budget)
        .unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(search.asked.borrow().len(), 3);
}

#[test]
fn suppressed_pairs_do_not_use_up_a_run_budget() {
    let indexed = sectioned_vault(30);
    // The two least linked notes search first, and their hits are suppressed.
    for n in [16, 17] {
        indexed
            .graph
            .connection()
            .execute(
                "INSERT INTO reviews (src, dst, rel, decision, strikes) \
                 VALUES (?, ?, 'see_also', 'reject', 3)",
                [id(1), id(n)],
            )
            .unwrap();
    }
    let search = Rc::new(FakeSearch {
        hits: vec![hit(1, 0.8)],
        asked: RefCell::default(),
    });
    let budget = Budget {
        per_run: 1,
        ..Budget::default()
    };
    let found = QmdKnn::new(Box::new(Shared(search.clone())))
        .run_within(&indexed.graph, &indexed.scope(), budget)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(search.asked.borrow().len(), 3);
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
        mention_weight: DEFAULT_MENTION_WEIGHT,
    };
    let error = QmdKnn::new(Box::new(Broken))
        .run(&indexed.graph, &scope)
        .unwrap_err();
    assert!(error.to_string().contains("no index"), "{error}");
}

fn rows(pairs: &BTreeMap<(String, String), Candidate>) -> Vec<Value> {
    let row = |c: &Candidate| json!({ "src": c.src, "dst": c.dst, "score": c.score, "via": c.features["via"] });
    pairs.values().map(row).collect()
}

/// Notes 2 and 3 link to note 1 as `frontmatter` says, and 4 and 5 by `body`.
/// Notes 6 to 10 do the same, so that neither 1 nor 6 stands out as a hub.
fn around_one(frontmatter: &str, body: &str) -> Indexed {
    let dir = TempDir::new().unwrap();
    let (twin_frontmatter, twin_body) = (
        frontmatter.replace(&id(1), &id(6)),
        body.replace(&id(1), &id(6)),
    );
    write_vault(
        dir.path(),
        &[
            (1, "Concept", "a", "", ""),
            (2, "Procedure", "a", frontmatter, ""),
            (3, "Procedure", "a", frontmatter, ""),
            (4, "Note", "a", "", body),
            (5, "Note", "a", "", body),
            (6, "Concept", "a", "", ""),
            (7, "Procedure", "a", &twin_frontmatter, ""),
            (8, "Procedure", "a", &twin_frontmatter, ""),
            (9, "Note", "a", "", &twin_body),
            (10, "Note", "a", "", &twin_body),
        ],
    );
    Indexed::written(dir)
}

#[test]
fn a_weight_of_zero_gives_what_typed_edges_alone_gave() {
    let indexed = Indexed::fixture();
    let found: BTreeMap<_, _> = STRUCTURAL
        .iter()
        .map(|generator| (generator.name(), rows(&indexed.run_at(*generator, 0.0))))
        .collect();
    // As text, since parsing the file could round a score.
    let found = serde_json::to_string_pretty(&found).unwrap();
    assert_eq!(found, TYPED.trim_end());
}

#[test]
fn without_mentions_edges_the_weight_changes_nothing() {
    let indexed = Indexed::fixture();
    let removed = indexed
        .graph
        .connection()
        .execute("DELETE FROM edges WHERE rel = 'mentions'", [])
        .unwrap();
    assert!(removed > 0, "the fixture has inline links to remove");
    for generator in STRUCTURAL {
        let off = indexed.run_at(generator, 0.0);
        assert_eq!(indexed.run(generator), off, "{}", generator.name());
    }
}

#[test]
fn inline_links_leave_every_typed_pair_at_least_as_good() {
    let indexed = Indexed::fixture();
    for generator in STRUCTURAL {
        let typed = indexed.run_at(generator, 0.0);
        let pairs = indexed.run(generator);
        for (pair, candidate) in &typed {
            assert!(pairs[pair].score >= candidate.score, "{pair:?}");
        }
        assert_no_linked_pairs(&indexed, &pairs);
    }
}

#[test]
fn two_notes_linking_inline_to_a_third_share_it() {
    let indexed = around_one("", &link(1));
    let weight = DEFAULT_MENTION_WEIGHT;
    let shared = pair(4, 5);

    let found = indexed.run(&CommonNeighbors);
    assert_eq!(found[&shared].score, weight * weight);
    assert_eq!(found[&shared].features["via"], json!([id(1)]));
    let found = indexed.run(&AdamicAdar);
    assert_eq!(found[&shared].score, weight * weight / 2f64.ln());
    let found = indexed.run(&TwoHopBridge);
    assert_eq!(found[&shared].score, weight * weight);
    assert_eq!(found[&shared].features["via"], json!([id(1)]));
}

#[test]
fn notes_a_third_links_to_inline_share_it_but_are_not_bridged() {
    let dir = TempDir::new().unwrap();
    let body = format!("{} {}", link(1), link(2));
    let twin = format!("{} {}", link(4), link(5));
    write_vault(
        dir.path(),
        &[
            (1, "Note", "a", "", ""),
            (2, "Note", "a", "", ""),
            (3, "Note", "a", "", &body),
            (4, "Note", "a", "", ""),
            (5, "Note", "a", "", ""),
            (6, "Note", "a", "", &twin),
        ],
    );
    let indexed = Indexed::written(dir);
    assert!(indexed.run(&CommonNeighbors).contains_key(&pair(1, 2)));
    assert!(indexed.run(&AdamicAdar).contains_key(&pair(1, 2)));
    assert!(!indexed.run(&TwoHopBridge).contains_key(&pair(1, 2)));
}

#[test]
fn a_pair_linked_inline_is_not_proposed() {
    let dir = TempDir::new().unwrap();
    let both = format!("{} {}", link(1), link(3));
    write_vault(
        dir.path(),
        &[
            (1, "Note", "a", "", ""),
            (2, "Note", "a", "", &both),
            (3, "Note", "a", "", &link(1)),
        ],
    );
    let indexed = Indexed::written(dir);
    for generator in STRUCTURAL {
        assert!(indexed.run(generator).is_empty(), "{}", generator.name());
    }
}

#[test]
fn a_shared_typed_link_outranks_a_shared_inline_one() {
    let depends_on = format!("depends_on: [\"{}\"]\n", link(1));
    let indexed = around_one(&depends_on, &link(1));
    for generator in STRUCTURAL {
        let found = indexed.run(generator);
        let (typed, inline) = (found[&pair(2, 3)].score, found[&pair(4, 5)].score);
        assert!(typed > inline, "{}: {typed} {inline}", generator.name());
        assert!(found[&pair(2, 4)].score > inline, "{}", generator.name());
        assert!(typed > found[&pair(2, 4)].score, "{}", generator.name());
    }
}

#[test]
fn the_weakest_example_relation_outranks_an_inline_link_in_a_bridge() {
    let see_also = format!("see_also: [\"{}\"]\n", link(1));
    let found = around_one(&see_also, &link(1)).run(&TwoHopBridge);
    assert!(found[&pair(2, 3)].score > found[&pair(4, 5)].score);
}

#[test]
fn inline_links_to_a_note_leave_the_score_of_a_typed_pair_alone() {
    let depends_on = format!("depends_on: [\"{}\"]\n", link(1));
    let indexed = around_one(&depends_on, &link(1));
    for generator in STRUCTURAL {
        let off = indexed.run_at(generator, 0.0);
        let on = indexed.run(generator);
        assert_eq!(on[&pair(2, 3)], off[&pair(2, 3)], "{}", generator.name());
    }
}

#[test]
fn a_hub_joins_nothing_by_inline_links_and_is_not_joined_by_them() {
    let dir = TempDir::new().unwrap();
    let (hub, two) = (link(1), link(2));
    let mut notes = vec![(1, "Note", "a", "", ""), (8, "Note", "a", "", two.as_str())];
    notes.extend((2..=7).map(|n| (n, "Note", "a", "", hub.as_str())));
    write_vault(dir.path(), &notes);
    let indexed = Indexed::written(dir);
    for generator in STRUCTURAL {
        assert!(indexed.run(generator).is_empty(), "{}", generator.name());
    }
}

#[test]
fn a_hub_still_joins_notes_by_typed_edges() {
    let dir = TempDir::new().unwrap();
    let typed = format!("see_also: [\"{}\"]\n", link(1));
    let mut notes = vec![(1, "Note", "a", "", "")];
    notes.extend((2..=4).map(|n| (n, "Note", "a", typed.as_str(), "")));
    notes.extend((5..=7).map(|n| (n, "Note", "a", "", "")));
    write_vault(dir.path(), &notes);
    let indexed = Indexed::written(dir);
    assert_eq!(indexed.run(&CommonNeighbors)[&pair(2, 3)].score, 0.1);
}

/// Four notes tie for the top, so p99 is their degree and only the cap applies.
fn four_lists_of(linkers: u32) -> BTreeMap<(String, String), Candidate> {
    let dir = TempDir::new().unwrap();
    let lists = (1..=4).map(link).collect::<Vec<_>>().join(" ");
    let mut notes: Vec<_> = (1..=4).map(|n| (n, "Note", "a", "", "")).collect();
    notes.extend((5..5 + linkers).map(|n| (n, "Note", "a", "", lists.as_str())));
    write_vault(dir.path(), &notes);
    Indexed::written(dir).run(&CommonNeighbors)
}

#[test]
fn a_note_with_too_many_inline_links_is_a_list() {
    assert!(four_lists_of(200).contains_key(&pair(5, 6)));
    assert!(four_lists_of(201).is_empty());
}

#[test]
fn a_relation_accepted_in_kbx_counts_like_one_in_the_frontmatter() {
    let depends_on = format!("depends_on: [\"{}\"]\n", link(1));
    let stated = around_one(&depends_on, &link(1));

    let mut accepted = around_one("", &link(1));
    for (src, dst) in [(2, 1), (3, 1), (7, 6), (8, 6)] {
        accepted
            .graph
            .connection()
            .execute(
                "INSERT INTO accepted (src, dst, rel) VALUES (?, ?, 'depends_on')",
                [id(src), id(dst)],
            )
            .unwrap();
    }
    index_vault(
        &mut accepted.graph,
        &accepted.root,
        ONTOLOGY,
        &Exclusions::new(&accepted.root, &[]).unwrap(),
        &Default::default(),
        &IndexOptions {
            full: true,
            ..IndexOptions::default()
        },
    )
    .unwrap();
    let from_kbx: i64 = accepted
        .graph
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE origin = 'kbx' AND weight IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(from_kbx, 4);

    for generator in STRUCTURAL {
        let found = accepted.run(generator);
        assert!(found.contains_key(&pair(2, 3)), "{}", generator.name());
        assert_eq!(found, stated.run(generator), "{}", generator.name());
    }
}

#[test]
fn inline_links_do_not_change_which_notes_qmd_knn_searches_first() {
    let dir = TempDir::new().unwrap();
    let both = format!("Words. {} {}", link(2), link(3));
    write_vault(
        dir.path(),
        &[
            (1, "Note", "a", "", &both),
            (2, "Note", "a", "", "Words."),
            (3, "Note", "a", "", "Words."),
        ],
    );
    let indexed = Indexed::written(dir);
    let search = Rc::new(FakeSearch {
        hits: Vec::new(),
        asked: RefCell::default(),
    });
    let knn = QmdKnn::new(Box::new(Shared(search.clone())));
    knn.run(&indexed.graph, &indexed.scope()).unwrap();
    let asked: Vec<_> = search.asked.borrow().iter().map(|q| q.0.clone()).collect();
    assert_eq!(asked, [id(1), id(2), id(3)]);
}
