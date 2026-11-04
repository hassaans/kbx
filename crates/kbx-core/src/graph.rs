use std::path::Path;

use rusqlite::Connection;

/// The §4 schema, plus two tables it has no room for:
///
/// - `blocks` holds every `^block-id` of a note, so `{id}#^{block-id}` is one
///   lookup. `sections.block_id` keeps only the id on the heading itself.
/// - `links` holds every link as written, with `dst` NULL while it is hidden
///   (§8). `edges` is derived from it, so a link in an unchanged file can
///   resolve, or stop resolving, when another file changes.
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
    origin TEXT CHECK(origin IN ('frontmatter','inline','derived')),
    src_section TEXT, line INTEGER, PRIMARY KEY(src, dst, rel, origin));
CREATE INDEX IF NOT EXISTS edges_dst ON edges(dst);
CREATE INDEX IF NOT EXISTS aliases_id ON aliases(id);
CREATE INDEX IF NOT EXISTS sections_id ON sections(id);
CREATE INDEX IF NOT EXISTS links_src ON links(src);
CREATE TABLE IF NOT EXISTS candidates (cid TEXT PRIMARY KEY, src TEXT, dst TEXT, generators TEXT,
    features TEXT, score REAL, created_at INTEGER);
CREATE TABLE IF NOT EXISTS verdicts (cid TEXT, backend TEXT, model TEXT, prompt_hash TEXT, verdict TEXT,
    rel TEXT, confidence REAL, rationale TEXT, cost_usd REAL, cached_tokens INTEGER,
    created_at INTEGER, PRIMARY KEY(cid, backend, model, prompt_hash));
CREATE TABLE IF NOT EXISTS reviews (src TEXT, dst TEXT, rel TEXT,
    decision TEXT CHECK(decision IN ('accept','reject','defer')),
    reason TEXT, strikes INTEGER DEFAULT 0, decided_at INTEGER, PRIMARY KEY(src, dst, rel));
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
";

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
        Ok(Graph { conn })
    }

    /// The connection, for queries this crate has no method for yet.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}
