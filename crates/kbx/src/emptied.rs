//! Says which top-level folders hold markdown and no note kbx reads, why, how
//! many of those files git tracks, and the line in `kbx.toml` that brings them
//! back. `kbx init` prints it and `kbx doctor` warns with it. See
//! `docs/excluding.md`.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use kbx_core::{EmptiedFolder, Excluded, Exclusions, Project, emptied_folders};

use crate::plural;

/// One line per emptied folder, then one for the fix. None if there are none.
pub fn lines(project: &Project, exclusions: &Exclusions) -> Vec<String> {
    let emptied = emptied_folders(&project.vault, exclusions);
    if emptied.is_empty() {
        return Vec::new();
    }
    let tracked = tracked(&project.vault, &emptied);
    let mut lines: Vec<String> = emptied
        .iter()
        .map(|folder| describe(folder, tracked.as_ref()))
        .collect();
    lines.push(fix(project, &emptied));
    lines
}

fn describe(emptied: &EmptiedFolder, tracked: Option<&HashSet<String>>) -> String {
    let count = emptied.files.len();
    let verb = if count == 1 { "is" } else { "are" };
    let reasons: Vec<String> = emptied
        .reasons
        .iter()
        .map(|(why, count)| format!("{count} {why}"))
        .collect();
    let mut line = format!(
        "{}: all {} in it {verb} excluded ({})",
        emptied.folder,
        plural(count, "markdown file"),
        reasons.join(", ")
    );
    if let Some(tracked) = tracked {
        let files = emptied.files.iter().filter(|file| tracked.contains(*file));
        line.push_str(&format!("; git tracks {} of them", files.count()));
    }
    line
}

/// What to change in `kbx.toml`: `include` for what `.gitignore` keeps out,
/// and `exclude` for what that keeps out, as `include` can't win over it.
fn fix(project: &Project, emptied: &[EmptiedFolder]) -> String {
    let file = if project.config_at_root() {
        "kbx.toml"
    } else {
        ".kbx/kbx.toml"
    };
    let reasons = emptied.iter().flat_map(|folder| &folder.reasons);
    let mut ignored = Vec::new();
    let mut listed = Vec::new();
    for folder in emptied {
        if folder
            .reasons
            .iter()
            .any(|(why, _)| matches!(why, Excluded::Ignored(_)))
        {
            ignored.push(format!("/{}/", escape_pattern(&folder.folder)));
        }
    }
    for (why, _) in reasons {
        if let Excluded::Listed(pattern) = why
            && !listed.contains(pattern)
        {
            listed.push(pattern.clone());
        }
    }
    let mut fixes = Vec::new();
    if !ignored.is_empty() {
        let include = &project.config.include;
        let mut patterns = include.clone();
        patterns.extend(ignored);
        let quoted: Vec<String> = patterns.iter().map(|p| toml_string(p)).collect();
        let line = format!("include = [{}]", quoted.join(", "));
        fixes.push(match include.is_empty() {
            true => format!("add `{line}` to {file}"),
            false => format!("change `include` in {file} to `{line}`"),
        });
    }
    for pattern in listed {
        fixes.push(format!("remove `{pattern}` from `exclude` in {file}"));
    }
    format!("to read them, {}", fixes.join("; "))
}

/// `name` as a `.gitignore` pattern matching only itself.
fn escape_pattern(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for (at, c) in name.chars().enumerate() {
        if "\\*?[".contains(c) || at == 0 && "!#".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// A TOML basic string: a JSON string is one.
fn toml_string(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

/// The files under `emptied` that git tracks, from one `git ls-files` for the
/// vault. None if git isn't on `PATH` or the vault isn't in a repository.
fn tracked(vault: &Path, emptied: &[EmptiedFolder]) -> Option<HashSet<String>> {
    let mut command = Command::new("git");
    command.arg("-C").arg(vault).args(["ls-files", "-z", "--"]);
    for folder in emptied {
        command.arg(format!(":(literal){}", folder.folder));
    }
    let output = command.stdin(Stdio::null()).stderr(Stdio::null()).output();
    let output = output.ok().filter(|output| output.status.success())?;
    let paths = output.stdout.split(|byte| *byte == 0);
    let paths = paths.filter(|path| !path.is_empty());
    Some(
        paths
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect(),
    )
}
