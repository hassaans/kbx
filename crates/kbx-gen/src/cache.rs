//! Remembers what a search found, so that a run over unchanged notes costs no
//! model time. See `docs/embedding-cache.md` for the measurements behind this.
//!
//! This is kbx's own file. Vectors stay in QMD's index, which kbx neither
//! opens nor writes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use kbx_core::Graph;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::search::{Hit, Query, Search, SearchError};

/// Bumped when a row's meaning changes. Rows of any other version are dropped.
const VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS results (scope TEXT NOT NULL, note TEXT NOT NULL, key TEXT NOT NULL,
    hits TEXT NOT NULL, PRIMARY KEY (scope, note));
CREATE TABLE IF NOT EXISTS refreshed (id INTEGER PRIMARY KEY CHECK (id = 1), state TEXT NOT NULL);
";

/// The file, which like every index can be deleted at any time.
#[derive(Debug)]
pub struct SearchCache {
    connection: Connection,
    source: String,
    writable: bool,
}

impl SearchCache {
    /// Opens the cache at `path` for results from `source`, whatever says what
    /// produced them: the models QMD is using, at the least. A file that can't
    /// be read is replaced.
    pub fn open(path: &Path, source: &str) -> rusqlite::Result<SearchCache> {
        let connection = prepare(path).or_else(|error| {
            warn!("{}: {error}; starting again", path.display());
            let _ = fs::remove_file(path);
            prepare(path)
        })?;
        Ok(SearchCache {
            connection,
            source: source.to_string(),
            writable: true,
        })
    }

    /// Opens the cache without changing it, if there's a usable one.
    pub fn open_read_only(path: &Path, source: &str) -> Option<SearchCache> {
        let connection =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let version = connection.query_row("PRAGMA user_version", [], |row| row.get(0));
        (version.ok() == Some(VERSION)).then(|| SearchCache {
            connection,
            source: source.to_string(),
            writable: false,
        })
    }

    /// Whether QMD was last brought up to date at `state`, a [`vault_state`].
    pub fn is_refreshed(&self, state: &str) -> bool {
        let stored = self
            .connection
            .query_row("SELECT state FROM refreshed", [], |row| {
                row.get::<_, String>(0)
            });
        stored.is_ok_and(|stored| stored == state)
    }

    /// Records that QMD is up to date at `state`.
    pub fn mark_refreshed(&self, state: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO refreshed (id, state) VALUES (1, ?)",
            [state],
        )?;
        Ok(())
    }

    /// `search`, on the QMD index `index` and the collections in `roots`, with
    /// each note's search remembered. Rows of notes no longer in a collection
    /// go now, so the file holds at most a row per note.
    pub fn around(
        self,
        search: Box<dyn Search>,
        g: &Graph,
        index: &str,
        roots: &BTreeMap<String, String>,
    ) -> rusqlite::Result<CachedSearch> {
        let connection = g.connection();
        let mut statement = connection.prepare("SELECT id, path FROM files")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut scopes = HashMap::new();
        for row in rows {
            let (id, path): (String, String) = row?;
            if let Some(scope) = scope_of(&path, roots) {
                scopes.insert(id, scope.clone());
            }
        }

        let mut statement = connection.prepare("SELECT key, text_hash FROM sections")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let sections = rows.collect::<rusqlite::Result<_>>()?;

        if self.writable {
            self.forget_all_but(&scopes)?;
        }
        let roots = roots.iter().map(|(name, root)| format!("{name}\0{root}"));
        let roots: Vec<String> = roots.collect();
        let searched = format!("{}\0{index}\0{}", self.source, roots.join("\0"));
        Ok(CachedSearch {
            search,
            cache: self,
            searched,
            scopes,
            sections,
        })
    }

    fn forget_all_but(&self, scopes: &HashMap<String, String>) -> rusqlite::Result<()> {
        let mut statement = self.connection.prepare("SELECT scope, note FROM results")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let stored = rows.collect::<rusqlite::Result<HashSet<(String, String)>>>()?;
        for (scope, note) in stored {
            if scopes.get(&note) != Some(&scope) {
                self.connection.execute(
                    "DELETE FROM results WHERE scope = ? AND note = ?",
                    [scope, note],
                )?;
            }
        }
        Ok(())
    }
}

fn prepare(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != VERSION {
        connection
            .execute_batch("DROP TABLE IF EXISTS results; DROP TABLE IF EXISTS refreshed;")?;
    }
    connection.execute_batch(SCHEMA)?;
    connection.pragma_update(None, "user_version", VERSION)?;
    Ok(connection)
}

/// The collection holding the note at `path`, from the vault root.
fn scope_of<'a>(path: &str, roots: &'a BTreeMap<String, String>) -> Option<&'a String> {
    let holds = |root: &str| root == "." || path.starts_with(&format!("{root}/"));
    let found = roots.iter().find(|(_, root)| holds(root));
    found.map(|(name, _)| name)
}

/// What the vault's notes are, with `reported`, whatever QMD says about its
/// index. If this hasn't changed, `qmd update` and `qmd embed` have nothing to
/// do. Saying what QMD holds catches an index lost or emptied behind kbx.
pub fn vault_state(g: &Graph, reported: &str) -> rusqlite::Result<String> {
    let connection = g.connection();
    let mut statement = connection.prepare("SELECT path, content_hash FROM files ORDER BY path")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut digest = Sha256::new();
    digest.update(reported);
    for row in rows {
        let (path, hash): (String, String) = row?;
        digest.update(format!("\0{path}\0{hash}"));
    }
    Ok(hex(&digest.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A hit as stored, with what its section said then.
#[derive(Serialize, Deserialize)]
struct Stored {
    note: String,
    section: String,
    score: f64,
    text_hash: String,
}

/// A [`Search`] that asks its backend only about what it hasn't seen.
///
/// A note's search is reused while the text of the note's sections, QMD's
/// models and the collections and their roots are what they were, and every section it hit
/// says what it said. So an edit repeats the edited note's search and those
/// that found it, and nothing else. What it can't see is an edited note that
/// has become a neighbour of a note it wasn't one of before; that shows up in
/// the edited note's own search, if the run gets to it.
pub struct CachedSearch {
    search: Box<dyn Search>,
    cache: SearchCache,
    /// The source, index, collections and roots, which every key includes.
    searched: String,
    /// Each note's collection.
    scopes: HashMap<String, String>,
    /// `text_hash` by section key.
    sections: HashMap<String, String>,
}

impl CachedSearch {
    fn key(&self, text: &str, limit: usize) -> String {
        let digest = Sha256::digest(format!("{}\0{limit}\0{text}", self.searched));
        hex(&digest)
    }

    fn remembered(&self, scope: &str, note: &str, key: &str) -> Option<Vec<Hit>> {
        let stored = self
            .cache
            .connection
            .query_row(
                "SELECT hits FROM results WHERE scope = ? AND note = ? AND key = ?",
                [scope, note, key],
                |row| row.get::<_, String>(0),
            )
            .optional();
        let stored = stored.unwrap_or_else(|error| {
            warn!("the search cache: {error}");
            None
        })?;
        let stored: Vec<Stored> = serde_json::from_str(&stored).ok()?;
        let current = |hit: &Stored| self.sections.get(&hit.section) == Some(&hit.text_hash);
        stored.iter().all(current).then(|| {
            let hit = |hit: Stored| Hit {
                note: hit.note,
                section: hit.section,
                score: hit.score,
            };
            stored.into_iter().map(hit).collect()
        })
    }

    /// A cache that can't be written costs the next run a search, not this one.
    fn remember(&self, scope: &str, note: &str, key: &str, hits: &[Hit]) {
        if !self.cache.writable {
            return;
        }
        let stored = hits.iter().filter_map(|hit| {
            Some(Stored {
                note: hit.note.clone(),
                section: hit.section.clone(),
                score: hit.score,
                text_hash: self.sections.get(&hit.section)?.clone(),
            })
        });
        let stored: Vec<Stored> = stored.collect();
        let written = serde_json::to_string(&stored).map_err(|e| e.to_string());
        let written = written.and_then(|hits| {
            self.cache
                .connection
                .execute(
                    "INSERT OR REPLACE INTO results (scope, note, key, hits) VALUES (?, ?, ?, ?)",
                    params![scope, note, key, hits],
                )
                .map_err(|e| e.to_string())
        });
        if let Err(error) = written {
            warn!("the search cache: {error}");
        }
    }
}

impl Search for CachedSearch {
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError> {
        let Query::Note { note, text } = query else {
            return self.search.search(query, limit);
        };
        let Some(scope) = self.scopes.get(*note) else {
            return self.search.search(query, limit);
        };
        let key = self.key(text, limit);
        if let Some(hits) = self.remembered(scope, note, &key) {
            return Ok(hits);
        }
        let hits = self.search.search(query, limit)?;
        self.remember(scope, note, &key, &hits);
        Ok(hits)
    }
}
