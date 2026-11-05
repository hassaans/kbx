use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::exclude::Exclusions;
use crate::index::{IndexError, select, walk};
use crate::infer::TypeDefaults;
use crate::note::{Note, parse_note_with};
use crate::ontology::Ontology;
use crate::review::{AcceptedRelation, held_keys};
use crate::vault::{Finding, FindingCode, Vault};

/// Validates the notes under `paths`, or the whole vault if `paths` is empty,
/// ordered by path and then line. `paths` are relative to the working directory.
///
/// This reads the files, not the index, so it sees what's on disk now. Every
/// note is parsed, because links and duplicate ids depend on the whole vault,
/// but only the given files are reported on: §8 leaves `link.unresolved` to
/// the author of the file. What `exclusions` rules out has no findings, even
/// when named in `paths`. `accepted` is what the database holds and the files
/// don't, which meets a class's `requires` as a key in the note would.
pub fn lint_vault(
    vault: &Path,
    ontology_yaml: &str,
    exclusions: &Exclusions,
    defaults: &TypeDefaults,
    paths: &[PathBuf],
    accepted: &[AcceptedRelation],
) -> Result<Vec<Finding>, IndexError> {
    let ontology = Ontology::parse(ontology_yaml)?;
    let selection = select(vault, paths)?;
    if let Some(missing) = selection.iter().find(|s| !s.absolute.exists()) {
        return Err(IndexError::NotFound(missing.absolute.clone()));
    }
    let selects = |path: &str| selection.iter().any(|s| s.contains(path));

    let mut files = BTreeSet::new();
    walk(vault, "", exclusions, &mut files);
    let mut notes = Vec::new();
    let mut findings = Vec::new();
    for path in &files {
        match read_note(vault, path, defaults) {
            Ok(note) => notes.push((selects(path), note)),
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
    let held = held_keys(accepted, &ontology);
    for (note, _) in vault.notes().iter().zip(selected).filter(|(_, s)| *s) {
        findings.extend(vault.validate_note_with(note, &ontology, &held));
    }
    findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(findings)
}

/// Reads a file as `kbx index` does. The error is a line and what's wrong, for
/// anything the index counts as failed.
fn read_note(vault: &Path, path: &str, defaults: &TypeDefaults) -> Result<Note, (usize, String)> {
    let bytes = fs::read(vault.join(path)).map_err(|error| (1, error.to_string()))?;
    let src = std::str::from_utf8(&bytes).map_err(|error| (1, error.to_string()))?;
    parse_note_with(path, src, defaults).map_err(|error| (error.line(), error.to_string()))
}
