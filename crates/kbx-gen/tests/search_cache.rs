//! [`CachedSearch`] around a backend that counts its searches.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kbx_core::{Graph, IndexOptions, index_vault};
use kbx_gen::{Hit, Query, Search, SearchCache, SearchError, vault_state};
use tempfile::TempDir;

const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
const ID_A: &str = "01KBX000000000000000000001";
const ID_B: &str = "01KBX000000000000000000002";
const MODELS: &str = "embeddinggemma";

/// Answers with `hits`, or fails if there are none, and counts the calls.
struct Counting {
    calls: Rc<Cell<usize>>,
    hits: Vec<Hit>,
}

impl Search for Counting {
    fn search(&self, _: &Query<'_>, _: usize) -> Result<Vec<Hit>, SearchError> {
        self.calls.set(self.calls.get() + 1);
        if self.hits.is_empty() {
            return Err(SearchError::Failed("no".to_string()));
        }
        Ok(self.hits.clone())
    }
}

struct Vault {
    dir: TempDir,
    root: PathBuf,
    graph: Graph,
}

fn write_note(root: &Path, folder: &str, id: &str, body: &str) {
    fs::create_dir_all(root.join(folder)).unwrap();
    let text = format!("---\nid: {id}\ntype: Concept\ntitle: {id}\n---\n{body}\n");
    fs::write(root.join(folder).join(format!("{id}.md")), text).unwrap();
}

impl Vault {
    fn new() -> Vault {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("vault");
        write_note(&root, "a", ID_A, "Token buckets.");
        write_note(&root, "b", ID_B, "Rate limits.");
        let root = root.canonicalize().unwrap();
        let graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
        let mut vault = Vault { dir, root, graph };
        vault.index();
        vault
    }

    fn index(&mut self) {
        index_vault(
            &mut self.graph,
            &self.root,
            ONTOLOGY,
            &IndexOptions::default(),
        )
        .unwrap();
    }

    fn cache_file(&self) -> PathBuf {
        self.dir.path().join("search-cache.sqlite")
    }

    fn section_of(&self, id: &str) -> String {
        self.graph
            .connection()
            .query_row("SELECT key FROM sections WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn hit_on_b(&self) -> Hit {
        Hit {
            note: ID_B.to_string(),
            section: self.section_of(ID_B),
            score: 0.5,
        }
    }

    /// One search for note A through a fresh cache, returning its hits and
    /// how many searches reached the backend.
    fn search(&self, hits: Vec<Hit>, roots: &[(&str, &str)]) -> (Vec<Hit>, usize) {
        let calls = Rc::new(Cell::new(0));
        let inner = Counting {
            calls: calls.clone(),
            hits,
        };
        let roots: BTreeMap<_, _> = roots
            .iter()
            .map(|(name, root)| (name.to_string(), root.to_string()))
            .collect();
        let cache = SearchCache::open(&self.cache_file(), MODELS).unwrap();
        let search = cache
            .around(Box::new(inner), &self.graph, "kbx-test", &roots)
            .unwrap();
        let query = Query::Note {
            note: ID_A,
            text: "Token buckets.",
        };
        let found = search.search(&query, 20).unwrap_or_default();
        (found, calls.get())
    }
}

const BOTH: &[(&str, &str)] = &[("a", "a"), ("b", "b")];

#[test]
fn the_same_search_is_answered_from_the_cache_with_the_same_hits() {
    let vault = Vault::new();
    let hit = vault.hit_on_b();
    let (first, calls) = vault.search(vec![hit.clone()], BOTH);
    assert_eq!((first, calls), (vec![hit.clone()], 1));
    let (second, calls) = vault.search(vec![hit.clone()], BOTH);
    assert_eq!((second, calls), (vec![hit], 0));
}

#[test]
fn a_failed_search_is_not_remembered() {
    let vault = Vault::new();
    assert_eq!(vault.search(Vec::new(), BOTH).1, 1);
    assert_eq!(vault.search(Vec::new(), BOTH).1, 1);
}

#[test]
fn free_text_goes_straight_to_the_backend() {
    let vault = Vault::new();
    let calls = Rc::new(Cell::new(0));
    let inner = Counting {
        calls: calls.clone(),
        hits: vec![vault.hit_on_b()],
    };
    let cache = SearchCache::open(&vault.cache_file(), MODELS).unwrap();
    let roots = BTreeMap::from([("a".to_string(), "a".to_string())]);
    let search = cache
        .around(Box::new(inner), &vault.graph, "kbx-test", &roots)
        .unwrap();
    for _ in 0..2 {
        search.search(&Query::Text("buckets"), 20).unwrap();
    }
    assert_eq!(calls.get(), 2);
}

#[test]
fn a_changed_section_at_a_hit_makes_the_entry_stale() {
    let mut vault = Vault::new();
    let hit = vault.hit_on_b();
    vault.search(vec![hit.clone()], BOTH);

    write_note(&vault.root, "b", ID_B, "Rate limits, rewritten.");
    vault.index();
    assert_eq!(vault.search(vec![vault.hit_on_b()], BOTH).1, 1);
}

#[test]
fn a_deleted_note_at_a_hit_makes_the_entry_stale() {
    let mut vault = Vault::new();
    let hit = vault.hit_on_b();
    vault.search(vec![hit.clone()], BOTH);

    fs::remove_file(vault.root.join("b").join(format!("{ID_B}.md"))).unwrap();
    vault.index();
    assert_eq!(vault.search(vec![hit], BOTH).1, 1);
}

#[test]
fn another_set_of_collections_is_another_search() {
    let vault = Vault::new();
    let hit = vault.hit_on_b();
    vault.search(vec![hit.clone()], BOTH);
    assert_eq!(vault.search(vec![hit], &[("a", "a")]).1, 1);
}

#[test]
fn a_scope_that_left_the_vault_takes_its_rows_with_it() {
    let vault = Vault::new();
    let hit = vault.hit_on_b();
    vault.search(vec![hit.clone()], BOTH);
    let rows = |vault: &Vault| -> i64 {
        rusqlite::Connection::open(vault.cache_file())
            .unwrap()
            .query_row("SELECT count(*) FROM results", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(rows(&vault), 1);

    vault.search(vec![hit], &[("b", "b")]);
    assert_eq!(rows(&vault), 0);
}

#[test]
fn a_note_in_no_collection_is_searched_every_time() {
    let vault = Vault::new();
    let hit = vault.hit_on_b();
    assert_eq!(vault.search(vec![hit.clone()], &[("b", "b")]).1, 1);
    assert_eq!(vault.search(vec![hit], &[("b", "b")]).1, 1);
}

#[test]
fn a_collection_at_the_vault_root_holds_every_note() {
    let vault = Vault::new();
    let hit = vault.hit_on_b();
    vault.search(vec![hit.clone()], &[("all", ".")]);
    assert_eq!(vault.search(vec![hit], &[("all", ".")]).1, 0);
}

#[test]
fn a_read_only_cache_answers_but_never_writes() {
    let vault = Vault::new();
    assert!(
        SearchCache::open_read_only(&vault.cache_file(), MODELS).is_none(),
        "no file, no cache"
    );
    let hit = vault.hit_on_b();
    vault.search(vec![hit.clone()], BOTH);

    let cache = SearchCache::open_read_only(&vault.cache_file(), MODELS).unwrap();
    let calls = Rc::new(Cell::new(0));
    let inner = Counting {
        calls: calls.clone(),
        hits: vec![hit],
    };
    let roots = BOTH
        .iter()
        .map(|(name, root)| (name.to_string(), root.to_string()))
        .collect();
    let search = cache
        .around(Box::new(inner), &vault.graph, "kbx-test", &roots)
        .unwrap();
    let query = Query::Note {
        note: ID_A,
        text: "Other words.",
    };
    search.search(&query, 20).unwrap();
    search.search(&query, 20).unwrap();
    assert_eq!(calls.get(), 2, "nothing new was remembered");
}

#[test]
fn the_refresh_stamp_follows_the_vault_and_what_qmd_reports() {
    let mut vault = Vault::new();
    let cache = SearchCache::open(&vault.cache_file(), MODELS).unwrap();
    let state = vault_state(&vault.graph, "Vectors: 2").unwrap();
    assert!(!cache.is_refreshed(&state));
    cache.mark_refreshed(&state).unwrap();
    assert!(cache.is_refreshed(&state));

    assert_ne!(state, vault_state(&vault.graph, "Vectors: 0").unwrap());
    write_note(&vault.root, "a", ID_A, "Changed.");
    vault.index();
    assert_ne!(state, vault_state(&vault.graph, "Vectors: 2").unwrap());
}
