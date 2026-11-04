use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::{IndexError, select, walk};
use crate::note::{Note, ParseError, parse_note};
use crate::ontology::Ontology;
use crate::vault::{Finding, FindingCode, Vault};

/// Validates the notes under `paths`, or the whole vault if `paths` is empty,
/// ordered by path and then line. `paths` are relative to the working directory.
///
/// This reads the files, not the index, so it sees what's on disk now. Every
/// note is parsed, because links and duplicate ids depend on the whole vault,
/// but only the given files are reported on: §8 leaves `link.unresolved` to
/// the author of the file. Plain markdown isn't a note, and has no findings.
pub fn lint_vault(
    vault: &Path,
    ontology_yaml: &str,
    paths: &[PathBuf],
) -> Result<Vec<Finding>, IndexError> {
    let ontology = Ontology::parse(ontology_yaml)?;
    let selection = select(vault, paths)?;
    if let Some(missing) = selection.iter().find(|s| !s.absolute.exists()) {
        return Err(IndexError::NotFound(missing.absolute.clone()));
    }
    let selects = |path: &str| selection.iter().any(|s| s.contains(path));

    let mut files = BTreeSet::new();
    walk(vault, "", &mut files);
    let mut notes = Vec::new();
    let mut findings = Vec::new();
    for path in &files {
        match read_note(vault, path) {
            Ok(Some(note)) => notes.push((selects(path), note)),
            Ok(None) => {}
            Err((line, message)) if selects(path) => findings.push(Finding {
                code: FindingCode::NoteInvalid,
                path: path.into(),
                line,
                message,
            }),
            Err(_) => {}
        }
    }

    let (selected, notes): (Vec<bool>, Vec<Note>) = notes.into_iter().unzip();
    let vault = Vault::new(notes);
    for (note, _) in vault.notes().iter().zip(selected).filter(|(_, s)| *s) {
        findings.extend(vault.validate_note(note, &ontology));
    }
    findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(findings)
}

/// Reads a file as `kbx index` does. The error is a line and what's wrong, for
/// anything the index counts as failed.
fn read_note(vault: &Path, path: &str) -> Result<Option<Note>, (usize, String)> {
    let bytes = fs::read(vault.join(path)).map_err(|error| (1, error.to_string()))?;
    let src = std::str::from_utf8(&bytes).map_err(|error| (1, error.to_string()))?;
    match parse_note(path, src) {
        Ok(note) => Ok(Some(note)),
        Err(ParseError::NoFrontmatter) => Ok(None),
        Err(error) => Err((error.line(), error.to_string())),
    }
}
