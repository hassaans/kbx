//! Rows out of the database and back in, and the Turtle export.

use std::path::Path;

use kbx_core::{
    DEFAULT_NAMESPACE, Exclusions, Graph, ImportError, IndexOptions, NOTE_NAMESPACE, Ontology,
    Review, STRIKES, cid, index_vault, parse_reviews, suppressed_pairs, turtle, unreject,
};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use oxttl::TurtleParser;
use rusqlite::params;
use tempfile::TempDir;

const VAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");
const ONTOLOGY: &str = include_str!("../../../schema/ontology.example.yaml");
const LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

struct Db {
    graph: Graph,
    _dir: TempDir,
}

fn empty() -> Db {
    let dir = TempDir::new().unwrap();
    Db {
        graph: Graph::open(&dir.path().join("graph.sqlite")).unwrap(),
        _dir: dir,
    }
}

fn indexed(ontology: &str) -> Db {
    let mut db = empty();
    let vault = Path::new(VAULT).canonicalize().unwrap();
    index_vault(
        &mut db.graph,
        &vault,
        ontology,
        &Exclusions::new(&vault, &[]).unwrap(),
        &Default::default(),
        &IndexOptions::default(),
    )
    .unwrap();
    db
}

fn review(src: &str, decision: &str, decided_at: i64) -> Review {
    Review {
        src: src.to_string(),
        dst: "b".to_string(),
        rel: "see_also".to_string(),
        decision: decision.to_string(),
        reason: Some(format!("{decision}ed\nfor \"reasons\"")),
        strikes: (decision == "reject") as i64,
        decided_at: Some(decided_at),
    }
}

fn parse(text: &str) -> Vec<Triple> {
    let quads = TurtleParser::new().for_slice(text.as_bytes());
    quads.collect::<Result<_, _>>().unwrap()
}

fn note_iri(n: u32) -> String {
    format!("{NOTE_NAMESPACE}01KBX{n:021}")
}

fn subject(triple: &Triple) -> String {
    match &triple.subject {
        NamedOrBlankNode::NamedNode(node) => node.as_str().to_string(),
        other => panic!("not an IRI: {other}"),
    }
}

fn object(triple: &Triple) -> String {
    match &triple.object {
        Term::NamedNode(node) => node.as_str().to_string(),
        Term::Literal(literal) => literal.value().to_string(),
        other => panic!("not an IRI or literal: {other}"),
    }
}

#[test]
fn review_rows_survive_json_lines() {
    let rows = vec![
        review("a", "accept", 10),
        Review {
            reason: None,
            strikes: 0,
            decided_at: None,
            ..review("c", "defer", 0)
        },
    ];
    let text: String = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap() + "\n")
        .collect();
    assert_eq!(parse_reviews(&text).unwrap(), rows);
}

#[test]
fn review_fields_keep_their_names_and_order() {
    let line = serde_json::to_string(&review("a", "accept", 10)).unwrap();
    assert_eq!(
        line,
        r#"{"src":"a","dst":"b","rel":"see_also","decision":"accept","reason":"accepted\nfor \"reasons\"","strikes":0,"decided_at":10}"#
    );
}

#[test]
fn blank_lines_and_crlf_are_fine() {
    let line = serde_json::to_string(&review("a", "accept", 10)).unwrap();
    let rows = parse_reviews(&format!("\r\n{line}\r\n\n")).unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn a_bad_line_is_reported_by_number() {
    let good = serde_json::to_string(&review("a", "accept", 10)).unwrap();
    let error = parse_reviews(&format!("{good}\nnot json\n")).unwrap_err();
    assert!(matches!(error, ImportError::Line(2, _)), "{error:?}");
}

#[test]
fn an_unknown_decision_is_refused() {
    let line = serde_json::to_string(&review("a", "maybe", 10)).unwrap();
    let error = parse_reviews(&line).unwrap_err();
    assert!(matches!(error, ImportError::Decision(1, _)), "{error:?}");
}

#[test]
fn a_field_this_version_cannot_keep_is_refused() {
    let line = r#"{"src":"a","dst":"b","rel":"r","decision":"accept","weight":1}"#;
    assert!(parse_reviews(line).is_err());
}

#[test]
fn importing_twice_changes_nothing() {
    let db = empty();
    let rows = vec![review("a", "accept", 10), review("c", "reject", 11)];
    db.graph.import_reviews(&rows).unwrap();
    db.graph.import_reviews(&rows).unwrap();
    assert_eq!(db.graph.reviews().unwrap(), rows);
}

#[test]
fn an_import_keeps_rows_it_does_not_mention() {
    let db = empty();
    db.graph
        .import_reviews(&[review("a", "accept", 10)])
        .unwrap();
    db.graph
        .import_reviews(&[review("c", "reject", 11)])
        .unwrap();
    assert_eq!(db.graph.reviews().unwrap().len(), 2);
}

#[test]
fn a_newer_decision_replaces_an_older_one_and_not_the_other_way_round() {
    let db = empty();
    db.graph
        .import_reviews(&[review("a", "reject", 10)])
        .unwrap();
    db.graph
        .import_reviews(&[review("a", "accept", 20)])
        .unwrap();
    assert_eq!(db.graph.reviews().unwrap()[0].decision, "accept");

    db.graph
        .import_reviews(&[review("a", "reject", 15)])
        .unwrap();
    assert_eq!(db.graph.reviews().unwrap()[0].decision, "accept");
}

#[test]
fn an_old_backup_does_not_undo_an_unreject() {
    let db = empty();
    let suppressed = Review {
        strikes: STRIKES,
        ..review("a", "reject", 10)
    };
    db.graph
        .import_reviews(std::slice::from_ref(&suppressed))
        .unwrap();
    assert_eq!(suppressed_pairs(db.graph.connection()).unwrap().len(), 1);
    unreject(&db.graph, &cid("a", "b")).unwrap();

    db.graph.import_reviews(&[suppressed]).unwrap();
    assert_eq!(db.graph.reviews().unwrap()[0].strikes, 0);
    assert!(suppressed_pairs(db.graph.connection()).unwrap().is_empty());
}

#[test]
fn a_later_rejection_brings_its_strikes_with_it() {
    let db = empty();
    db.graph
        .import_reviews(&[review("a", "reject", 10)])
        .unwrap();
    let later = Review {
        strikes: STRIKES,
        ..review("a", "reject", 20)
    };
    db.graph.import_reviews(&[later]).unwrap();
    assert_eq!(db.graph.reviews().unwrap()[0].strikes, STRIKES);
}

#[test]
fn a_row_without_strikes_has_none_and_can_still_gather_them() {
    let line = r#"{"src":"a","dst":"b","rel":"see_also","decision":"reject"}"#;
    let rows = parse_reviews(line).unwrap();
    assert_eq!(rows[0].strikes, 0);

    let db = empty();
    db.graph.import_reviews(&rows).unwrap();
    let strikes: Option<i64> = db
        .graph
        .connection()
        .query_row("SELECT strikes FROM reviews", [], |row| row.get(0))
        .unwrap();
    assert_eq!(strikes, Some(0), "NULL would never add up to a suppression");
}

#[test]
fn null_strikes_are_refused() {
    let line = r#"{"src":"a","dst":"b","rel":"r","decision":"reject","strikes":null}"#;
    assert!(matches!(parse_reviews(line), Err(ImportError::Line(1, _))));
}

#[test]
fn edges_come_out_in_a_stable_order_with_every_column() {
    let db = indexed(ONTOLOGY);
    let edges = db.graph.edges().unwrap();
    assert!(!edges.is_empty());
    let key = |e: &kbx_core::EdgeRow| {
        (
            e.src.clone(),
            e.dst.clone(),
            e.rel.clone(),
            e.origin.clone(),
        )
    };
    assert!(edges.windows(2).all(|pair| key(&pair[0]) < key(&pair[1])));
    let line: serde_json::Value = serde_json::to_value(&edges[0]).unwrap();
    let fields: Vec<_> = line.as_object().unwrap().keys().cloned().collect();
    let mut expected = [
        "dst",
        "inverse_of",
        "line",
        "origin",
        "rel",
        "src",
        "src_section",
        "weight",
    ];
    expected.sort();
    assert_eq!(fields, expected);
}

#[test]
fn turtle_from_the_fixture_parses_and_carries_the_graph() {
    let db = indexed(ONTOLOGY);
    let ontology = Ontology::parse(ONTOLOGY).unwrap();
    let triples = parse(&turtle(&db.graph, &ontology).unwrap());

    let notes = db.graph.stats().unwrap().nodes as usize;
    let of = |predicate: &str| {
        let matching = triples.iter().filter(|t| t.predicate.as_str() == predicate);
        matching.collect::<Vec<_>>()
    };
    assert_eq!(of(TYPE).len(), notes);
    assert_eq!(of(LABEL).len(), notes);

    let depends_on = format!("{DEFAULT_NAMESPACE}depends_on");
    let wanted = of(&depends_on);
    assert!(!wanted.is_empty());
    assert!(
        wanted
            .iter()
            .all(|t| subject(t).starts_with(NOTE_NAMESPACE))
    );
    assert!(wanted.iter().all(|t| object(t).starts_with(NOTE_NAMESPACE)));
    let class = of(TYPE).into_iter().find(|t| subject(t) == note_iri(8));
    assert_eq!(
        object(class.unwrap()),
        format!("{DEFAULT_NAMESPACE}Procedure")
    );
}

#[test]
fn inverse_rows_are_not_stated_twice() {
    let db = indexed(ONTOLOGY);
    let ontology = Ontology::parse(ONTOLOGY).unwrap();
    let triples = parse(&turtle(&db.graph, &ontology).unwrap());
    let required_by = format!("{DEFAULT_NAMESPACE}required_by");
    assert!(triples.iter().all(|t| t.predicate.as_str() != required_by));

    let mut unique: Vec<_> = triples.iter().map(ToString::to_string).collect();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), triples.len());
}

#[test]
fn mentions_have_a_predicate_and_stray_keys_do_not() {
    let db = indexed(ONTOLOGY);
    let connection = db.graph.connection();
    let insert = "INSERT INTO edges (src, dst, rel, origin) VALUES (?, ?, ?, ?)";
    let (a, b) = ("01KBX000000000000000000008", "01KBX000000000000000000012");
    connection
        .execute(insert, params![a, b, "mentions", "inline"])
        .unwrap();
    connection
        .execute(insert, params![a, b, "no such key", "frontmatter"])
        .unwrap();
    let ontology = Ontology::parse(ONTOLOGY).unwrap();
    let text = turtle(&db.graph, &ontology).unwrap();
    let triples = parse(&text);
    let mentions = format!("{DEFAULT_NAMESPACE}mentions");
    assert!(triples.iter().any(|t| t.predicate.as_str() == mentions));
    assert!(!text.contains("no%20such"));
}

#[test]
fn a_relation_with_an_iri_uses_it_and_uri_means_the_same() {
    for key in ["iri", "uri"] {
        let related = "http://www.w3.org/2004/02/skos/core#related";
        let yaml = ONTOLOGY.replace(
            "see_also:    {inverse: see_also,",
            &format!("see_also:    {{{key}: \"{related}\", inverse: see_also,"),
        );
        let ontology = Ontology::parse(&yaml).unwrap();
        assert_eq!(ontology.relations["see_also"].iri.as_deref(), Some(related));

        let db = indexed(&yaml);
        let connection = db.graph.connection();
        connection
            .execute(
                "INSERT INTO edges (src, dst, rel, weight, origin)
                 VALUES ('01KBX000000000000000000008', '01KBX000000000000000000012',
                         'see_also', 0.3, 'frontmatter')",
                [],
            )
            .unwrap();
        let triples = parse(&turtle(&db.graph, &ontology).unwrap());
        assert!(triples.iter().any(|t| t.predicate.as_str() == related));
        let default = format!("{DEFAULT_NAMESPACE}see_also");
        assert!(triples.iter().all(|t| t.predicate.as_str() != default));
    }
}

#[test]
fn an_iri_that_would_break_the_turtle_is_refused() {
    for bad in ["not an iri", "relative/path", "http://a/b>c", "http://a/ b"] {
        let yaml = ONTOLOGY.replace(
            "see_also:    {inverse: see_also,",
            &format!("see_also:    {{iri: \"{bad}\", inverse: see_also,"),
        );
        let error = Ontology::parse(&yaml).unwrap_err().to_string();
        assert!(error.contains("see_also"), "{bad}: {error}");
    }
}

#[test]
fn awkward_titles_ids_and_classes_stay_valid_turtle() {
    let db = empty();
    let title = "Say \"hi\" \\ to\nthe\ttab\r and é 日本 \u{1}";
    db.graph
        .connection()
        .execute(
            "INSERT INTO files (path, id, type, title) VALUES ('a.md', ?, ?, ?)",
            params!["id with space/and>angle", "My Class", title],
        )
        .unwrap();
    let ontology = Ontology::parse(ONTOLOGY).unwrap();
    let triples = parse(&turtle(&db.graph, &ontology).unwrap());

    let label = triples.iter().find(|t| t.predicate.as_str() == LABEL);
    assert_eq!(object(label.unwrap()), title);
    assert_eq!(
        subject(label.unwrap()),
        format!("{NOTE_NAMESPACE}id%20with%20space%2Fand%3Eangle")
    );
    let class = triples.iter().find(|t| t.predicate.as_str() == TYPE);
    assert_eq!(
        object(class.unwrap()),
        format!("{DEFAULT_NAMESPACE}My%20Class")
    );
}
