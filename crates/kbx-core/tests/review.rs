//! Accepting, rejecting, deferring and unrejecting candidates.

use std::fs;
use std::path::{Path, PathBuf};

use kbx_core::{
    Decision, Graph, IndexOptions, Ontology, ReviewError, STRIKES, accept, cid, defer, index_vault,
    ordered, parse_note, reject, section_cid, split_frontmatter, suppressed_pairs, unreject,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const ONTOLOGY: &str = "
version: 1
classes:
  Concept: {description: c}
  Procedure: {description: p}
  Decision: {description: d}
relations:
  depends_on: {inverse: required_by, domain: [Procedure], range: [Concept, Procedure], weight: 0.9}
  see_also: {inverse: see_also, domain: [any], range: [any], weight: 0.3, symmetric: true}
  part_of: {inverse: has_part, domain: [any], range: [any], weight: 1.0}
  supersedes: {inverse: superseded_by, domain: [Decision], range: [Decision], weight: 0.8, exclusive: true}
  example_of: {inverse: has_example, domain: [any], range: [any], weight: 0.6, derived_only: true}
  twin_of: {inverse: twin_of, domain: [any], range: [any], weight: 0.5, symmetric: true, exclusive: true}
judge:
  vocabulary: [depends_on, see_also, part_of, supersedes, example_of, none]
  direction: from_schema
";

struct Vault {
    dir: TempDir,
    graph: Graph,
    ontology: Ontology,
}

impl Vault {
    /// Notes `a`, `b` and `c`, of the classes given.
    fn new(classes: [&str; 3]) -> Vault {
        let dir = TempDir::new().unwrap();
        for (name, class) in ["a", "b", "c"].into_iter().zip(classes) {
            let text = format!(
                "---\nid: {name}\ntype: {class}\ntitle: Note {name} # kept\n---\nBody of {name}.\r\n"
            );
            fs::write(dir.path().join(format!("{name}.md")), text).unwrap();
        }
        let graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
        let mut vault = Vault {
            dir,
            graph,
            ontology: Ontology::parse(ONTOLOGY).unwrap(),
        };
        vault.index();
        vault
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn index(&mut self) {
        let root = self.root();
        index_vault(&mut self.graph, &root, ONTOLOGY, &IndexOptions::default()).unwrap();
    }

    fn db(&self) -> &Connection {
        self.graph.connection()
    }

    /// Stores the candidate as `kbx suggest` would.
    fn candidate(&self, src: &str, dst: &str) -> String {
        let cid = cid(src, dst);
        self.db()
            .execute(
                "INSERT INTO candidates (cid, src, dst, src_hash, dst_hash)
                 VALUES (?1, ?2, ?3, (SELECT content_hash FROM files WHERE id = ?2),
                     (SELECT content_hash FROM files WHERE id = ?3))
                 ON CONFLICT(cid) DO NOTHING",
                params![cid, src, dst],
            )
            .unwrap();
        cid
    }

    fn accept(&self, cid: &str, rel: &str) -> Result<kbx_core::Accepted, ReviewError> {
        let decision = Decision {
            rel: Some(rel),
            reason: None,
        };
        accept(&self.graph, &self.root(), &self.ontology, cid, decision)
    }

    fn reject(&self, src: &str, dst: &str, rel: Option<&str>) -> kbx_core::Rejected {
        let cid = self.candidate(src, dst);
        let decision = Decision {
            rel,
            reason: Some("no"),
        };
        reject(&self.graph, &self.ontology, &cid, decision).unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root().join(format!("{name}.md"))).unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.root().join(format!("{name}.md")), text).unwrap();
    }

    fn snapshot(&self) -> Vec<String> {
        ["a", "b", "c"].map(|name| self.read(name)).to_vec()
    }

    fn links(&self, name: &str, key: &str) -> Vec<String> {
        let note = parse_note(Path::new("n.md"), &self.read(name)).unwrap();
        let entry = note.relation(key);
        entry.map_or(Vec::new(), |e| {
            e.links.iter().map(|l| l.target.clone()).collect()
        })
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

/// What's around the frontmatter of a file.
fn outside(text: &str) -> (String, String) {
    let frontmatter = split_frontmatter(text).unwrap().unwrap();
    let end = frontmatter.yaml_start + frontmatter.yaml.len();
    (text[..frontmatter.yaml_start].into(), text[end..].into())
}

#[test]
fn accepting_writes_the_relation_and_the_inverse_and_nothing_else() {
    let vault = Vault::new(["Procedure", "Concept", "Concept"]);
    let before = vault.snapshot();
    let cid = vault.candidate("a", "b");
    let accepted = vault.accept(&cid, "depends_on").unwrap();

    assert_eq!((accepted.src.as_str(), accepted.dst.as_str()), ("a", "b"));
    assert_eq!(
        accepted.written,
        [PathBuf::from("a.md"), PathBuf::from("b.md")]
    );
    assert_eq!(vault.links("a", "depends_on"), ["b"]);
    assert_eq!(vault.links("b", "required_by"), ["a"]);
    assert_eq!(outside(&before[0]), outside(&vault.read("a")));
    assert_eq!(outside(&before[1]), outside(&vault.read("b")));
    assert_eq!(before[2], vault.read("c"));
    assert!(vault.read("a").contains("title: Note a # kept\n"));
    assert!(
        vault
            .read("a")
            .contains("metadata: {type: Procedure, tags: []}")
    );
    assert!(
        vault
            .read("b")
            .contains("metadata: {type: Concept, tags: []}")
    );

    assert_eq!(vault.count("SELECT COUNT(*) FROM candidates"), 0);
    let row: (String, String, String, String) = vault
        .db()
        .query_row("SELECT src, dst, rel, decision FROM reviews", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap();
    assert_eq!(
        row,
        ("a".into(), "b".into(), "depends_on".into(), "accept".into())
    );
}

#[test]
fn a_symmetric_relation_is_written_under_its_own_name_on_both_notes() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    vault.accept(&cid, "see_also").unwrap();
    assert_eq!(vault.links("a", "see_also"), ["b"]);
    assert_eq!(vault.links("b", "see_also"), ["a"]);
}

#[test]
fn a_derived_only_inverse_leaves_the_target_alone() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let before = vault.read("b");
    let cid = vault.candidate("a", "b");
    let accepted = vault.accept(&cid, "example_of").unwrap();
    assert_eq!(accepted.written, [PathBuf::from("a.md")]);
    assert_eq!(vault.links("a", "example_of"), ["b"]);
    assert_eq!(vault.read("b"), before);
}

#[test]
fn the_schema_decides_direction() {
    let vault = Vault::new(["Concept", "Procedure", "Concept"]);
    let cid = vault.candidate("a", "b");
    let accepted = vault.accept(&cid, "depends_on").unwrap();
    assert_eq!((accepted.src.as_str(), accepted.dst.as_str()), ("b", "a"));
    assert_eq!(vault.links("b", "depends_on"), ["a"]);
    assert_eq!(vault.links("a", "required_by"), ["b"]);
}

#[test]
fn an_inverse_name_reads_the_relation_the_other_way() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    let accepted = vault.accept(&cid, "has_part").unwrap();
    assert_eq!(accepted.rel, "part_of");
    assert_eq!(vault.links("b", "part_of"), ["a"]);
    assert_eq!(vault.links("a", "has_part"), ["b"]);
}

#[test]
fn classes_that_fit_neither_way_are_refused() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let before = vault.snapshot();
    let cid = vault.candidate("a", "b");
    let error = vault.accept(&cid, "depends_on").unwrap_err();
    assert!(matches!(error, ReviewError::OutOfSchema { .. }), "{error}");
    assert_eq!(vault.snapshot(), before);
}

#[test]
fn a_second_target_of_an_exclusive_relation_is_refused() {
    let vault = Vault::new(["Decision", "Decision", "Decision"]);
    let first = vault.candidate("a", "b");
    vault.accept(&first, "supersedes").unwrap();
    let before = vault.snapshot();
    // Fresh hashes, as a re-run of `kbx suggest` would record.
    let mut vault = vault;
    vault.index();
    let second = vault.candidate("a", "c");
    let error = vault.accept(&second, "supersedes").unwrap_err();
    assert!(matches!(error, ReviewError::Exclusive { .. }), "{error}");
    assert_eq!(vault.snapshot(), before);
}

#[test]
fn a_symmetric_exclusive_relation_is_checked_on_the_target_too() {
    let mut vault = Vault::new(["Concept", "Concept", "Concept"]);
    let taken = "---\nid: b\ntype: Concept\ntitle: T\ntwin_of: [\"[[c]]\"]\n---\n";
    vault.write("b", taken);
    vault.index();
    let cid = vault.candidate("a", "b");
    let before = vault.snapshot();
    let error = vault.accept(&cid, "twin_of").unwrap_err();
    assert!(
        matches!(&error, ReviewError::Exclusive { path, .. } if path == Path::new("b.md")),
        "{error}"
    );
    assert_eq!(vault.snapshot(), before);
    assert_eq!(vault.count("SELECT COUNT(*) FROM reviews"), 0);
}

/// `[[b-child]]` is the note with that id, not `b` with a slug.
#[test]
fn a_longer_id_starting_with_the_target_is_another_note() {
    let mut vault = Vault::new(["Concept", "Concept", "Concept"]);
    vault.write("c", "---\nid: b-child\ntype: Concept\ntitle: T\n---\n");
    let linked = "---\nid: a\ntype: Concept\ntitle: T\nsee_also: [\"[[b-child]]\"]\n---\n";
    vault.write("a", linked);
    vault.index();
    let cid = vault.candidate("a", "b");
    vault.accept(&cid, "see_also").unwrap();
    assert_eq!(vault.links("a", "see_also"), ["b-child", "b"]);
    assert_eq!(vault.links("b", "see_also"), ["a"]);
}

#[test]
fn a_link_by_slug_or_filename_already_counts() {
    for target in ["b-a-slug", "B"] {
        let mut vault = Vault::new(["Concept", "Concept", "Concept"]);
        let text =
            format!("---\nid: a\ntype: Concept\ntitle: T\nsee_also: [\"[[{target}]]\"]\n---\n");
        vault.write("a", &text);
        vault.index();
        let cid = vault.candidate("a", "b");
        vault.accept(&cid, "see_also").unwrap();
        assert_eq!(vault.links("a", "see_also"), [target]);
    }
}

#[test]
fn an_unknown_relation_or_none_is_refused() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    for rel in ["nonsense", "none"] {
        let error = vault.accept(&cid, rel).unwrap_err();
        assert!(matches!(error, ReviewError::UnknownRelation(_)), "{error}");
    }
}

#[test]
fn the_relation_comes_from_the_latest_verdict_that_names_one() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    let none = Decision::default();
    let root = vault.root();
    let error = accept(&vault.graph, &root, &vault.ontology, &cid, none).unwrap_err();
    assert!(matches!(error, ReviewError::NoRelation(_)), "{error}");

    let verdict =
        "INSERT INTO verdicts (cid, backend, model, prompt_hash, verdict, rel, created_at)
                   VALUES (?, ?, 'm', 'p', 'accept', ?, ?)";
    vault
        .db()
        .execute(verdict, params![cid, "old", "part_of", 1])
        .unwrap();
    vault
        .db()
        .execute(verdict, params![cid, "new", "see_also", 2])
        .unwrap();
    vault
        .db()
        .execute(verdict, params![cid, "newest", "none", 3])
        .unwrap();
    let accepted = accept(&vault.graph, &root, &vault.ontology, &cid, none).unwrap();
    assert_eq!(accepted.rel, "see_also");
}

#[test]
fn an_edit_to_either_note_refuses_acceptance() {
    for changed in ["a", "b"] {
        let vault = Vault::new(["Procedure", "Concept", "Concept"]);
        let cid = vault.candidate("a", "b");
        let text = vault.read(changed) + "An edit.\n";
        vault.write(changed, &text);
        let before = vault.snapshot();

        let error = vault.accept(&cid, "depends_on").unwrap_err();
        assert!(matches!(error, ReviewError::Changed(_)), "{error}");
        assert!(error.to_string().contains("kbx suggest"), "{error}");
        assert_eq!(vault.snapshot(), before);
        assert_eq!(vault.count("SELECT COUNT(*) FROM reviews"), 0);
        assert_eq!(vault.count("SELECT COUNT(*) FROM candidates"), 1);
    }
}

/// `files.content_hash` moves with the index; the candidate's hash doesn't.
#[test]
fn an_edit_is_still_refused_after_a_reindex() {
    let mut vault = Vault::new(["Procedure", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    let text = vault.read("a") + "An edit.\n";
    vault.write("a", &text);
    vault.index();
    let error = vault.accept(&cid, "depends_on").unwrap_err();
    assert!(matches!(error, ReviewError::Changed(_)), "{error}");
    assert_eq!(vault.read("a"), text);
}

#[test]
fn a_candidate_without_recorded_hashes_is_refused() {
    let vault = Vault::new(["Procedure", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    vault
        .db()
        .execute("UPDATE candidates SET src_hash = NULL, dst_hash = NULL", [])
        .unwrap();
    let error = vault.accept(&cid, "depends_on").unwrap_err();
    assert!(matches!(error, ReviewError::Changed(_)), "{error}");
}

#[test]
fn a_missing_target_is_target_unavailable_and_nothing_is_written() {
    let vault = Vault::new(["Procedure", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    let source = vault.read("a");
    fs::remove_file(vault.root().join("b.md")).unwrap();
    let error = vault.accept(&cid, "depends_on").unwrap_err();
    assert!(
        matches!(error, ReviewError::TargetUnavailable(_)),
        "{error}"
    );
    assert!(
        error.to_string().starts_with("target.unavailable"),
        "{error}"
    );
    assert_eq!(vault.read("a"), source);
    assert_eq!(vault.count("SELECT COUNT(*) FROM reviews"), 0);
}

#[test]
fn a_missing_source_is_refused() {
    let vault = Vault::new(["Procedure", "Concept", "Concept"]);
    let cid = vault.candidate("a", "b");
    let target = vault.read("b");
    fs::remove_file(vault.root().join("a.md")).unwrap();
    let error = vault.accept(&cid, "depends_on").unwrap_err();
    assert!(
        matches!(error, ReviewError::SourceUnavailable(_)),
        "{error}"
    );
    assert_eq!(vault.read("b"), target);
}

/// A target that can't be edited must not leave the source half done.
#[test]
fn a_target_that_cannot_be_edited_leaves_the_source_alone() {
    let mut vault = Vault::new(["Procedure", "Concept", "Concept"]);
    vault.write(
        "b",
        "---\nid: b\ntype: Concept\ntitle: T\nrequired_by: plain\n---\n",
    );
    vault.index();
    let cid = vault.candidate("a", "b");
    let before = vault.snapshot();
    let error = vault.accept(&cid, "depends_on").unwrap_err();
    assert!(matches!(error, ReviewError::Write(..)), "{error}");
    assert_eq!(vault.snapshot(), before);
    assert_eq!(vault.count("SELECT COUNT(*) FROM reviews"), 0);
}

#[test]
fn candidates_sharing_a_note_can_be_accepted_together() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let first = vault.candidate("a", "b");
    let second = vault.candidate("c", "a");
    vault.accept(&first, "see_also").unwrap();
    vault.accept(&second, "see_also").unwrap();
    assert_eq!(vault.links("a", "see_also"), ["b", "c"]);
}

#[test]
fn an_unknown_candidate_is_refused() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let error = vault.accept("nope", "see_also").unwrap_err();
    assert!(matches!(error, ReviewError::UnknownCandidate(_)), "{error}");
}

#[test]
fn a_rejection_keeps_its_reason_and_leaves_the_queue() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let rejected = vault.reject("a", "b", Some("see_also"));
    assert_eq!(rejected.strikes, 1);
    assert!(!rejected.suppressed());
    assert_eq!(vault.count("SELECT COUNT(*) FROM candidates"), 0);
    let reason: String = vault
        .db()
        .query_row("SELECT reason FROM reviews", [], |row| row.get(0))
        .unwrap();
    assert_eq!(reason, "no");
}

#[test]
fn a_rejection_with_no_relation_is_recorded_under_none() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    assert_eq!(vault.reject("a", "b", None).rel, "none");
}

#[test]
fn three_strikes_across_relations_and_directions_suppress_the_pair() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let suppressed = || suppressed_pairs(vault.db()).unwrap();
    vault.reject("a", "b", Some("see_also"));
    assert!(suppressed().is_empty());
    vault.reject("b", "a", Some("part_of"));
    assert!(suppressed().is_empty());
    let third = vault.reject("a", "b", None);
    assert_eq!(third.strikes, STRIKES);
    assert!(third.suppressed());
    assert!(suppressed().contains(&ordered("b", "a")));
}

#[test]
fn repeating_one_relation_strikes_too() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    for _ in 0..STRIKES {
        vault.reject("a", "b", Some("see_also"));
    }
    assert_eq!(vault.count("SELECT COUNT(*) FROM reviews"), 1);
    assert_eq!(suppressed_pairs(vault.db()).unwrap().len(), 1);
}

#[test]
fn strikes_on_one_pair_do_not_count_for_another() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    vault.reject("a", "b", None);
    vault.reject("a", "c", None);
    vault.reject("b", "c", None);
    assert!(suppressed_pairs(vault.db()).unwrap().is_empty());
}

#[test]
fn unreject_lifts_the_pair_and_keeps_the_history() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    vault.reject("a", "b", Some("see_also"));
    vault.reject("b", "a", Some("part_of"));
    vault.reject("a", "b", None);

    let lifted = unreject(&vault.graph, &cid("a", "b")).unwrap();
    assert_eq!(lifted.strikes, STRIKES);
    assert!(suppressed_pairs(vault.db()).unwrap().is_empty());
    let kept = "SELECT COUNT(*) FROM reviews WHERE decision = 'reject' AND reason = 'no'";
    assert_eq!(vault.count(kept), 3);

    // A clean slate: it takes three more.
    assert_eq!(vault.reject("a", "b", None).strikes, 1);
    assert!(suppressed_pairs(vault.db()).unwrap().is_empty());
}

#[test]
fn unreject_of_a_pair_with_no_strikes_is_refused() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    let error = unreject(&vault.graph, &cid("a", "b")).unwrap_err();
    assert!(matches!(error, ReviewError::NotRejected(_)), "{error}");
}

#[test]
fn deferring_keeps_the_candidate_and_the_strikes() {
    let vault = Vault::new(["Concept", "Concept", "Concept"]);
    vault.reject("a", "b", None);
    let cid = vault.candidate("a", "b");
    let decision = Decision {
        rel: None,
        reason: Some("later"),
    };
    defer(&vault.graph, &vault.ontology, &cid, decision).unwrap();
    assert_eq!(vault.count("SELECT COUNT(*) FROM candidates"), 1);
    let row: (String, String, i64) = vault
        .db()
        .query_row("SELECT decision, reason, strikes FROM reviews", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(row, ("defer".into(), "later".into(), 1));
}

#[test]
fn an_older_database_gains_the_hash_columns() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.sqlite");
    let old = Connection::open(&path).unwrap();
    old.execute_batch(
        "CREATE TABLE candidates (cid TEXT PRIMARY KEY, src TEXT, dst TEXT, generators TEXT,
             features TEXT, score REAL, created_at INTEGER);
         INSERT INTO candidates (cid, src, dst) VALUES ('c', 'a', 'b');",
    )
    .unwrap();
    drop(old);
    for _ in 0..2 {
        let graph = Graph::open(&path).unwrap();
        let hash: Option<String> = graph
            .connection()
            .query_row("SELECT src_hash FROM candidates", [], |row| row.get(0))
            .unwrap();
        assert_eq!(hash, None);
    }
}

#[test]
fn a_section_cid_depends_on_the_sections_at_each_end() {
    let plain = cid("a", "b");
    assert_eq!(section_cid("a", "b", None, None), plain);

    let both = section_cid("a", "b", Some("x"), Some("y"));
    assert_ne!(both, plain);
    assert_eq!(section_cid("b", "a", Some("y"), Some("x")), both);
    assert_ne!(section_cid("a", "b", Some("y"), Some("x")), both);
    assert_ne!(section_cid("a", "b", Some("x"), None), both);
    assert_ne!(
        section_cid("a", "b", Some("x"), None),
        section_cid("a", "b", None, Some("x"))
    );
}
