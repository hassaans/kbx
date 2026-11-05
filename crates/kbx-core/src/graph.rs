use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};

use crate::path::without_verbatim_prefix;

/// The §4 schema, plus two tables it has no room for:
///
/// - `blocks` holds every `^block-id` of a note, so `{id}#^{block-id}` is one
///   lookup. `sections.block_id` keeps only the id on the heading itself.
/// - `links` holds every link as written, with `dst` NULL while it is hidden
///   (§8). `edges` is derived from it, so a link in an unchanged file can
///   resolve, or stop resolving, when another file changes.
///
/// `candidates` also has `src_hash` and `dst_hash`: each note's `content_hash`
/// when the pair was generated, which `kbx accept` compares with the file. A
/// later `kbx index` moves `files.content_hash`, so that can't serve. Its
/// `src_section` and `dst_section` hold the stable section key at each end of a
/// section-level pair, and are NULL on a note-level one (`docs/sections.md`).
///
/// `staged` is the ledger of `kbx.proposed`: what kbx put in which note, which
/// is how `kbx sync-stage` can tell a line a person removed from one never
/// written. `hash` is the note's `content_hash` after the write, and `content`
/// the same without the tool namespaces.
///
/// `locks` holds each `kbx.reviewed`: `baseline` is the note's content when the
/// date was first seen and `current` its content now, so equal means locked.
/// The frontmatter has room for a date alone (§3). `held` is 1 on a lock kept
/// here alone, with no `kbx.reviewed` in the note, which the index leaves in place.
///
/// `accepted` holds the relations accepted but in no note: the one thing in
/// `.kbx/` that no file can give back, so a rebuild keeps it. `src`, `dst` and
/// `rel` read as the frontmatter would, and each row becomes an `edges` row of
/// origin `kbx`. It goes when the note states the relation (`docs/review.md`).
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, id TEXT UNIQUE, type TEXT, title TEXT,
    content_hash TEXT, fm_hash TEXT, mtime INTEGER, scope TEXT, indexed_at INTEGER);
CREATE TABLE IF NOT EXISTS aliases (alias TEXT, id TEXT, PRIMARY KEY(alias, id));
CREATE TABLE IF NOT EXISTS sections (key TEXT PRIMARY KEY, id TEXT, heading_path TEXT, block_id TEXT,
    start_line INTEGER, end_line INTEGER, text_hash TEXT, tokens INTEGER);
CREATE TABLE IF NOT EXISTS blocks (id TEXT, block_id TEXT, section_key TEXT,
    start_line INTEGER, end_line INTEGER, PRIMARY KEY(id, block_id));
CREATE TABLE IF NOT EXISTS links (src TEXT, key TEXT, target TEXT, fragment TEXT,
    origin TEXT CHECK(origin IN ('frontmatter','inline')), src_section TEXT, line INTEGER, dst TEXT);
CREATE TABLE IF NOT EXISTS edges (src TEXT, dst TEXT, rel TEXT, inverse_of TEXT, weight REAL,
    origin TEXT CHECK(origin IN ('frontmatter','inline','derived','kbx')),
    src_section TEXT, line INTEGER, PRIMARY KEY(src, dst, rel, origin));
CREATE INDEX IF NOT EXISTS edges_dst ON edges(dst);
CREATE INDEX IF NOT EXISTS aliases_id ON aliases(id);
CREATE INDEX IF NOT EXISTS sections_id ON sections(id);
CREATE INDEX IF NOT EXISTS links_src ON links(src);
CREATE TABLE IF NOT EXISTS candidates (cid TEXT PRIMARY KEY, src TEXT, dst TEXT, generators TEXT,
    features TEXT, score REAL, created_at INTEGER, src_hash TEXT, dst_hash TEXT,
    src_section TEXT, dst_section TEXT);
CREATE TABLE IF NOT EXISTS verdicts (cid TEXT, backend TEXT, model TEXT, prompt_hash TEXT, verdict TEXT,
    rel TEXT, confidence REAL, rationale TEXT, cost_usd REAL, cached_tokens INTEGER,
    created_at INTEGER, PRIMARY KEY(cid, backend, model, prompt_hash));
CREATE TABLE IF NOT EXISTS reviews (src TEXT, dst TEXT, rel TEXT,
    decision TEXT CHECK(decision IN ('accept','reject','defer')),
    reason TEXT, strikes INTEGER DEFAULT 0, decided_at INTEGER, PRIMARY KEY(src, dst, rel));
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS staged (cid TEXT PRIMARY KEY, note TEXT, target TEXT, rel TEXT,
    hash TEXT, content TEXT);
CREATE TABLE IF NOT EXISTS locks (id TEXT PRIMARY KEY, reviewed TEXT, baseline TEXT, current TEXT,
    held INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS accepted (cid TEXT, src TEXT, dst TEXT, rel TEXT, src_section TEXT,
    dst_section TEXT, decided_at INTEGER, PRIMARY KEY(src, dst, rel));
CREATE INDEX IF NOT EXISTS accepted_cid ON accepted(cid);
";

/// A note in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The note's id.
    pub id: String,
    /// The note's class, which the ontology may not know.
    pub class: String,
    /// The note's title.
    pub title: String,
}

/// One edge seen from a note: where it goes and what kind it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbor {
    /// The note at the other end.
    pub node: Node,
    /// The edge's `rel`: a frontmatter key, or [`MENTIONS`](crate::MENTIONS).
    pub rel: String,
    /// The edge's `origin`: `frontmatter`, `inline`, `derived` or `kbx`.
    pub origin: String,
    /// The relation's weight. Mentions and keys outside the ontology have none.
    pub weight: Option<f64>,
    /// The edge points at the note it's seen from, not away from it.
    pub incoming: bool,
}

/// What the index holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphStats {
    /// Rows in `files`.
    pub nodes: i64,
    /// Rows in `edges`, derived inverses included.
    pub edges: i64,
    /// Of those, the accepted relations no note states.
    pub accepted: i64,
    /// Rows in `sections`.
    pub sections: i64,
    /// Links that resolve to nothing (§8).
    pub hidden: i64,
}

/// The graph database, `.kbx/graph.sqlite`.
#[derive(Debug)]
pub struct Graph {
    pub(crate) conn: Connection,
}

impl Graph {
    /// Opens the database in WAL mode, creating it and its tables if needed.
    pub fn open(path: &Path) -> rusqlite::Result<Graph> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        add_candidate_columns(&conn)?;
        add_kbx_origin(&conn)?;
        add_held_column(&conn)?;
        Ok(Graph { conn })
    }

    /// Opens an existing database for reading, changing nothing on disk.
    ///
    /// A plain read-only connection to a WAL database leaves `-wal` and `-shm`
    /// files behind, so without a WAL to read the file is opened as immutable,
    /// which creates neither. With one, a writer may have rows there that the
    /// main file lacks, and both sidecars are already in place.
    pub fn open_read_only(path: &Path) -> rusqlite::Result<Graph> {
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        let live = fs::metadata(PathBuf::from(wal)).is_ok_and(|file| file.len() > 0);
        let query = if live { "mode=ro" } else { "immutable=1" };
        let uri = format!("{}?{query}", file_uri(path));
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(uri, flags)?;
        // Opening is lazy: fail here, not at the first query, if it's no database.
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |_| Ok(()))?;
        Ok(Graph { conn })
    }

    /// The connection, for queries this crate has no method for yet.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Copies the database to a new file and opens the copy, leaving this one
    /// as it is, read-only or not.
    pub fn copy_to(&self, path: &Path) -> rusqlite::Result<Graph> {
        self.conn
            .execute("VACUUM INTO ?", [path.to_string_lossy()])?;
        Graph::open(path)
    }

    /// The note with this id.
    pub fn node(&self, id: &str) -> rusqlite::Result<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, type, title FROM files WHERE id = ?",
                [id],
                |row| node(row, 0),
            )
            .optional()
    }

    /// Every edge at a note, outgoing first, then by `rel` and id. A relation
    /// has a mirror row from each end, so only edges without one, which are
    /// the ones with no weight, are also listed from the end they point at.
    pub fn neighbors(&self, id: &str) -> rusqlite::Result<Vec<Neighbor>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT f.id, f.type, f.title, e.rel, e.origin, e.weight, 0 AS incoming
             FROM edges e JOIN files f ON f.id = e.dst WHERE e.src = ?1
             UNION ALL
             SELECT f.id, f.type, f.title, e.rel, e.origin, e.weight, 1 AS incoming
             FROM edges e JOIN files f ON f.id = e.src
             WHERE e.dst = ?1 AND e.src <> ?1 AND e.weight IS NULL
             ORDER BY incoming, rel, id",
        )?;
        let rows = statement.query_map([id], |row| {
            Ok(Neighbor {
                node: node(row, 0)?,
                rel: row.get(3)?,
                origin: row.get(4)?,
                weight: row.get(5)?,
                incoming: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// A shortest path between two notes, as the hops after `from`. Empty if
    /// they are the same note, `None` if no path joins them.
    pub fn path(&self, from: &str, to: &str) -> rusqlite::Result<Option<Vec<Neighbor>>> {
        let mut seen = HashSet::from([from.to_string()]);
        let mut queue = VecDeque::from([from.to_string()]);
        let mut reached_by: HashMap<String, (String, Neighbor)> = HashMap::new();
        while let Some(id) = queue.pop_front() {
            if id == to {
                let mut hops = Vec::new();
                let mut at = id;
                while let Some((previous, hop)) = reached_by.remove(&at) {
                    hops.push(hop);
                    at = previous;
                }
                hops.reverse();
                return Ok(Some(hops));
            }
            for neighbor in self.neighbors(&id)? {
                let next = neighbor.node.id.clone();
                if seen.insert(next.clone()) {
                    reached_by.insert(next.clone(), (id.clone(), neighbor));
                    queue.push_back(next);
                }
            }
        }
        Ok(None)
    }

    /// The classes notes have, with how many of each, commonest first.
    pub fn class_counts(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut statement = self.conn.prepare(
            "SELECT type, COUNT(*) AS notes FROM files GROUP BY type ORDER BY notes DESC, type",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Counts what the index holds.
    pub fn stats(&self) -> rusqlite::Result<GraphStats> {
        let count = |sql: &str| self.conn.query_row(sql, [], |row| row.get(0));
        Ok(GraphStats {
            nodes: count("SELECT COUNT(*) FROM files")?,
            edges: count("SELECT COUNT(*) FROM edges")?,
            accepted: count("SELECT COUNT(*) FROM edges WHERE origin = 'kbx'")?,
            sections: count("SELECT COUNT(*) FROM sections")?,
            hidden: count("SELECT COUNT(*) FROM links WHERE dst IS NULL")?,
        })
    }
}

/// Upgrades a database from before these columns. Its candidates keep NULL
/// there: without hashes accepting one asks for a new `kbx suggest`, without
/// sections it's a note-level pair, and without `accepted_rel` it's undecided.
fn add_candidate_columns(conn: &Connection) -> rusqlite::Result<()> {
    let columns = candidate_columns(conn)?;
    let added = [
        "src_hash",
        "dst_hash",
        "src_section",
        "dst_section",
        "accepted_rel",
    ];
    for column in added {
        if !columns.contains(column) {
            conn.execute_batch(&format!("ALTER TABLE candidates ADD COLUMN {column} TEXT"))?;
        }
    }
    Ok(())
}

/// Upgrades `edges` from before origin `kbx`. SQLite can't alter a `CHECK`, so
/// the table is copied.
fn add_kbx_origin(conn: &Connection) -> rusqlite::Result<()> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'edges'",
        [],
        |row| row.get(0),
    )?;
    if sql.contains("'kbx'") {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE edges_new (src TEXT, dst TEXT, rel TEXT, inverse_of TEXT, weight REAL,
             origin TEXT CHECK(origin IN ('frontmatter','inline','derived','kbx')),
             src_section TEXT, line INTEGER, PRIMARY KEY(src, dst, rel, origin));
         INSERT INTO edges_new SELECT src, dst, rel, inverse_of, weight, origin, src_section, line
             FROM edges;
         DROP TABLE edges;
         ALTER TABLE edges_new RENAME TO edges;
         CREATE INDEX edges_dst ON edges(dst);
         COMMIT;",
    )
}

/// Upgrades `locks` from before `held`: every lock until then is in a note.
fn add_held_column(conn: &Connection) -> rusqlite::Result<()> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info('locks')")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let columns = rows.collect::<rusqlite::Result<HashSet<String>>>()?;
    if !columns.contains("held") {
        conn.execute_batch("ALTER TABLE locks ADD COLUMN held INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

fn candidate_columns(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info('candidates')")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Whether `candidates` has the section columns. A database opened read-only
/// can't be upgraded, so one from an older kbx may not.
pub(crate) fn has_candidate_sections(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(candidate_columns(conn)?.contains("src_section"))
}

/// Whether there is an `accepted` table, likewise.
pub(crate) fn has_accepted_table(conn: &Connection) -> rusqlite::Result<bool> {
    let tables = "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'accepted'";
    Ok(conn.query_row(tables, [], |row| row.get::<_, i64>(0))? > 0)
}

/// Whether `candidates` has `accepted_rel`, likewise.
pub(crate) fn has_accepted_rel(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(candidate_columns(conn)?.contains("accepted_rel"))
}

/// `path` as a `file:` URI, which SQLite needs for its query parameters.
fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy();
    if cfg!(windows) {
        uri_of(&forward_slashes(&path))
    } else {
        uri_of(&path)
    }
}

/// A Windows path with `/` separators and without the `\\?\` prefix that
/// `canonicalize` adds, which means nothing in a URI.
fn forward_slashes(path: &str) -> String {
    without_verbatim_prefix(path).replace('\\', "/")
}

fn uri_of(path: &str) -> String {
    let path = path
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    let bytes = path.as_bytes();
    if path.starts_with("//") {
        // A network share. What follows `file://` would be the authority.
        format!("file://{path}")
    } else if bytes.len() > 1 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        format!("file:/{path}")
    } else {
        format!("file:{path}")
    }
}

/// Reads a [`Node`] from three columns, starting at `first`.
fn node(row: &Row<'_>, first: usize) -> rusqlite::Result<Node> {
    Ok(Node {
        id: row.get(first)?,
        class: row.get(first + 1)?,
        title: row.get(first + 2)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{forward_slashes, uri_of};

    #[test]
    fn windows_paths_lose_the_verbatim_prefix() {
        let uri = |path| uri_of(&forward_slashes(path));
        assert_eq!(uri(r"\\?\C:\a b\g.sqlite"), "file:/C:/a b/g.sqlite");
        assert_eq!(uri(r"C:\a\g.sqlite"), "file:/C:/a/g.sqlite");
        assert_eq!(
            uri(r"\\?\UNC\host\share\g.sqlite"),
            "file:////host/share/g.sqlite"
        );
    }

    #[test]
    fn unix_paths_keep_their_form_and_escape_uri_characters() {
        assert_eq!(uri_of("/a/g.sqlite"), "file:/a/g.sqlite");
        assert_eq!(uri_of(".kbx/g.sqlite"), "file:.kbx/g.sqlite");
        assert_eq!(uri_of("/a#b?c%d"), "file:/a%23b%3fc%25d");
    }
}
