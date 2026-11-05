//! Finding QMD, installing it when it's missing, and keeping a vault's
//! collections in it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use kbx_core::{Project, canonical_path, find_on_path};
use tracing::info;

use crate::embedding::{Backlog, Coverage};

const PACKAGE: &str = "@tobilu/qmd";
const INSTALL: &str = "npm i -g @tobilu/qmd";

/// `qmd embed` on an M2 Pro: about 20 a second on `~/HQ`, 23 on the 10,000-note
/// vault.
const CHUNKS_PER_SECOND: f64 = 20.0;

/// What a note comes to before anything is embedded to measure it by: `~/HQ`'s
/// 256,317 chunks of 80,050 documents. Short notes come to less.
const CHUNKS_PER_DOCUMENT: f64 = 3.2;

const DOCUMENTS: &str = "documents";

/// Finds `qmd` on `PATH`, installing it first if it isn't there. Says nothing
/// on success. The error is the whole report for the user.
pub fn ensure() -> Result<PathBuf, String> {
    if let Some(path) = find_on_path("qmd") {
        return Ok(path);
    }
    let (Some(_), Some(npm)) = (find_on_path("node"), find_on_path("npm")) else {
        return Err(format!(
            "qmd is not installed, and installing it needs Node >= 22 and npm, \
             which are not on PATH. Install Node >= 22, then run:\n  {INSTALL}"
        ));
    };

    info!("installing qmd with {}", npm.display());
    // By the path that was found: on Windows npm is a `.cmd`, which a bare
    // `npm` wouldn't run. Nothing reads stdin, so nothing can prompt.
    let output = Command::new(&npm)
        .args(["i", "-g", PACKAGE])
        .stdin(Stdio::null())
        .output();
    let failure = match output {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "{}\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) => Some(error.to_string()),
    };
    if let Some(failure) = failure {
        return Err(format!(
            "`{INSTALL}` failed: {}\nTo install qmd by hand, run:\n  {INSTALL}",
            failure.trim_end()
        ));
    }
    find_on_path("qmd").ok_or_else(|| {
        format!(
            "`{INSTALL}` finished, but qmd is not on PATH. \
             Add the bin folder under `npm prefix -g` to PATH."
        )
    })
}

/// The program `kbx.toml` names, by its full path if it's on `PATH`.
pub fn program(project: &Project) -> PathBuf {
    let bin = &project.config.qmd.bin;
    find_on_path(bin).unwrap_or_else(|| PathBuf::from(bin))
}

/// Runs qmd on the vault's index. The error is one line for the user.
fn run(project: &Project, args: &[&str]) -> Result<Output, String> {
    let bin = program(project);
    Command::new(&bin)
        .args(["--index", &project.qmd_index()])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run {}: {error}", bin.display()))
}

fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{} {}", output.status, stderr.trim())
}

/// What it takes to bring qmd's collections in line with the plan.
#[derive(Default)]
pub struct Changes {
    /// Planned collections qmd doesn't have yet.
    pub missing: Vec<String>,
    /// Collections of folders that are now excluded.
    pub barred: Vec<String>,
    /// Collections whose mask isn't the one the folders on disk call for now.
    pub narrowing: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.barred.is_empty() && self.narrowing.is_empty()
    }
}

/// Compares qmd's collections with the plan. A named index is left to whoever
/// named it, so nothing in it is taken away or changed.
pub fn collection_changes(project: &Project) -> Result<Changes, String> {
    let listed = listed_collections(project)?;
    let planned = project.planned_qmd().map_err(|e| e.to_string())?;
    let missing = planned.roots.keys().filter(|n| !listed.contains_key(*n));
    let mut changes = Changes {
        missing: missing.cloned().collect(),
        ..Changes::default()
    };
    if project.config.qmd.index.is_some() {
        return Ok(changes);
    }
    let barred = project.config.qmd.roots.keys();
    let barred = barred.filter(|name| !planned.roots.contains_key(*name));
    changes.barred = barred
        .filter(|n| listed.contains_key(*n))
        .cloned()
        .collect();
    let exclusions = project.exclusions().map_err(|e| e.to_string())?;
    for (name, root) in &planned.roots {
        let mask = listed.get(name).and_then(Option::as_ref);
        if mask.is_some_and(|mask| *mask != exclusions.note_glob(root)) {
            changes.narrowing.push(name.clone());
        }
    }
    Ok(changes)
}

/// Each collection in qmd, with its mask if `qmd collection list` shows it.
fn listed_collections(project: &Project) -> Result<BTreeMap<String, Option<String>>, String> {
    let output = run(project, &["collection", "list"])?;
    if !output.status.success() {
        return Err(failure(&output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut listed = BTreeMap::new();
    let mut current = None;
    for line in stdout.lines() {
        if let Some(name) = listed_name(line) {
            listed.insert(name.to_string(), None);
            current = Some(name);
        } else if let (Some(name), Some(mask)) = (current, listed_mask(line)) {
            listed.insert(name.to_string(), Some(mask.to_string()));
        }
    }
    Ok(listed)
}

/// The name in a `name (qmd://name/)` line of `qmd collection list`.
fn listed_name(line: &str) -> Option<&str> {
    line.split_once(" (qmd://").map(|(name, _)| name)
}

/// The mask in a `  Pattern:  mask` line of `qmd collection list`.
fn listed_mask(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("Pattern:").map(str::trim)
}

/// Records every scope's collection in `kbx.toml` and brings qmd in line with
/// it, returning what that took. Safe to run again. A collection is removed
/// before `kbx.toml` forgets it, so a failure leaves it there to try again.
pub fn set_up_collections(project: &mut Project) -> Result<Changes, String> {
    let changes = collection_changes(project)?;
    for name in changes.barred.iter().chain(&changes.narrowing) {
        remove_collection(project, name)?;
    }
    project.record_collections().map_err(|e| e.to_string())?;
    for name in changes.missing.iter().chain(&changes.narrowing) {
        add_collection(project, name)?;
    }
    Ok(changes)
}

fn remove_collection(project: &Project, name: &str) -> Result<(), String> {
    let output = run(project, &["collection", "remove", name])?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!("collection {name}: {}", failure(&output)))
}

fn add_collection(project: &Project, name: &str) -> Result<(), String> {
    let root = &project.config.qmd.roots[name];
    let exclusions = project.exclusions().map_err(|e| e.to_string())?;
    let mask = exclusions.note_glob(root);
    let folder = project.vault.join(root);
    let folder = canonical_path(&folder).unwrap_or(folder);
    let folder = folder.to_string_lossy();
    let args = [
        "collection",
        "add",
        &folder,
        "--name",
        name,
        "--mask",
        &mask,
    ];
    let output = run(project, &args)?;
    // Another run got there first, which is what was wanted.
    let exists = String::from_utf8_lossy(&output.stderr).contains("already exists.");
    if output.status.success() || exists {
        return Ok(());
    }
    Err(format!("collection {name}: {}", failure(&output)))
}

/// What `qmd status` says that kbx has a use for.
pub struct Status {
    /// The lines naming the models, the same in qmd 1 and 2.
    pub models: String,
    /// The lines counting the index's files and vectors.
    pub index: String,
    /// Those counts, if they could be read.
    counts: Option<Counts>,
}

/// What the `Total:`, `Vectors:` and `Pending:` lines say. No `Pending:` line
/// means that nothing is waiting.
#[derive(Clone, Copy)]
struct Counts {
    documents: usize,
    /// Chunks, of which a document can have several.
    vectors: usize,
    /// Documents with no vector.
    pending: usize,
}

impl Status {
    /// What's left to embed, if qmd says. It counts documents, not chunks, so
    /// those come from the documents embedded so far.
    pub fn backlog(&self) -> Option<Backlog> {
        let Counts {
            documents,
            vectors,
            pending,
        } = self.counts?;
        let embedded = documents.saturating_sub(pending);
        let per_document = match (vectors, embedded) {
            (0, _) | (_, 0) => CHUNKS_PER_DOCUMENT,
            _ => vectors as f64 / embedded as f64,
        };
        let chunks = pending as f64 * per_document;
        Some(Backlog {
            count: pending,
            unit: DOCUMENTS,
            chunks: Some(chunks.round() as usize),
            seconds: chunks / CHUNKS_PER_SECOND,
        })
    }

    pub fn coverage(&self) -> Option<Coverage> {
        let counts = self.counts?;
        Some(Coverage {
            embedded: counts.documents.saturating_sub(counts.pending),
            total: counts.documents,
            unit: DOCUMENTS,
        })
    }
}

/// The number that follows `label` in `stdout`.
fn count_after(stdout: &str, label: &str) -> Option<usize> {
    let (_, rest) = stdout.split_once(label)?;
    let rest = rest.trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

fn counts(stdout: &str) -> Option<Counts> {
    Some(Counts {
        documents: count_after(stdout, "Total:")?,
        vectors: count_after(stdout, "Vectors:")?,
        pending: count_after(stdout, "Pending:").unwrap_or(0),
    })
}

/// `None` if qmd doesn't say which models it uses, and with no way to tell
/// when they change, nothing it finds can be kept.
pub fn status(project: &Project) -> Option<Status> {
    let output = run(project, &["status"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = |labels: &[&str]| {
        let wanted = |line: &&str| labels.iter().any(|label| line.contains(label));
        let lines: Vec<&str> = stdout.lines().filter(wanted).map(str::trim).collect();
        lines.join("\n")
    };
    let models = lines(&["Embedding:", "Reranking:", "Generation:"]);
    let index = lines(&["Total:", "Vectors:", "Pending:"]);
    (!models.is_empty()).then_some(Status {
        models,
        index,
        counts: counts(&stdout),
    })
}

/// Brings the vault's own index up to the text of the notes on disk.
pub fn update(project: &Project) -> Result<(), String> {
    refresh(project, "update")
}

/// Embeds what `update` found: `qmd query` finds neighbours by their vectors,
/// so a note with none can only be found by its words.
pub fn embed(project: &Project) -> Result<(), String> {
    refresh(project, "embed")
}

/// How to do what [`embed`] does, on its own or from `kbx`.
pub fn embed_by_hand(project: &Project) -> String {
    let bin = &project.config.qmd.bin;
    let index = project.qmd_index();
    format!("`kbx suggest --embed wait`, or `{bin} --index {index} embed`")
}

/// Both steps work on a whole index, which is why a named index, perhaps
/// shared with much else, is left alone.
fn refresh(project: &Project, step: &str) -> Result<(), String> {
    if project.config.qmd.index.is_some() {
        return Ok(());
    }
    info!("running qmd {step}");
    let output = run(project, &[step])?;
    if !output.status.success() {
        return Err(format!("qmd {step}: {}", failure(&output)));
    }
    Ok(())
}
