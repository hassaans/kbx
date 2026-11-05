//! [`SearchIndex`] with an embedder that counts what it's asked for, so that
//! keying by `text_hash` can be checked. Nothing here needs a model.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kbx_core::{Exclusions, Graph, IndexOptions, index_vault};
use kbx_gen::{EmbedError, Embedder, Query, Search, SearchIndex, SyncError};
use rusqlite::Connection;
use tempfile::TempDir;

const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
const ID_A: &str = "01KBX000000000000000000001";
const ID_B: &str = "01KBX000000000000000000002";
const ID_C: &str = "01KBX000000000000000000003";

/// Vectors from the words `axis` knows, each its own dimension, so that notes
/// sharing a meaning can share no word. Keeps every text it's given.
#[derive(Clone)]
struct Fake {
    id: &'static str,
    seen: Rc<RefCell<Vec<String>>>,
    /// Fails after this many calls.
    calls_left: Rc<Cell<usize>>,
}

const AXIS: [&[&str]; 3] = [&["throttle", "limit"], &["kitchen", "recipe"], &["garden"]];

impl Fake {
    fn new() -> Fake {
        Fake::named("fake-1")
    }

    fn named(id: &'static str) -> Fake {
        Fake {
            id,
            seen: Rc::default(),
            calls_left: Rc::new(Cell::new(usize::MAX)),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.borrow().clone()
    }

    fn vector(text: &str) -> Vec<f32> {
        let lower = text.to_lowercase();
        let hits = |words: &&[&str]| words.iter().filter(|w| lower.contains(**w)).count() as f32;
        let mut vector: Vec<f32> = AXIS.iter().map(hits).collect();
        vector.push(0.01);
        let length = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        vector.iter().map(|x| x / length).collect()
    }
}

impl Embedder for Fake {
    fn id(&self) -> &str {
        self.id
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if self.calls_left.get() == 0 {
            return Err(EmbedError::Run("out of calls".to_string()));
        }
        self.calls_left.set(self.calls_left.get().saturating_sub(1));
        self.seen
            .borrow_mut()
            .extend(texts.iter().map(|text| text.to_string()));
        Ok(texts.iter().map(|text| Fake::vector(text)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(Fake::vector(text))
    }
}

struct Vault {
    _dir: TempDir,
    root: PathBuf,
    file: PathBuf,
    graph: Graph,
}

fn write_note(root: &Path, id: &str, extra: &str, body: &str) {
    let text = format!("---\nid: {id}\ntype: Concept\ntitle: {id}\n{extra}---\n{body}\n");
    fs::write(root.join(format!("{id}.md")), text).unwrap();
}

impl Vault {
    fn new(notes: &[(&str, &str)]) -> Vault {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        for (id, body) in notes {
            write_note(&root, id, "", body);
        }
        let file = dir.path().join("search-index.sqlite");
        let graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
        let mut vault = Vault {
            _dir: dir,
            root,
            file,
            graph,
        };
        vault.reindex();
        vault
    }

    fn reindex(&mut self) {
        let options = IndexOptions::default();
        index_vault(
            &mut self.graph,
            &self.root,
            ONTOLOGY,
            &Exclusions::new(&self.root, &[]).unwrap(),
            &Default::default(),
            &options,
        )
        .unwrap();
    }

    fn sync(&self, embedder: Option<&dyn Embedder>) -> Result<kbx_gen::Synced, SyncError> {
        let index = SearchIndex::open(&self.file).unwrap();
        index.sync(&self.graph, &self.root, embedder, &mut |_, _| {})
    }

    fn searcher(&self) -> kbx_gen::BuiltinSearch {
        let index = SearchIndex::open(&self.file).unwrap();
        index.searcher(&self.graph, None).unwrap()
    }
}

fn notes(hits: Vec<kbx_gen::Hit>) -> Vec<String> {
    hits.into_iter().map(|hit| hit.note).collect()
}

const THROTTLE: &str = "# One\n\nThrottle the callers.\n\n# Two\n\nOther words here.";

#[test]
fn an_unchanged_section_is_never_embedded_again() {
    let vault = Vault::new(&[(ID_A, THROTTLE), (ID_B, "# Three\n\nA kitchen.")]);
    let first = Fake::new();
    let synced = vault.sync(Some(&first)).unwrap();
    assert_eq!(first.seen().len(), 3);
    assert_eq!((synced.added, synced.embedded), (3, 3));

    let second = Fake::new();
    let synced = vault.sync(Some(&second)).unwrap();
    assert_eq!(second.seen(), Vec::<String>::new());
    assert_eq!((synced.added, synced.removed, synced.embedded), (0, 0, 0));
}

#[test]
fn an_edit_embeds_only_the_section_it_changed() {
    let mut vault = Vault::new(&[(ID_A, THROTTLE)]);
    vault.sync(Some(&Fake::new())).unwrap();

    let body = THROTTLE.replace("Other words here.", "New words now.");
    write_note(&vault.root, ID_A, "", &body);
    vault.reindex();
    let embedder = Fake::new();
    let synced = vault.sync(Some(&embedder)).unwrap();
    assert_eq!(embedder.seen(), ["# Two New words now."]);
    assert_eq!((synced.added, synced.removed), (1, 1));
}

#[test]
fn a_frontmatter_edit_embeds_nothing() {
    let mut vault = Vault::new(&[(ID_A, THROTTLE)]);
    vault.sync(Some(&Fake::new())).unwrap();

    write_note(&vault.root, ID_A, "aliases: [Rate limits]\n", THROTTLE);
    vault.reindex();
    let embedder = Fake::new();
    vault.sync(Some(&embedder)).unwrap();
    assert_eq!(embedder.seen(), Vec::<String>::new());
}

#[test]
fn the_same_text_in_two_notes_is_embedded_once_and_found_in_both() {
    let vault = Vault::new(&[
        (ID_A, "# Same\n\nThe garden gate."),
        (ID_B, "# Same\n\nThe garden gate."),
        (ID_C, "# Other\n\nThe garden wall."),
    ]);
    let embedder = Fake::new();
    vault.sync(Some(&embedder)).unwrap();
    assert_eq!(embedder.seen().len(), 2);

    let hits = vault.searcher().search(&Query::Text("gate"), 10).unwrap();
    assert_eq!(notes(hits), [ID_A, ID_B]);
}

#[test]
fn a_text_nothing_says_any_more_leaves_the_index() {
    let mut vault = Vault::new(&[
        (ID_A, "# One\n\nZebra crossing."),
        (ID_B, "# Two\n\nGarden."),
    ]);
    vault.sync(Some(&Fake::new())).unwrap();

    fs::remove_file(vault.root.join(format!("{ID_A}.md"))).unwrap();
    vault.reindex();
    let synced = vault.sync(Some(&Fake::new())).unwrap();
    assert_eq!(synced.removed, 1);
    let hits = vault.searcher().search(&Query::Text("zebra"), 10).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn another_model_embeds_everything_again() {
    let vault = Vault::new(&[(ID_A, THROTTLE)]);
    vault.sync(Some(&Fake::new())).unwrap();

    let other = Fake::named("fake-2");
    vault.sync(Some(&other)).unwrap();
    assert_eq!(other.seen().len(), 2);
}

#[test]
fn vectors_counts_what_a_model_has_embedded() {
    let mut vault = Vault::new(&[(ID_A, THROTTLE)]);
    let file = vault.file.clone();
    let index = || SearchIndex::open(&file).unwrap();
    vault.sync(None).unwrap();
    let (embedded, total) = index().vectors("fake-1").unwrap();
    assert_eq!(embedded, 0);
    assert!(total > 0);

    vault.sync(Some(&Fake::new())).unwrap();
    assert_eq!(index().vectors("fake-1").unwrap(), (total, total));
    assert_eq!(index().vectors("fake-2").unwrap(), (0, total));

    write_note(&vault.root, ID_A, "", "# One\n\nChanged.");
    vault.reindex();
    vault.sync(None).unwrap();
    let (embedded, total) = index().vectors("fake-1").unwrap();
    assert!(embedded < total);
}

#[test]
fn a_run_that_stops_keeps_what_it_embedded() {
    let bodies: Vec<String> = (0..100)
        .map(|n| format!("# Heading\n\nLine {n}."))
        .collect();
    let ids: Vec<String> = (0..100).map(|n| format!("01KBX{n:021}")).collect();
    let notes: Vec<(&str, &str)> = ids
        .iter()
        .map(String::as_str)
        .zip(bodies.iter().map(String::as_str))
        .collect();
    let vault = Vault::new(&notes);

    let stopping = Fake::new();
    stopping.calls_left.set(1);
    assert!(vault.sync(Some(&stopping)).is_err());
    let done = stopping.seen().len();
    assert!(done > 0 && done < 100, "{done}");

    let rest = Fake::new();
    vault.sync(Some(&rest)).unwrap();
    assert_eq!(rest.seen().len(), 100 - done);
}

#[test]
fn keywords_work_with_no_model() {
    let vault = Vault::new(&[
        (ID_A, "# One\n\nToken buckets refill."),
        (ID_B, "# Two\n\nToken buckets drain."),
        (ID_C, "# Three\n\nA kitchen recipe."),
    ]);
    let synced = vault.sync(None).unwrap();
    assert_eq!((synced.embedded, synced.unembedded), (0, 3));

    let query = Query::Note {
        note: ID_A,
        text: "Token buckets refill.",
    };
    assert_eq!(notes(vault.searcher().search(&query, 10).unwrap()), [ID_B]);
}

#[test]
fn a_note_is_never_its_own_neighbour_and_vectors_find_what_words_cannot() {
    let vault = Vault::new(&[
        (ID_A, "# One\n\nThrottle the callers."),
        (ID_B, "# Two\n\nA limit on requests."),
        (ID_C, "# Three\n\nA kitchen recipe."),
    ]);
    vault.sync(Some(&Fake::new())).unwrap();

    let query = Query::Note {
        note: ID_A,
        text: "Throttle the callers.",
    };
    let hits = vault.searcher().search(&query, 1).unwrap();
    assert_eq!(notes(hits), [ID_B]);
}

#[test]
fn a_hit_names_a_section_of_its_note() {
    let vault = Vault::new(&[(ID_A, THROTTLE), (ID_B, "# Three\n\nThrottle more.")]);
    vault.sync(Some(&Fake::new())).unwrap();

    let hits = vault
        .searcher()
        .search(&Query::Text("other words"), 1)
        .unwrap();
    let hit = hits.first().unwrap();
    let section: String = vault
        .graph
        .connection()
        .query_row(
            "SELECT heading_path FROM sections WHERE key = ?",
            [&hit.section],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((hit.note.as_str(), section.as_str()), (ID_A, "two"));
}

#[test]
fn a_query_of_fts5_syntax_is_only_words() {
    let vault = Vault::new(&[(ID_A, "# One\n\nNear the end.")]);
    vault.sync(None).unwrap();
    let search = vault.searcher();
    for text in ["\"", "NEAR(", "a AND", "*", "col:umn", ""] {
        search.search(&Query::Text(text), 5).unwrap();
    }
}

#[test]
fn a_file_of_another_version_is_rebuilt() {
    let vault = Vault::new(&[(ID_A, THROTTLE)]);
    vault.sync(Some(&Fake::new())).unwrap();
    let connection = Connection::open(&vault.file).unwrap();
    connection.pragma_update(None, "user_version", 0).unwrap();
    drop(connection);

    assert!(SearchIndex::open_read_only(&vault.file).is_none());
    let embedder = Fake::new();
    vault.sync(Some(&embedder)).unwrap();
    assert_eq!(embedder.seen().len(), 2);
}

#[test]
fn a_file_that_is_not_a_database_is_replaced() {
    let vault = Vault::new(&[(ID_A, THROTTLE)]);
    fs::write(&vault.file, "not sqlite, and long enough to be looked at").unwrap();
    vault.sync(Some(&Fake::new())).unwrap();
}

#[test]
fn a_file_edited_since_the_index_cannot_poison_its_old_hash() {
    let original = "# One\n\nThrottle the callers.";
    let mut vault = Vault::new(&[(ID_A, original)]);
    write_note(&vault.root, ID_A, "", "# One\n\nA kitchen recipe.");
    let fake = Fake::new();
    let synced = vault.sync(Some(&fake)).unwrap();
    assert_eq!((synced.added, synced.embedded), (0, 0));

    write_note(&vault.root, ID_A, "", original);
    vault.reindex();
    let synced = vault.sync(Some(&fake)).unwrap();
    assert_eq!((synced.added, synced.embedded), (1, 1));
    assert!(fake.seen()[0].contains("Throttle"), "{:?}", fake.seen());
}

/// A note of more short sections than a ranking keeps, all matching, and a
/// note matching less well.
fn a_large_note_and_its_neighbour() -> Vault {
    let sections: Vec<String> = (0..250)
        .map(|n| format!("# Part {n}\n\nThrottle item{n}."))
        .collect();
    let neighbour = "# Other\n\nThrottle, and a garden with many more words in it than these.";
    Vault::new(&[(ID_A, &sections.join("\n\n")), (ID_B, neighbour)])
}

#[test]
fn a_large_note_does_not_hide_its_neighbours_from_keywords() {
    let vault = a_large_note_and_its_neighbour();
    vault.sync(None).unwrap();
    let query = Query::Note {
        note: ID_A,
        text: "Throttle",
    };
    assert_eq!(notes(vault.searcher().search(&query, 10).unwrap()), [ID_B]);
}

#[test]
fn a_large_note_does_not_hide_its_neighbours_from_vectors() {
    let vault = a_large_note_and_its_neighbour();
    vault.sync(Some(&Fake::new())).unwrap();
    let query = Query::Note {
        note: ID_A,
        text: "unrelated",
    };
    assert_eq!(notes(vault.searcher().search(&query, 10).unwrap()), [ID_B]);
}
