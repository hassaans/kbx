//! `Graph::open_read_only` reads the database without changing anything beside it.

use std::fs;
use std::path::Path;

use kbx_core::Graph;
use rusqlite::Connection;
use tempfile::TempDir;

fn sidecars(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn seeded(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("graph.sqlite");
    let graph = Graph::open(&path).unwrap();
    graph
        .connection()
        .execute("INSERT INTO meta (k, v) VALUES ('a', '1')", [])
        .unwrap();
    drop(graph);
    path
}

fn count(graph: &Graph) -> i64 {
    graph
        .connection()
        .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn reads_and_leaves_no_sidecar_files() {
    let dir = TempDir::new().unwrap();
    let path = seeded(dir.path());
    let before = fs::read(&path).unwrap();

    let graph = Graph::open_read_only(&path).unwrap();
    assert_eq!(count(&graph), 1);
    assert_eq!(sidecars(dir.path()), ["graph.sqlite"], "while it's open");
    drop(graph);

    assert_eq!(sidecars(dir.path()), ["graph.sqlite"]);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn refuses_to_write() {
    let dir = TempDir::new().unwrap();
    let graph = Graph::open_read_only(&seeded(dir.path())).unwrap();
    let error = graph
        .connection()
        .execute("INSERT INTO meta (k, v) VALUES ('b', '2')", []);
    assert!(error.is_err());
}

#[test]
fn sees_what_a_live_writer_has_committed_to_the_wal() {
    let dir = TempDir::new().unwrap();
    let path = seeded(dir.path());
    let writer = Connection::open(&path).unwrap();
    writer
        .execute("INSERT INTO meta (k, v) VALUES ('b', '2')", [])
        .unwrap();

    assert_eq!(count(&Graph::open_read_only(&path).unwrap()), 2);
}

#[test]
fn handles_a_path_with_uri_characters() {
    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("a b#c?d%20e");
    fs::create_dir_all(&folder).unwrap();
    let path = seeded(&folder);
    assert_eq!(count(&Graph::open_read_only(&path).unwrap()), 1);
}

#[test]
fn does_not_create_a_missing_database() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.sqlite");
    assert!(Graph::open_read_only(&path).is_err());
    assert!(!path.exists());
}
