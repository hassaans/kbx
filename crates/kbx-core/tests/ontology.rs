use kbx_core::{Direction, Ontology, OntologyError};

const EXAMPLE: &str = include_str!("../../../schema/ontology.example.yaml");

fn broken(from: &str, to: &str) -> String {
    assert!(EXAMPLE.contains(from), "{from}");
    match Ontology::parse(&EXAMPLE.replace(from, to)) {
        Err(OntologyError::Invalid(message)) => message,
        other => panic!("{from} -> {to}: {other:?}"),
    }
}

#[test]
fn parses_the_example() {
    let ontology = Ontology::parse(EXAMPLE).unwrap();
    assert_eq!(ontology.version, 1);
    assert_eq!(ontology.classes.len(), 5);
    assert_eq!(ontology.classes["Procedure"].requires, ["depends_on"]);
    assert_eq!(ontology.relations.len(), 7);

    let part_of = &ontology.relations["part_of"];
    assert_eq!(part_of.inverse, "has_part");
    assert_eq!(part_of.weight, 1.0);
    assert!(part_of.transitive && !part_of.symmetric && !part_of.exclusive);
    assert!(ontology.relations["supersedes"].exclusive);
    assert!(ontology.relations["see_also"].symmetric);

    assert_eq!(ontology.judge.vocabulary.len(), 8);
    assert_eq!(ontology.judge.direction, Direction::FromSchema);
}

#[test]
fn the_example_has_no_min_confidence() {
    assert!(!EXAMPLE.contains("min_confidence"));
}

#[test]
fn unknown_keys_are_rejected() {
    let typo = EXAMPLE.replace("requires:", "requries:");
    assert_ne!(typo, EXAMPLE);
    assert!(matches!(
        Ontology::parse(&typo),
        Err(OntologyError::Yaml(_))
    ));
    let confidence = EXAMPLE.replace("  vocabulary:", "  min_confidence: 0.6\n  vocabulary:");
    assert_ne!(confidence, EXAMPLE);
    assert!(matches!(
        Ontology::parse(&confidence),
        Err(OntologyError::Yaml(_))
    ));
}

#[test]
fn unsupported_versions_are_rejected() {
    let next = EXAMPLE.replace("version: 1", "version: 2");
    assert!(matches!(
        Ontology::parse(&next),
        Err(OntologyError::UnsupportedVersion(2))
    ));
}

#[test]
fn lookup_finds_relations_and_their_inverses() {
    let ontology = Ontology::parse(EXAMPLE).unwrap();

    let forward = ontology.lookup("depends_on").unwrap();
    assert_eq!(forward.name, "depends_on");
    assert!(!forward.inverted);
    assert_eq!(forward.range(), ["Concept", "Procedure", "Entity"]);

    let inverse = ontology.lookup("required_by").unwrap();
    assert_eq!(inverse.name, "depends_on");
    assert!(inverse.inverted);
    assert_eq!(inverse.range(), ["Procedure"]);
    assert_eq!(inverse.domain(), ["Concept", "Procedure", "Entity"]);

    assert!(!ontology.lookup("contradicts").unwrap().inverted);
    assert!(ontology.lookup("supersedes").unwrap().exclusive());
    assert!(!ontology.lookup("superseded_by").unwrap().exclusive());
    assert!(ontology.lookup("none").is_none());
    assert!(ontology.lookup("relates_to").is_none());
}

#[test]
fn dangling_references_are_rejected() {
    assert!(broken("requires: [depends_on]", "requires: [nope]").contains("nope"));
    assert!(broken("domain: [Decision]", "domain: [Nope]").contains("Nope"));
    assert!(broken("range: [Decision]", "range: [Nope]").contains("Nope"));
    assert!(broken("vocabulary: [part_of,", "vocabulary: [nope,").contains("nope"));
}

#[test]
fn inconsistent_relations_are_rejected() {
    assert!(broken("inverse: contradicts", "inverse: conflicts_with").contains("symmetric"));
    assert!(broken("inverse: required_by", "inverse: has_part").contains("has_part"));
    assert!(broken("inverse: required_by", "inverse: part_of").contains("part_of"));
}

#[test]
fn reserved_names_are_rejected() {
    assert!(
        broken(
            "  see_also:    {inverse: see_also,",
            "  tags:    {inverse: tags,"
        )
        .contains("tags")
    );
    assert!(broken("inverse: has_part", "inverse: aliases").contains("aliases"));
    assert!(broken("  Reference:", "  any:").contains("any"));
    assert!(
        broken(
            "  see_also:    {inverse: see_also,",
            "  none:    {inverse: none,"
        )
        .contains("none")
    );
}
