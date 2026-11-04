//! `kbx lint` resolves links from the files and `kbx graph` reads what
//! `kbx index` resolved. They must never disagree about where a link points.

use std::fs;
use std::path::Path;

use kbx_core::{EXAMPLE_ONTOLOGY, Graph, IndexOptions, Vault, index_vault, parse_note};
use tempfile::TempDir;

fn write(root: &Path, relative: &str, id: &str, extra: &str, body: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = format!("---\nid: {id}\ntype: Concept\ntitle: {relative}\n{extra}---\n{body}\n");
    fs::write(path, text).unwrap();
}

#[test]
fn every_link_lands_on_the_same_note_in_the_vault_and_the_index() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let files = [
        // `-` sorts before `/`, so the first file wins the id.
        ("a-b/dup.md", "1", "aliases: [winner]\n", ""),
        ("a/dup.md", "1", "aliases: [loser, shared]\n", ""),
        ("c.md", "2", "aliases: [shared]\n", ""),
        (
            "d.md",
            "3",
            "see_also: [\"[[1]]\", \"[[1-renamed]]\", \"[[winner]]\", \"[[loser]]\"]\n",
            "[[shared]] [[a/dup]] [[a-b/dup]] [[dup]] [[missing]]",
        ),
    ];
    for (path, id, extra, body) in files {
        write(&root, path, id, extra, body);
    }

    let mut graph = Graph::open(&root.join("graph.sqlite")).unwrap();
    let options = IndexOptions::default();
    index_vault(&mut graph, &root, EXAMPLE_ONTOLOGY, &options).unwrap();

    let notes = files
        .iter()
        .map(|(path, ..)| parse_note(*path, &fs::read_to_string(root.join(path)).unwrap()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let vault = Vault::new(notes);
    let linker = vault.notes().iter().find(|n| n.id == "3").unwrap();

    let mut statement = graph
        .connection()
        .prepare("SELECT dst FROM links WHERE src = '3' AND target = ?")
        .unwrap();
    let typed = linker.relations.iter().flat_map(|entry| &entry.links);
    let links: Vec<_> = typed.chain(linker.mentions()).collect();
    assert_eq!(links.len(), 9);
    for link in links {
        let stored: Option<String> = statement
            .query_row([&link.target], |row| row.get(0))
            .unwrap();
        let resolved = vault.resolve(link).map(|r| r.note.id.clone());
        assert_eq!(resolved, stored, "[[{}]]", link.target);
    }
}
