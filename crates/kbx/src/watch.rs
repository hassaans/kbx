//! `kbx watch`: keep the index current as notes are saved (§8).

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use kbx_core::{CONFIG_FILE, Exclusions, Graph, IndexOptions, Project, index_vault, is_note_path};
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
    // The debouncer ages each event on its own, so a burst longer than its
    // timeout comes out in pieces. It only thins the events: `Timers` waits for
    // the quiet that ends a burst, and the gaps between pieces fit inside that.
    let mut debouncer: Debouncer<RecommendedWatcher, RecommendedCache> =
        new_debouncer(options.debounce / 4, None, sender)?;
    debouncer.watch(&project.vault, RecursiveMode::Recursive)?;

    // After the watcher starts, so that no save falls between the two.
    update(&project, Vec::new())?;
    println!("watching {}", project.vault.display());

    let mut timers = Timers::new(&options);
    let mut saved = BTreeSet::new();
    loop {
        let message = match timers.next() {
            Some(at) => events.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(Ok(batch)) => match {
                eprintln!(
                    "DIAG {:?} {} batch n={} first={:?} rescan={}",
                    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap(),
                    project.vault.display(),
                    batch.len(),
                    batch.first().map(|event| (event.kind, event.paths.clone())),
                    batch.iter().any(|event| event.need_rescan()),
                );
                current_exclusions(&project)
            } {
                Ok(exclusions) => {
                    let (paths, rescan) = changed_paths(&project.vault, &exclusions, &batch);
                    if rescan {
                        timers.reconcile_now();
                    }
                    if !paths.is_empty() {
                        saved.extend(paths);
                        timers.after_save(Instant::now());
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    timers.retry();
                }
            },
            Ok(Err(errors)) => {
                eprintln!("DIAG {} watcher errors {errors:?}", project.vault.display());
                for error in errors {
                    warn!("{error}");
                }
                timers.reconcile_now();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("the file watcher stopped".into()),
        }

        if timers.take_index(Instant::now()) {
            // Windows can hold back the events of a burst until it ends, so
            // the files have the last word on whether the saves have stopped.
            let diag_quiet = since_last_write(&saved);
            eprintln!(
                "DIAG {:?} {} take_index quiet={diag_quiet:?} window={:?} saved={}",
                SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap(),
                project.vault.display(),
                options.debounce,
                saved.len(),
            );
            match since_last_write(&saved).filter(|quiet| *quiet < options.debounce) {
                Some(quiet) => timers.index_after(Instant::now(), options.debounce - quiet),
                None => match update(&project, mem::take(&mut saved).into_iter().collect()) {
                    Ok(changed) => timers.after_batch(changed),
                    Err(error) => {
                        eprintln!("error: {error}");
                        timers.retry();
                    }
                },
            }
        }
        if timers.take_reconcile() {
            eprintln!("DIAG {} reconcile pass", project.vault.display());
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
                // Never on a timer: judging spends money on every save.
                judge: suggest::Judge::None,
                stage: false,
                budget: None,
                batch: None,
                budget_usd: None,
                dry_run: false,
                eval: false,
            };
            if let Err(error) = suggest::run(options) {
                eprintln!("error: {error}");
            }
        }
    }
}

/// When the work that isn't driven by an event is due.
struct Timers {
    debounce: Duration,
    reconcile_every: Option<Duration>,
    suggest_after: Option<Duration>,
    /// When the saved notes are indexed, if the saves have stopped by then.
    index_at: Option<Instant>,
    reconcile_at: Option<Instant>,
    /// A whole-vault pass is owed, because events were lost or a run failed.
    /// Only a pass that succeeds settles it.
    recover_at: Option<Instant>,
    suggest_at: Option<Instant>,
}

impl Timers {
    fn new(options: &Options) -> Timers {
        Timers {
            debounce: options.debounce,
            reconcile_every: options.reconcile,
            suggest_after: options.suggest,
            index_at: None,
            reconcile_at: options.reconcile.map(|every| Instant::now() + every),
            recover_at: None,
            suggest_at: None,
        }
    }

    fn next(&self) -> Option<Instant> {
        // Saves still to be indexed come first: `suggest` reads the index.
        let suggest_at = self.suggest_at.filter(|_| self.index_at.is_none());
        let timers = [
            self.index_at,
            self.reconcile_at,
            self.recover_at,
            suggest_at,
        ];
        timers.into_iter().flatten().min()
    }

    /// Every save starts the wait again, so a burst is indexed once it ends,
    /// however long it lasts.
    fn after_save(&mut self, now: Instant) {
        self.index_after(now, self.debounce);
    }

    fn index_after(&mut self, now: Instant, wait: Duration) {
        self.index_at = Some(now + wait);
    }

    fn take_index(&mut self, now: Instant) -> bool {
        let due = self.index_at.is_some_and(|at| at <= now);
        if due {
            self.index_at = None;
        }
        due
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
        let waiting = self.index_at.is_some();
        let due = !waiting && self.suggest_at.is_some_and(|at| at <= Instant::now());
        if due {
            self.suggest_at = None;
        }
        due
    }
}

/// How long ago the most recently written of `paths` was written. A path that
/// is gone or dated in the future says nothing, so it can't hold up the index.
fn since_last_write(paths: &BTreeSet<PathBuf>) -> Option<Duration> {
    let now = SystemTime::now();
    let written = paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok()?.modified().ok());
    written.filter_map(|at| now.duration_since(at).ok()).min()
}

/// The rule as `kbx.toml` and the `.gitignore` files now have it. They can change
/// while `project`, read at the start, stays as it was.
fn current_exclusions(project: &Project) -> Result<Exclusions, Box<dyn Error>> {
    Ok(Project::load(&project.config_file)?.exclusions()?)
}

/// Indexes `paths`, or the whole vault if there are none, and lints what
/// changed. Says whether anything did. Re-reads everything each time: `kbx.toml`
/// and the ontology may have been edited and the database deleted, as a cache
/// can be.
fn update(project: &Project, paths: Vec<PathBuf>) -> Result<bool, Box<dyn Error>> {
    let project = &Project::load(&project.config_file)?;
    let ontology = read_ontology(project)?;
    let mut graph = Graph::open(&project.graph)?;
    let options = IndexOptions {
        paths: paths.clone(),
        missing_ok: true,
        ..IndexOptions::default()
    };
    let exclusions = project.exclusions()?;
    let defaults = project.config.type_defaults();
    let report = index_vault(
        &mut graph,
        &project.vault,
        &ontology,
        &exclusions,
        &defaults,
        &options,
    )?;

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

/// The paths in a batch worth indexing, and whether only a full pass can catch
/// up: the events were lost, or the rule on what's excluded changed.
fn changed_paths(
    vault: &Path,
    exclusions: &Exclusions,
    batch: &[DebouncedEvent],
) -> (Vec<PathBuf>, bool) {
    let mut paths = BTreeSet::new();
    let mut rescan = false;
    for event in batch {
        rescan |= event.need_rescan();
        // Reading a file is an event on Linux, and indexing reads files.
        if matches!(event.kind, EventKind::Access(_)) {
            continue;
        }
        for path in &event.paths {
            rescan |= changes_exclusions(path);
            if is_wanted(vault, exclusions, path, &event.kind) {
                paths.insert(path.clone());
            }
        }
    }
    (paths.into_iter().collect(), rescan)
}

fn changes_exclusions(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == ".gitignore" || name == CONFIG_FILE)
}

/// A note, or a folder that appeared or moved, whose notes raise no events of
/// their own. Anything gone may have been either. What's excluded is left out,
/// and that keeps the writes to `.kbx/` from waking the watch. Symlinks are
/// left out because the index doesn't follow them.
fn is_wanted(vault: &Path, exclusions: &Exclusions, path: &Path, kind: &EventKind) -> bool {
    let Ok(relative) = path.strip_prefix(vault) else {
        return false;
    };
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    let relative = parts.join("/");
    if parts.is_empty() {
        return false;
    }
    let metadata = fs::symlink_metadata(path);
    let is_dir = metadata.as_ref().is_ok_and(|file| file.is_dir());
    if exclusions.why(&relative, is_dir).is_some() {
        return false;
    }
    match metadata {
        Ok(file) if file.is_file() => is_note_path(&relative),
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

    const DEBOUNCE: Duration = Duration::from_millis(100);

    fn timers(reconcile: Option<Duration>) -> Timers {
        Timers::new(&Options {
            debounce: DEBOUNCE,
            reconcile,
            suggest: None,
        })
    }

    #[test]
    fn saves_inside_the_window_put_off_the_index_until_they_stop() {
        let mut timers = timers(None);
        let start = Instant::now();
        let mut last = start;
        for n in 1..=10 {
            last = start + DEBOUNCE / 2 * n;
            assert!(!timers.take_index(last));
            timers.after_save(last);
        }

        assert!(!timers.take_index(last + DEBOUNCE / 2));
        assert!(timers.take_index(last + DEBOUNCE));
        assert!(!timers.take_index(last + DEBOUNCE * 2));
        assert_eq!(timers.next(), None);
    }

    #[test]
    fn a_save_after_the_index_waits_again() {
        let mut timers = timers(None);
        let start = Instant::now();
        timers.after_save(start);
        assert!(timers.take_index(start + DEBOUNCE));

        timers.after_save(start + DEBOUNCE);
        assert!(!timers.take_index(start + DEBOUNCE));
        assert!(timers.take_index(start + DEBOUNCE * 2));
    }

    #[test]
    fn suggest_waits_for_the_saves_still_to_be_indexed() {
        let mut timers = Timers::new(&Options {
            debounce: DEBOUNCE,
            reconcile: None,
            suggest: Some(Duration::ZERO),
        });
        timers.after_batch(true);

        let now = Instant::now();
        timers.after_save(now);
        assert!(!timers.take_suggest());
        assert_eq!(timers.next(), Some(now + DEBOUNCE));

        assert!(timers.take_index(now + DEBOUNCE));
        assert!(timers.take_suggest());
    }

    #[test]
    fn a_file_written_just_now_counts_and_a_missing_one_does_not() {
        let dir = tempfile::TempDir::new().unwrap();
        let note = dir.path().join("note.md");
        let gone = BTreeSet::from([dir.path().join("gone.md")]);
        assert_eq!(since_last_write(&gone), None);

        fs::write(&note, "saved").unwrap();
        let quiet = since_last_write(&BTreeSet::from([note]));
        assert!(quiet.is_none_or(|quiet| quiet < Duration::from_secs(60)));
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
