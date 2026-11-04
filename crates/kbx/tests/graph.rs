//! Drives `kbx graph` over an indexed copy of the fixture vault.

mod common;

use std::fs;

use common::{Fixture, id, stderr};
use rusqlite::Connection;

fn indexed() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault
}

fn count(vault: &Fixture, table: &str) -> i64 {
    Connection::open(vault.root().join(".kbx/graph.sqlite"))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn neighbors_gives_the_relation_direction_and_class_of_each() {
    let vault = indexed();
    let stdout = vault.ok(&["graph", "neighbors", &id(19)]);
    let want = [
        format!("-depends_on-> {} (Concept) ", id(7)),
        format!("-implements-> {} (Decision) ", id(14)),
    ];
    for row in want {
        assert!(stdout.contains(&row), "{row} is missing from:\n{stdout}");
    }
}

#[test]
fn neighbors_shows_derived_inverses_as_typed_edges() {
    let vault = indexed();
    let stdout = vault.ok(&["graph", "neighbors", &id(7)]);
    let row = format!("-required_by-> {} (Procedure) ", id(19));
    assert!(stdout.contains(&row), "{stdout}");
    assert!(stdout.contains("derived"), "{stdout}");
}

#[test]
fn neighbors_shows_edges_that_have_no_mirror_from_both_ends() {
    let vault = indexed();
    vault.write(
        "scratch/mentioner.md",
        &format!(
            "---\nid: {}\ntype: Concept\ntitle: Mentioner\n---\nSee [[{}]].\n",
            id(31),
            id(6)
        ),
    );
    vault.ok(&["index"]);

    let stdout = vault.ok(&["graph", "neighbors", &id(31)]);
    assert!(
        stdout.contains(&format!("-mentions-> {}", id(6))),
        "{stdout}"
    );
    let stdout = vault.ok(&["graph", "neighbors", &id(6)]);
    assert!(
        stdout.contains(&format!("<-mentions- {}", id(31))),
        "{stdout}"
    );

    // `relates_to` isn't in the ontology, so nothing derives its inverse.
    let stdout = vault.ok(&["graph", "neighbors", &id(14)]);
    assert!(
        stdout.contains(&format!("<-relates_to- {}", id(18))),
        "{stdout}"
    );
}

#[test]
fn an_edge_with_a_mirror_is_listed_once() {
    let vault = indexed();
    let stdout = vault.ok(&["graph", "neighbors", &id(19)]);
    let rows = stdout.matches(id(7).as_str()).count();
    assert_eq!(rows, 1, "{stdout}");
}

#[test]
fn neighbors_of_an_unknown_id_is_an_error() {
    let vault = indexed();
    let output = vault.kbx(&["graph", "neighbors", "01NOPE"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("01NOPE"), "{}", stderr(&output));
}

#[test]
fn path_gives_each_typed_hop() {
    let vault = indexed();
    let stdout = vault.ok(&["graph", "path", &id(19), &id(14)]);
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(
        lines[0].starts_with(&format!("{} (Procedure) ", id(19))),
        "{stdout}"
    );
    assert!(
        lines[1].contains(&format!("-implements-> {} (Decision) ", id(14))),
        "{stdout}"
    );
}

#[test]
fn path_finds_the_shortest_route_through_other_notes() {
    let vault = indexed();
    // Unlinked, with both depending on the same two Concepts.
    let stdout = vault.ok(&["graph", "path", &id(8), &id(9)]);
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(lines[1].contains("-depends_on-> "), "{stdout}");
    assert!(
        lines[2].contains(&format!("-required_by-> {} ", id(9))),
        "{stdout}"
    );
}

#[test]
fn a_path_from_a_note_to_itself_has_no_hops() {
    let vault = indexed();
    let stdout = vault.ok(&["graph", "path", &id(7), &id(7)]);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
}

#[test]
fn no_path_is_an_error() {
    let vault = indexed();
    vault.write(
        "scratch/island.md",
        &format!("---\nid: {}\ntype: Concept\ntitle: Island\n---\n", id(32)),
    );
    vault.ok(&["index"]);
    let output = vault.kbx(&["graph", "path", &id(7), &id(32)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no path"), "{}", stderr(&output));
}

#[test]
fn stats_prints_node_edge_and_section_counts() {
    let vault = indexed();
    let stdout = vault.ok(&["graph", "stats"]);
    for (label, table) in [
        ("nodes", "files"),
        ("edges", "edges"),
        ("sections", "sections"),
    ] {
        let row = format!("{label} {}", count(&vault, table));
        assert!(
            stdout.lines().any(|line| line == row),
            "{row} is missing from:\n{stdout}"
        );
    }
    assert!(stdout.lines().any(|line| line == "nodes 25"), "{stdout}");
}

#[test]
fn stats_on_a_new_index_is_all_zero() {
    let vault = Fixture::new();
    let stdout = vault.ok(&["graph", "stats"]);
    assert!(stdout.contains("nodes 0"), "{stdout}");
}

#[test]
fn graph_reads_the_index_not_the_files() {
    let vault = indexed();
    fs::remove_file(
        vault
            .root()
            .join("lint/01KBX000000000000000000019-rel-range.md"),
    )
    .unwrap();
    let stdout = vault.ok(&["graph", "neighbors", &id(19)]);
    assert!(stdout.contains(&id(7)), "{stdout}");
}
