//! Finding QMD, installing it when it's missing, and keeping a vault's
//! collections in it.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use kbx_core::{Project, canonical_path, find_on_path};
use tracing::info;

const PACKAGE: &str = "@tobilu/qmd";
const INSTALL: &str = "npm i -g @tobilu/qmd";
const MASK: &str = "**/*.md";

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

/// The planned collections qmd doesn't have yet.
pub fn missing_collections(project: &Project) -> Result<Vec<String>, String> {
    let output = run(project, &["collection", "list"])?;
    if !output.status.success() {
        return Err(failure(&output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let existing: Vec<&str> = stdout.lines().filter_map(listed_name).collect();
    let planned = project.planned_qmd().roots.into_keys();
    Ok(planned
        .filter(|name| !existing.contains(&name.as_str()))
        .collect())
}

/// The name in a `name (qmd://name/)` line of `qmd collection list`.
fn listed_name(line: &str) -> Option<&str> {
    line.split_once(" (qmd://").map(|(name, _)| name)
}

/// Records every scope's collection in `kbx.toml` and creates the ones qmd
/// doesn't have, returning their names. Safe to run again.
pub fn set_up_collections(project: &mut Project) -> Result<Vec<String>, String> {
    project.record_collections().map_err(|e| e.to_string())?;
    let missing = missing_collections(project)?;
    for name in &missing {
        add_collection(project, name)?;
    }
    Ok(missing)
}

fn add_collection(project: &Project, name: &str) -> Result<(), String> {
    let root = &project.config.qmd.roots[name];
    let folder = project.vault.join(root);
    let folder = canonical_path(&folder).unwrap_or(folder);
    let folder = folder.to_string_lossy();
    let args = ["collection", "add", &folder, "--name", name, "--mask", MASK];
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
