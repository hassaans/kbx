//! Drives a running `kbx watch` over copies of the fixture vault.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{Fixture, id};
use rusqlite::Connection;

/// How long anything may take on a loaded CI runner. Waits end as soon as
/// what they wait for happens, so this only bounds a failing test.
const PATIENCE: Duration = Duration::from_secs(60);

fn note(n: u32, class: &str, body: &str) -> String {
    format!(
        "---\nid: {}\ntype: {class}\ntitle: Note {n}\n---\n{body}\n",
        id(n)
    )
}

fn indexed() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault
}

fn db(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

/// Records every later write to `files` in `audit`. The triggers live in the
/// file, so they see what the kbx process does.
fn audit(vault: &Fixture) {
    let db = db(vault);
    db.execute_batch("CREATE TABLE audit (op TEXT, path TEXT)")
        .unwrap();
    for op in ["INSERT", "UPDATE"] {
        db.execute_batch(&format!(
            "CREATE TRIGGER audit_{op} AFTER {op} ON files BEGIN
                 INSERT INTO audit VALUES ('{op}', NEW.path);
             END"
        ))
        .unwrap();
    }
}

/// How many times each path was written, for paths written at all.
fn writes(vault: &Fixture) -> Vec<(String, i64)> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT path, COUNT(*) FROM audit GROUP BY path ORDER BY path")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// What `writes` holds after a burst to the busy note and `Watch::settle`.
fn once_each() -> [(String, i64); 2] {
    [
        ("notes/busy.md".to_string(), 1),
        ("notes/settled.md".to_string(), 1),
    ]
}

fn is_indexed(vault: &Fixture, n: u32) -> bool {
    let count: i64 = db(vault)
        .query_row("SELECT COUNT(*) FROM files WHERE id = ?", [id(n)], |row| {
            row.get(0)
        })
        .unwrap();
    count == 1
}

/// Saves note 100 under a title that tells this save from the others.
fn save(vault: &Fixture, n: u32) {
    vault.write(
        "notes/busy.md",
        &format!(
            "---\nid: {}\ntype: Concept\ntitle: Save {n}\n---\nFine.\n",
            id(100)
        ),
    );
}

fn saved_title(vault: &Fixture) -> String {
    db(vault)
        .query_row("SELECT title FROM files WHERE id = ?", [id(100)], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Saves to note 100 that keep track of whether they stayed a burst. The watch
/// asks the files when the saves stopped, and the operating system dates those,
/// so no clock a test could hand it reaches this far. A runner can still stall
/// a test for a whole window between two saves. The saves have then really
/// stopped and indexing is right, so each stall may cost one more write and
/// anything else fails. The unit tests in `src/watch.rs` have the exact rule.
struct Burst {
    window: Duration,
    began: Option<Instant>,
    stalls: i64,
}

impl Burst {
    fn new(window: Duration) -> Burst {
        Burst {
            window,
            began: None,
            stalls: 0,
        }
    }

    fn save(&mut self, vault: &Fixture, n: u32) {
        let began = Instant::now();
        save(vault, n);
        // From before one write to after the next, so it can't undercount.
        if self.began.is_some_and(|at| at.elapsed() >= self.window) {
            self.stalls += 1;
        }
        self.began = Some(began);
    }

    /// `writes` is `once_each`, but for what the stalls excuse.
    fn assert_indexed_once(&self, vault: &Fixture) {
        let mut written = writes(vault);
        if let Some((_, count)) = written.first_mut() {
            *count -= self.stalls.min(*count - 1);
        }
        assert_eq!(written, once_each(), "{} stalls", self.stalls);
    }
}

/// A running `kbx watch`, killed when dropped.
struct Watch {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Watch {
    /// Starts it and waits until it's watching.
    fn start(vault: &Fixture, args: &[&str]) -> Watch {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kbx"))
            .arg("watch")
            .args(args)
            .current_dir(vault.root())
            .env("PATH", vault.path.dir())
            .env_remove("KBX_LOG")
            .stdout(Stdio::piped())
            .spawn()
            .expect("kbx should run");
        let stdout = child.stdout.take().unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                sink.lock().unwrap().push(line.unwrap());
            }
        });
        let watch = Watch { child, lines };
        watch.wait_for("the watch to start", |lines| {
            lines.iter().any(|line| line.starts_with("watching "))
        });
        watch
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn wait_for(&self, what: &str, done: impl Fn(&[String]) -> bool) {
        self.wait_until(what, || done(&self.lines()));
    }

    fn wait_until(&self, what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < PATIENCE,
                "timed out waiting for {what}; output:\n{}",
                self.lines().join("\n")
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Returns once everything saved so far has been handled. Events arrive in
    /// order and a batch commits whole, so nothing older is still on its way
    /// once a note saved now is in the index.
    fn settle(&self, vault: &Fixture) {
        vault.write("notes/settled.md", &note(101, "Concept", "Fine."));
        self.wait_until("the last note", || is_indexed(vault, 101));
    }

    /// The sum of `indexed N` over every report printed so far.
    fn indexed(&self) -> usize {
        self.count("indexed ")
    }

    fn removed(&self) -> usize {
        let reports = self.lines();
        let counts = reports.iter().filter_map(|line| {
            let (_, rest) = line.split_once(", removed ")?;
            rest.split(',').next()?.parse::<usize>().ok()
        });
        counts.sum()
    }

    fn count(&self, prefix: &str) -> usize {
        let reports = self.lines();
        let counts = reports.iter().filter_map(|line| {
            let rest = line.strip_prefix(prefix)?;
            rest.split(',').next()?.parse::<usize>().ok()
        });
        counts.sum()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn saving_a_note_reindexes_it_within_the_debounce_window_and_prints_findings() {
    let vault = indexed();
    let watch = Watch::start(&vault, &["--debounce-ms", "1000", "--reconcile-secs", "0"]);

    let saved = Instant::now();
    vault.write("notes/saved.md", &note(100, "Recipe", "Not a class."));
    watch.wait_for("the lint finding", |lines| {
        lines
            .iter()
            .any(|line| line.contains("notes/saved.md") && line.contains("type.unknown"))
    });
    let took = saved.elapsed();

    assert!(is_indexed(&vault, 100));
    assert_eq!(watch.indexed(), 1, "{:?}", watch.lines());
    // The window, its tick and room for a slow runner to schedule the process.
    assert!(took < Duration::from_secs(10), "took {took:?}");
}

#[test]
fn a_note_that_passes_lint_says_so() {
    let vault = indexed();
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "0"]);

    vault.write("notes/clean.md", &note(100, "Concept", "Fine."));
    watch.wait_for("the lint result", |lines| {
        lines.iter().any(|line| line == "no findings")
    });
}

#[test]
fn a_burst_of_1000_changes_indexes_each_file_exactly_once() {
    let vault = indexed();
    audit(&vault);
    // Reconciliation stays on: a runner that overflows its event buffer still
    // has to reach every file, and reconciling must not index one twice.
    let watch = Watch::start(&vault, &["--debounce-ms", "1000", "--reconcile-secs", "5"]);

    for n in 0..1000 {
        vault.write(
            &format!("burst/{n:04}.md"),
            &note(1000 + n, "Concept", "Fine."),
        );
    }
    watch.wait_for("all 1000 notes", |_| watch.indexed() >= 1000);
    // Anything indexed twice would show up within a window and a reconcile.
    thread::sleep(Duration::from_secs(7));

    let writes = writes(&vault);
    assert_eq!(writes.len(), 1000);
    let repeats: Vec<_> = writes.iter().filter(|(_, count)| *count != 1).collect();
    assert_eq!(repeats, Vec::<&(String, i64)>::new());
    assert_eq!(watch.indexed(), 1000, "{:?}", watch.lines());
}

#[test]
fn a_burst_of_saves_to_one_note_indexes_it_once() {
    let vault = indexed();
    audit(&vault);
    let watch = Watch::start(&vault, &["--debounce-ms", "3000", "--reconcile-secs", "0"]);

    let mut burst = Burst::new(Duration::from_secs(3));
    for n in 0..1000 {
        burst.save(&vault, n);
    }
    watch.settle(&vault);

    burst.assert_indexed_once(&vault);
    assert_eq!(saved_title(&vault), "Save 999");
}

#[test]
fn saves_that_outlast_the_window_index_the_note_once() {
    let vault = indexed();
    audit(&vault);
    let watch = Watch::start(&vault, &["--debounce-ms", "2000", "--reconcile-secs", "0"]);

    // However fast the runner, this burst is longer than the window. The sleep
    // only spaces the saves out: `Burst` notices if a gap wasn't inside the window.
    let mut burst = Burst::new(Duration::from_secs(2));
    let start = Instant::now();
    let mut last = 0;
    while start.elapsed() < Duration::from_secs(5) {
        last += 1;
        burst.save(&vault, last);
        thread::sleep(Duration::from_millis(50));
    }
    watch.settle(&vault);

    burst.assert_indexed_once(&vault);
    assert_eq!(saved_title(&vault), format!("Save {last}"));
}

#[test]
fn reconciliation_finds_a_change_when_events_never_arrive() {
    let vault = indexed();
    // No event survives an hour of debouncing, so only the fallback can act.
    let watch = Watch::start(
        &vault,
        &["--debounce-ms", "3600000", "--reconcile-secs", "1"],
    );

    vault.write("notes/quiet.md", &note(100, "Recipe", "Not a class."));
    watch.wait_for("the lint finding", |lines| {
        lines
            .iter()
            .any(|line| line.contains("notes/quiet.md") && line.contains("type.unknown"))
    });
    assert!(is_indexed(&vault, 100));
}

#[test]
fn a_new_default_type_reaches_a_running_watch() {
    let vault = indexed();
    vault.write("notes/plain.md", "No frontmatter here.\n");
    let watch = Watch::start(&vault, &["--debounce-ms", "50"]);
    let class = || -> Option<String> {
        let path = "notes/plain.md";
        let sql = "SELECT type FROM files WHERE path = ?";
        db(&vault).query_row(sql, [path], |row| row.get(0)).ok()
    };
    watch.wait_until("the note", || class().as_deref() == Some("Note"));

    let config = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, text.replace("= \"Note\"", "= \"Concept\"")).unwrap();
    watch.wait_until("the new class", || class().as_deref() == Some("Concept"));

    // The rebuild this leaves behind must not bring the old class back.
    vault.ok(&["index"]);
    watch.settle(&vault);
    assert_eq!(class().as_deref(), Some("Concept"));
}

#[test]
fn reconciliation_stays_quiet_when_nothing_changed() {
    let vault = indexed();
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "1"]);

    thread::sleep(Duration::from_secs(3));

    assert_eq!(watch.lines().len(), 1, "{:?}", watch.lines());
}

#[test]
fn notes_saved_while_not_watching_are_caught_at_start() {
    let vault = indexed();
    vault.write("notes/offline.md", &note(100, "Concept", "Fine."));

    let watch = Watch::start(&vault, &["--reconcile-secs", "0"]);

    assert_eq!(watch.indexed(), 1);
    assert!(is_indexed(&vault, 100));
}

#[test]
fn a_deleted_note_leaves_the_index() {
    let vault = indexed();
    vault.write("notes/doomed.md", &note(100, "Concept", "Fine."));
    vault.ok(&["index"]);
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "0"]);

    fs::remove_file(vault.root().join("notes/doomed.md")).unwrap();

    watch.wait_for("the removal", |_| watch.removed() == 1);
    assert!(!is_indexed(&vault, 100));
}

#[test]
fn a_note_that_comes_and_goes_within_the_window_does_not_stop_the_batch() {
    let vault = indexed();
    let watch = Watch::start(&vault, &["--debounce-ms", "500", "--reconcile-secs", "0"]);

    vault.write("notes/fleeting.md", &note(100, "Concept", "Fine."));
    fs::remove_file(vault.root().join("notes/fleeting.md")).unwrap();
    vault.write("notes/kept.md", &note(101, "Concept", "Fine."));

    watch.wait_until("the kept note", || is_indexed(&vault, 101));
    assert!(!is_indexed(&vault, 100));
}

#[test]
fn renaming_a_folder_moves_the_notes_inside_it() {
    let vault = indexed();
    vault.write("before/moved.md", &note(100, "Concept", "Fine."));
    vault.ok(&["index"]);
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "0"]);

    fs::rename(vault.root().join("before"), vault.root().join("after")).unwrap();

    watch.wait_until("the new path", || {
        let path: String = db(&vault)
            .query_row("SELECT path FROM files WHERE id = ?", [id(100)], |row| {
                row.get(0)
            })
            .unwrap_or_default();
        path == "after/moved.md"
    });
}

#[test]
fn files_that_are_not_notes_are_ignored() {
    let vault = indexed();
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "0"]);

    vault.write("notes/image.png", "not a note");
    vault.write(".obsidian/workspace.md", &note(100, "Recipe", "Hidden."));
    vault.write("notes/after.md", &note(101, "Concept", "Fine."));

    watch.wait_until("the note", || is_indexed(&vault, 101));
    assert!(!is_indexed(&vault, 100));
    assert_eq!(watch.indexed(), 1, "{:?}", watch.lines());
}

#[test]
fn a_broken_ontology_does_not_stop_the_watch() {
    let vault = indexed();
    let ontology = vault.root().join(".kbx/ontology.yaml");
    let good = fs::read_to_string(&ontology).unwrap();
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "0"]);

    fs::write(&ontology, "classes: [not, a, map").unwrap();
    vault.write("notes/first.md", &note(100, "Concept", "Fine."));
    thread::sleep(Duration::from_secs(1));
    assert!(!is_indexed(&vault, 100));

    fs::write(&ontology, good).unwrap();
    vault.write("notes/second.md", &note(101, "Concept", "Fine."));
    watch.wait_until("the second note", || is_indexed(&vault, 101));
    // Its batch failed and nothing will save it again, so the retry has to.
    watch.wait_until("the first note", || is_indexed(&vault, 100));
}

#[test]
fn suggest_runs_once_after_the_saves_stop() {
    let vault = indexed();
    let watch = Watch::start(
        &vault,
        &[
            "--suggest",
            "--suggest-debounce-secs",
            "2",
            "--debounce-ms",
            "200",
            "--reconcile-secs",
            "0",
        ],
    );
    let suggested = |lines: &[String]| {
        lines
            .iter()
            .filter(|line| line.ends_with(" candidates"))
            .count()
    };

    for n in 0..3 {
        vault.write(
            "notes/draft.md",
            &note(100, "Concept", &format!("Save {n}.")),
        );
        watch.wait_until("the save", || watch.indexed() == n + 1);
        assert_eq!(suggested(&watch.lines()), 0, "{:?}", watch.lines());
    }
    watch.wait_for("suggest", |lines| suggested(lines) == 1);
    thread::sleep(Duration::from_secs(3));

    assert_eq!(suggested(&watch.lines()), 1, "{:?}", watch.lines());
    let stored: i64 = db(&vault)
        .query_row("SELECT COUNT(*) FROM candidates", [], |row| row.get(0))
        .unwrap();
    assert!(stored > 0);
}

#[test]
fn without_the_flag_suggest_never_runs() {
    let vault = indexed();
    let watch = Watch::start(&vault, &["--debounce-ms", "200", "--reconcile-secs", "0"]);

    vault.write("notes/draft.md", &note(100, "Concept", "Fine."));
    watch.wait_until("the note", || is_indexed(&vault, 100));
    thread::sleep(Duration::from_secs(1));

    let stored: i64 = db(&vault)
        .query_row("SELECT COUNT(*) FROM candidates", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, 0);
}

#[test]
fn watch_needs_a_project() {
    let dir = tempfile::TempDir::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
        .arg("watch")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(common::stderr(&output).contains("kbx init"));
}

#[test]
fn a_note_saved_in_an_excluded_folder_stays_out_of_the_index() {
    let vault = indexed();
    let watch = Watch::start(&vault, &[]);

    vault.write("credentials/note.md", &note(70, "Concept", "Private."));
    vault.write("notes/after.md", &note(72, "Concept", "Fine."));

    watch.wait_until("the visible note", || is_indexed(&vault, 72));
    assert!(!is_indexed(&vault, 70));
}

#[test]
fn editing_gitignore_takes_a_folder_out_while_watching() {
    let vault = indexed();
    vault.write("kept/note.md", &note(71, "Concept", "Kept for now."));
    vault.ok(&["index"]);
    let watch = Watch::start(&vault, &[]);

    vault.write(".gitignore", "kept/\n");

    watch.wait_until("the note to leave", || !is_indexed(&vault, 71));
}

/// The note leaves the index and is on the list of what changed, so a lint
/// using the rule from when the watch began would read it again.
#[test]
fn a_note_excluded_while_watching_is_not_linted() {
    let vault = indexed();
    let broken = "[[01KBX000000000000000000099-gone]]";
    vault.write("kept/note.md", &note(73, "Concept", broken));
    vault.ok(&["index"]);
    let watch = Watch::start(&vault, &[]);

    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let kept = text.lines().filter(|line| !line.starts_with("exclude ="));
    let kept = kept.collect::<Vec<_>>().join("\n");
    fs::write(&file, format!("exclude = [\"kept\"]\n{kept}\n")).unwrap();

    watch.wait_until("the note to leave", || !is_indexed(&vault, 73));
    watch.settle(&vault);
    let lines = watch.lines();
    assert!(
        !lines.iter().any(|line| line.contains("kept/")),
        "{lines:?}"
    );
}

#[test]
fn it_finds_a_config_in_kbx() {
    let vault = Fixture::in_kbx();
    vault.ok(&["index"]);
    let watch = Watch::start(&vault, &[]);
    assert!(!is_indexed(&vault, 100));
    save(&vault, 1);
    watch.wait_until("the save to be indexed", || is_indexed(&vault, 100));
}

#[test]
fn editing_a_config_in_kbx_takes_a_folder_out_while_watching() {
    let vault = Fixture::in_kbx();
    vault.write("kept/note.md", &note(74, "Concept", "Kept for now."));
    vault.ok(&["index"]);
    let watch = Watch::start(&vault, &[]);

    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let kept = text.lines().filter(|line| !line.starts_with("exclude ="));
    let kept = kept.collect::<Vec<_>>().join("\n");
    fs::write(&file, format!("exclude = [\"kept\"]\n{kept}\n")).unwrap();

    watch.wait_until("the note to leave", || !is_indexed(&vault, 74));
}
