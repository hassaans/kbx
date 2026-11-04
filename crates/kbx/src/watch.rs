//! `kbx watch`: keep the index current as notes are saved (§8).

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use kbx_core::{Graph, IndexOptions, Project, index_vault, is_note_path};
use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use tracing::warn;

use crate::{LintFormat, lint_project, print_index_report, read_ontology, suggest};

/// How soon to try a whole-vault pass again after something failed.
const RETRY: Duration = Duration::from_secs(5);

pub struct Options {
    pub debounce: Duration,
    /// How often to hash the whole vault, if at all.
    pub reconcile: Option<Duration>,
    /// How long after the last save to run `kbx suggest`, if at all.
    pub suggest: Option<Duration>,
}

pub fn run(options: Options) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let (sender, events) = mpsc::channel();
    let mut debouncer: Debouncer<RecommendedWatcher, RecommendedCache> =
        new_debouncer(options.debounce, None, sender)?;
    debouncer.watch(&project.vault, RecursiveMode::Recursive)?;

    // After the watcher starts, so that no save falls between the two.
    update(&project, Vec::new())?;
    println!("watching {}", project.vault.display());

    let mut timers = Timers::new(&options);
    loop {
        let message = match timers.next() {
            Some(at) => events.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(Ok(batch)) => {
                let (paths, rescan) = changed_paths(&project.vault, &batch);
                if rescan {
                    timers.reconcile_now();
                }
                if !paths.is_empty() {
                    match update(&project, paths) {
                        Ok(changed) => timers.after_batch(changed),
                        Err(error) => {
                            eprintln!("error: {error}");
                            timers.retry();
                        }
                    }
                }
            }
            Ok(Err(errors)) => {
                for error in errors {
                    warn!("{error}");
                }
                timers.reconcile_now();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("the file watcher stopped".into()),
        }

        if timers.take_reconcile() {
            match update(&project, Vec::new()) {
                Ok(changed) => timers.after_reconcile(changed),
                Err(error) => {
                    eprintln!("error: {error}");
                    timers.retry();
                }
            }
        }
        if timers.take_suggest() {
            let options = suggest::Options {
                note: None,
                generators: None,
                dry_run: false,
            };
            if let Err(error) = suggest::run(options) {
                eprintln!("error: {error}");
            }
        }
    }
}

/// When the work that isn't driven by an event is due.
struct Timers {
    reconcile_every: Option<Duration>,
    suggest_after: Option<Duration>,
    reconcile_at: Option<Instant>,
    /// A whole-vault pass is owed, because events were lost or a run failed.
    /// Only a pass that succeeds settles it.
    recover_at: Option<Instant>,
    suggest_at: Option<Instant>,
}

impl Timers {
    fn new(options: &Options) -> Timers {
        Timers {
            reconcile_every: options.reconcile,
            suggest_after: options.suggest,
            reconcile_at: options.reconcile.map(|every| Instant::now() + every),
            recover_at: None,
            suggest_at: None,
        }
    }

    fn next(&self) -> Option<Instant> {
        let timers = [self.reconcile_at, self.recover_at, self.suggest_at];
        timers.into_iter().flatten().min()
    }

    fn reconcile_now(&mut self) {
        self.recover_at = Some(Instant::now());
    }

    fn retry(&mut self) {
        self.recover_at = Some(Instant::now() + RETRY);
    }

    fn after_reconcile(&mut self, changed: bool) {
        self.recover_at = None;
        self.after_batch(changed);
    }

    /// A batch just ran, so a periodic reconcile can wait, and if it changed
    /// anything `suggest` waits for the saves to stop.
    fn after_batch(&mut self, changed: bool) {
        self.reconcile_at = self.reconcile_every.map(|every| Instant::now() + every);
        if changed && let Some(after) = self.suggest_after {
            self.suggest_at = Some(Instant::now() + after);
        }
    }

    fn take_reconcile(&mut self) -> bool {
        let now = Instant::now();
        let due = [self.reconcile_at, self.recover_at]
            .into_iter()
            .flatten()
            .any(|at| at <= now);
        if due {
            self.reconcile_at = self.reconcile_every.map(|every| now + every);
            // Until the pass reports back, so that a failure can't fire twice.
            self.recover_at = None;
        }
        due
    }

    fn take_suggest(&mut self) -> bool {
        let due = self.suggest_at.is_some_and(|at| at <= Instant::now());
        if due {
            self.suggest_at = None;
        }
        due
    }
}

/// Indexes `paths`, or the whole vault if there are none, and lints what
/// changed. Says whether anything did. Re-reads everything each time: the
/// ontology may have been edited and the database deleted, as a cache can be.
fn update(project: &Project, paths: Vec<PathBuf>) -> Result<bool, Box<dyn Error>> {
    let ontology = read_ontology(project)?;
    let mut graph = Graph::open(&project.graph)?;
    let options = IndexOptions {
        paths: paths.clone(),
        missing_ok: true,
        ..IndexOptions::default()
    };
    let report = index_vault(&mut graph, &project.vault, &ontology, &options)?;

    // A note that fails has no hash to compare, so it counts only when an
    // event names it. Otherwise every reconcile would report it again.
    let failed = !paths.is_empty() && report.failed > 0;
    if report.changed.is_empty() && !failed {
        return Ok(false);
    }
    print_index_report(&report);
    let changed = report.changed.iter().map(|path| project.vault.join(path));
    let mut linted: BTreeSet<PathBuf> = changed.chain(paths).collect();
    linted.retain(|path| path.is_file());
    if !linted.is_empty() {
        let linted: Vec<PathBuf> = linted.into_iter().collect();
        lint_project(project, LintFormat::Text, false, &linted)?;
    }
    Ok(true)
}

/// The paths in a batch worth indexing, and whether the events were lost so
/// that only a full pass can catch up.
fn changed_paths(vault: &Path, batch: &[DebouncedEvent]) -> (Vec<PathBuf>, bool) {
    let mut paths = BTreeSet::new();
    let mut rescan = false;
    for event in batch {
        rescan |= event.need_rescan();
        // Reading a file is an event on Linux, and indexing reads files.
        if matches!(event.kind, EventKind::Access(_)) {
            continue;
        }
        for path in &event.paths {
            if is_wanted(vault, path, &event.kind) {
                paths.insert(path.clone());
            }
        }
    }
    (paths.into_iter().collect(), rescan)
}

/// A note, or a folder that appeared or moved, whose notes raise no events of
/// their own. Anything gone may have been either. Hidden paths are left out,
/// which keeps the writes to `.kbx/` from waking the watch. Symlinks are left
/// out because the index doesn't follow them.
fn is_wanted(vault: &Path, path: &Path, kind: &EventKind) -> bool {
    let Ok(relative) = path.strip_prefix(vault) else {
        return false;
    };
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    if parts.is_empty() || parts.iter().any(|part| part.starts_with('.')) {
        return false;
    }
    match fs::symlink_metadata(path) {
        Ok(file) if file.is_file() => is_note_path(&parts.join("/")),
        Ok(file) if file.is_dir() => matches!(
            kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        ),
        Ok(_) => false,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timers(reconcile: Option<Duration>) -> Timers {
        Timers::new(&Options {
            debounce: Duration::from_millis(1),
            reconcile,
            suggest: None,
        })
    }

    #[test]
    fn a_batch_does_not_cancel_a_rescan() {
        for every in [None, Some(Duration::from_secs(3600))] {
            let mut timers = timers(every);
            timers.reconcile_now();
            timers.after_batch(true);
            assert!(timers.take_reconcile());
        }
    }

    #[test]
    fn a_batch_does_not_cancel_a_retry() {
        let mut timers = timers(None);
        timers.retry();
        timers.after_batch(true);
        assert!(timers.next().is_some());
    }

    #[test]
    fn a_reconcile_that_succeeds_settles_what_was_owed() {
        let mut timers = timers(None);
        timers.reconcile_now();
        assert!(timers.take_reconcile());
        timers.after_reconcile(false);
        assert_eq!(timers.next(), None);
    }
}
