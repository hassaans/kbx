use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use kbx_core::Graph;
use serde::Deserialize;

/// What to search for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query<'a> {
    /// Free text.
    Text(&'a str),
    /// The neighbours of a section. An engine with its own vectors can look
    /// them up by `key`; `text` is for one that has to search by words.
    Section {
        /// The section's note.
        note: &'a str,
        /// The section's key.
        key: &'a str,
        /// The section's text.
        text: &'a str,
    },
}

/// A section that matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The note's id.
    pub note: String,
    /// The section's key.
    pub section: String,
    /// The backend's own score, higher is better.
    pub score: f64,
}

/// A search backend. [`QmdKnn`](crate::QmdKnn) depends on nothing else, so the
/// built-in engine can replace QMD here.
pub trait Search {
    /// The best `limit` hits, best first. Only notes in the index are hits.
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError>;
}

/// Why a search failed.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The backend could not be started.
    #[error("could not run {0}: {1}")]
    Spawn(PathBuf, io::Error),
    /// The backend reported a failure.
    #[error("{0}")]
    Failed(String),
    /// The backend's output could not be read.
    #[error("unexpected search output: {0}")]
    Output(String),
}

/// One result of `qmd query --json`. `line` is missing before qmd 2.
#[derive(Deserialize)]
struct Row {
    file: String,
    score: f64,
    line: Option<usize>,
    snippet: Option<String>,
}

/// Search by shelling out to QMD.
///
/// §5 says `--filter`, but no qmd release has that flag and qmd ignores flags
/// it doesn't know, so the search would not be limited at all. `-c` limits it
/// to the configured collections, and hits are kept only if the index knows
/// their file, so nothing outside the vault gets through.
#[derive(Debug, Clone)]
pub struct QmdSearch {
    bin: PathBuf,
    collections: Vec<String>,
    command: &'static str,
    /// Note ids by the [`handle`] of their path.
    notes: HashMap<String, String>,
    /// Each note's sections as `(start_line, end_line, key)`, in file order.
    sections: HashMap<String, Vec<(usize, usize, String)>>,
}

impl QmdSearch {
    /// Runs `bin`, limited to `collections`, or to qmd's default ones if empty.
    pub fn new(bin: PathBuf, collections: Vec<String>, g: &Graph) -> rusqlite::Result<QmdSearch> {
        let connection = g.connection();
        let mut statement = connection.prepare("SELECT path, id FROM files")?;
        let rows = statement.query_map([], |row| {
            Ok((handle(&row.get::<_, String>(0)?), row.get(1)?))
        })?;
        let notes = rows.collect::<rusqlite::Result<_>>()?;

        let mut statement = connection.prepare(
            "SELECT id, start_line, end_line, key FROM sections ORDER BY id, start_line",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        let mut sections: HashMap<String, Vec<_>> = HashMap::new();
        for row in rows {
            let (id, start, end, key): (String, i64, i64, String) = row?;
            let lines = (start as usize, end as usize, key);
            sections.entry(id).or_default().push(lines);
        }
        Ok(QmdSearch {
            bin,
            collections,
            command: "query",
            notes,
            sections,
        })
    }

    /// Uses `qmd search`, which is BM25 alone and needs no models.
    pub fn keyword_only(mut self) -> QmdSearch {
        self.command = "search";
        self
    }

    fn hit(&self, row: Row) -> Option<Hit> {
        let note = self.note_at(&row.file)?;
        let sections = self.sections.get(note)?;
        let line = row.line.or_else(|| snippet_line(row.snippet.as_deref()?));
        let at = |line| sections.iter().find(|(s, e, _)| (*s..=*e).contains(&line));
        let section = line.and_then(at).or(sections.first())?;
        Some(Hit {
            note: note.clone(),
            section: section.2.clone(),
            score: row.score,
        })
    }

    /// A collection may be rooted at a scope, which is how `kbx init` names
    /// them, so that its name is part of the vault path. Or it may be rooted
    /// at the vault, but only a configured one can be taken to be: the same
    /// path in another collection is another file.
    fn note_at(&self, file: &str) -> Option<&String> {
        let path = file.strip_prefix("qmd://").unwrap_or(file);
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        self.notes.get(&handle(path)).or_else(|| {
            let (collection, inside) = path.split_once('/')?;
            let configured = self.collections.iter().any(|c| c == collection);
            configured.then(|| self.notes.get(&handle(inside)))?
        })
    }
}

impl Search for QmdSearch {
    fn search(&self, query: &Query<'_>, limit: usize) -> Result<Vec<Hit>, SearchError> {
        let (Query::Text(text) | Query::Section { text, .. }) = query;
        // qmd reads a query of several lines as a typed query document.
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

        let mut command = Command::new(&self.bin);
        command.args([self.command, "--json", "-n", &limit.to_string()]);
        for collection in &self.collections {
            command.args(["-c", collection]);
        }
        let output = command
            .args(["--", &text])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| SearchError::Spawn(self.bin.clone(), error))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let failure = format!("qmd failed: {} {}", output.status, stderr.trim());
            return Err(SearchError::Failed(failure));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let rows: Vec<Row> = serde_json::from_str(json_array(&stdout))
            .map_err(|error| SearchError::Output(format!("{error}: {}", stdout.trim())))?;
        Ok(rows.into_iter().filter_map(|row| self.hit(row)).collect())
    }
}

/// The JSON in qmd's output, skipping any progress printed before it. Inside
/// the array everything is indented, so only its start is at a line's start.
fn json_array(stdout: &str) -> &str {
    if stdout.starts_with('[') {
        return stdout;
    }
    stdout.find("\n[").map_or(stdout, |at| &stdout[at + 1..])
}

/// The first line of a snippet headed `@@ -10,2 @@ …`.
fn snippet_line(snippet: &str) -> Option<usize> {
    let rest = snippet.strip_prefix("@@ -")?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// A path the way qmd stores it, which folds case and turns each run of other
/// characters into a dash, so that a note's path and its hit can be compared.
fn handle(path: &str) -> String {
    let lower = path.to_lowercase();
    let lower = lower.strip_suffix(".md").unwrap_or(&lower);
    let segments = lower.split('/').map(|segment| {
        let parts = segment.split(|c: char| !c.is_alphanumeric() && c != '$');
        parts
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    });
    let segments: Vec<_> = segments.filter(|segment| !segment.is_empty()).collect();
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::{handle, json_array, snippet_line};

    #[test]
    fn handle_folds_a_path_and_its_qmd_form_together() {
        assert_eq!(
            handle("Team X/Sub.Dir/01ABC-Token Buckets.md"),
            "team-x/sub-dir/01abc-token-buckets"
        );
        assert_eq!(
            handle("team-x/sub-dir/01abc-token-buckets.md"),
            "team-x/sub-dir/01abc-token-buckets"
        );
    }

    #[test]
    fn json_array_skips_what_comes_before_it() {
        assert_eq!(json_array("[]\n"), "[]\n");
        assert_eq!(
            json_array("\u{1b}[?25lloading\n[\n  {}\n]\n"),
            "[\n  {}\n]\n"
        );
    }

    #[test]
    fn snippet_line_reads_the_header() {
        assert_eq!(
            snippet_line("@@ -12,4 @@ (2 before, 0 after)\ntext"),
            Some(12)
        );
        assert_eq!(snippet_line("no header"), None);
    }
}
