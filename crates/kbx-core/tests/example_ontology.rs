use kbx_core::{EXAMPLE_ONTOLOGY, Ontology};

/// `cargo publish` packages only the crate, so the binary embeds a copy of the
/// file that the repo documents.
#[test]
fn the_embedded_ontology_matches_the_published_example() {
    assert_eq!(
        EXAMPLE_ONTOLOGY,
        include_str!("../../../schema/ontology.example.yaml")
    );
    Ontology::parse(EXAMPLE_ONTOLOGY).unwrap();
}
