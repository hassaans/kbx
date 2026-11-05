//! Finding QMD, installing it when it's missing, and keeping a vault's
//! collections in it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use kbx_core::{Exclusions, MAX_MASK, Project, canonical_path, find_on_path};
use tracing::info;

use crate::plural;

const PACKAGE: &str = "@tobilu/qmd";
const INSTALL: &str = "npm i -g @tobilu/qmd";

/// After a collection's name, the name its replacement is read under.
const REPLACEMENT: &str = "-kbx-new";

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
    /// Collections of folders that are now excluded, and replacements an
    /// earlier run left behind.
    pub barred: Vec<String>,
    /// Collections whose mask isn't the one the folders on disk call for now.
    pub narrowing: Vec<String>,
    /// Collections whose mask stops short of what's excluded, with how many
    /// levels it reaches.
    pub stopped_short: Vec<(String, usize)>,
    /// Collections that couldn't be brought in line, with why. Those above are
    /// what went through.
    pub failed: Vec<(String, String)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.barred.is_empty() && self.narrowing.is_empty()
    }

    /// What the masks that stop short mean for the user, if there are any.
    pub fn mask_warning(&self) -> Option<String> {
        if self.stopped_short.is_empty() {
            return None;
        }
        let reach = |(name, levels): &(String, usize)| match levels {
            0 => format!("{name} reads every note"),
            _ => format!("{name} stops {} down", plural(*levels, "level")),
        };
        let masks: Vec<String> = self.stopped_short.iter().map(reach).collect();
        Some(format!(
            "{}: a mask around every excluded folder would pass {MAX_MASK} characters, so what's \
             excluded below that reaches qmd's local index, though never a judge",
            masks.join(", ")
        ))
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
    let named = project.config.qmd.index.is_some();
    let exclusions = project.exclusions().map_err(|e| e.to_string())?;
    for (name, root) in &planned.roots {
        if named && listed.contains_key(name) {
            continue;
        }
        let wanted = exclusions.note_mask(root);
        if let Some(levels) = wanted.levels {
            changes.stopped_short.push((name.clone(), levels));
        }
        let mask = listed.get(name).and_then(Option::as_ref);
        if mask.is_some_and(|mask| *mask != wanted.glob) {
            changes.narrowing.push(name.clone());
        }
    }
    if named {
        return Ok(changes);
    }
    let barred = project.config.qmd.roots.keys();
    let barred = barred.filter(|name| !planned.roots.contains_key(*name));
    changes.barred = barred
        .filter(|n| listed.contains_key(*n))
        .cloned()
        .collect();
    let left = listed.keys().filter(|name| name.ends_with(REPLACEMENT));
    let left = left.filter(|name| !planned.roots.contains_key(*name));
    changes.barred.extend(left.cloned());
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
/// it, returning what that took. Safe to run again. One collection failing
/// doesn't stop the rest; see [`Changes::failed`]. A collection is removed
/// before `kbx.toml` forgets it, so a failure leaves it there to try again.
pub fn set_up_collections(project: &mut Project) -> Result<Changes, String> {
    let mut changes = collection_changes(project)?;
    let planned = project.planned_qmd().map_err(|e| e.to_string())?;
    let exclusions = project.exclusions().map_err(|e| e.to_string())?;
    let mut failed = Vec::new();
    attempt(&mut changes.barred, &mut failed, |name| {
        remove_collection(project, name)
    });
    if failed.is_empty() {
        project.record_collections().map_err(|e| e.to_string())?;
    }
    let project = &*project;
    attempt(&mut changes.missing, &mut failed, |name| {
        add_collection(project, &exclusions, name, &planned.roots[name]).map(|_| ())
    });
    attempt(&mut changes.narrowing, &mut failed, |name| {
        let replacement = replacement_name(name, &planned.roots);
        replace_collection(
            project,
            &exclusions,
            name,
            &replacement,
            &planned.roots[name],
        )
    });
    changes.failed = failed;
    Ok(changes)
}

/// Runs `step` on each of `names`, moving those it fails on to `failed`.
fn attempt(
    names: &mut Vec<String>,
    failed: &mut Vec<(String, String)>,
    mut step: impl FnMut(&str) -> Result<(), String>,
) {
    names.retain(|name| match step(name) {
        Ok(()) => true,
        Err(error) => {
            failed.push((name.clone(), error));
            false
        }
    });
}

fn remove_collection(project: &Project, name: &str) -> Result<(), String> {
    let output = run(project, &["collection", "remove", name])?;
    if output.status.success() {
        return Ok(());
    }
    Err(failure(&output))
}

/// Returns whether this run added it. qmd records a collection before it reads
/// the folder, so one that fails is taken out again. Left there, it would pass
/// for a collection with its notes.
fn add_collection(
    project: &Project,
    exclusions: &Exclusions,
    name: &str,
    root: &str,
) -> Result<bool, String> {
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
        return Ok(!exists);
    }
    let _ = remove_collection(project, name);
    Err(failure(&output))
}

/// The name to read a replacement of `name` under, which no planned collection
/// has. It keeps the ending, so that one left behind is known for what it is.
fn replacement_name(name: &str, planned: &BTreeMap<String, String>) -> String {
    let mut replacement = format!("{name}{REPLACEMENT}");
    while planned.contains_key(&replacement) {
        replacement.push_str(REPLACEMENT);
    }
    replacement
}

/// Makes a collection again with the mask it should have now. qmd has no way to
/// change a mask or to add over a name, so the new one is read under another
/// name, and until that has worked the old one isn't touched. One already
/// there isn't this run's, so nothing says it has the right notes.
fn replace_collection(
    project: &Project,
    exclusions: &Exclusions,
    name: &str,
    replacement: &str,
    root: &str,
) -> Result<(), String> {
    if !add_collection(project, exclusions, replacement, root)? {
        return Err(format!("{replacement} is already in qmd"));
    }
    if let Err(error) = remove_collection(project, name) {
        let _ = remove_collection(project, replacement);
        return Err(error);
    }
    let output = run(project, &["collection", "rename", replacement, name])?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!("left as {replacement}: {}", failure(&output)))
}

/// What `qmd status` says that kbx has a use for.
pub struct Status {
    /// The lines naming the models, the same in qmd 1 and 2.
    pub models: String,
    /// The lines counting the index's files and vectors.
    pub index: String,
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
    (!models.is_empty()).then_some(Status { models, index })
}

/// Brings the vault's own index up to the notes on disk: `qmd query` finds
/// neighbours by their vectors, so a note has to be embedded to be found.
/// Both commands work on a whole index, which is why a named index, perhaps
/// shared with much else, is left alone.
pub fn refresh(project: &Project) -> Result<(), String> {
    if project.config.qmd.index.is_some() {
        return Ok(());
    }
    for step in ["update", "embed"] {
        info!("running qmd {step}");
        let output = run(project, &[step])?;
        if !output.status.success() {
            return Err(format!("qmd {step}: {}", failure(&output)));
        }
    }
    Ok(())
}
