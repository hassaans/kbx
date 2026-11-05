//! What a note without `id`, `type` or `title` is taken to say. Nothing here
//! is ever written to a note. See `docs/existing-vaults.md`.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::section::Section;

/// The class of a note with no `type`, unless `kbx.toml` says otherwise.
pub const DEFAULT_CLASS: &str = "Note";

/// The longest slug in an inferred id, in characters. Deep paths would
/// otherwise make ids that nobody can read.
const SLUG_CHARS: usize = 64;

/// Which of a note's keys are not in its frontmatter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inferred {
    /// `id` came from the path.
    pub id: bool,
    /// `type` came from `default_type` in `kbx.toml`.
    pub class: bool,
    /// `title` came from the first H1 or the filename.
    pub title: bool,
}

impl Inferred {
    /// Whether any key was inferred.
    pub fn any(&self) -> bool {
        self.id || self.class || self.title
    }
}

/// The classes of notes with no `type`: the `default_type` keys of `kbx.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDefaults {
    /// For a note in no scope, or in a scope with no class of its own.
    pub global: String,
    /// By scope, the top-level folder a note is in.
    pub scopes: BTreeMap<String, String>,
}

impl Default for TypeDefaults {
    fn default() -> TypeDefaults {
        TypeDefaults {
            global: DEFAULT_CLASS.to_string(),
            scopes: BTreeMap::new(),
        }
    }
}

impl TypeDefaults {
    /// The class of a note at `path`, from the vault root with `/` separators.
    pub fn class_for(&self, path: &str) -> &str {
        let scope = path.split_once('/').map(|(scope, _)| scope);
        let class = scope.and_then(|scope| self.scopes.get(scope));
        class.unwrap_or(&self.global)
    }

    /// Every class a note could be given, without repeats.
    pub fn classes(&self) -> Vec<&str> {
        let mut classes: Vec<&str> = self.scopes.values().map(String::as_str).collect();
        classes.push(&self.global);
        classes.sort_unstable();
        classes.dedup();
        classes
    }
}

/// The id of the note at `path`, from the vault root with `/` separators: the
/// path's slug, then the first 8 hex digits of the path's SHA-256.
///
/// It depends on the path alone, so a run over part of the vault gets the id a
/// rebuild does, and a note appearing elsewhere can't change it. The digest
/// tells apart `a/b.md`, `a-b.md` and `A/B.md`, which share a slug. It also
/// means an inferred id always holds a `-`, so the `{id}-slug` step of link
/// resolution, which looks up what comes before the first `-`, can't turn a
/// filename link such as `[[projects-plan]]` into a link to `projects.md`.
pub fn inferred_id(path: &str) -> String {
    let lower = path.to_lowercase();
    let stem = lower.strip_suffix(".md").unwrap_or(&lower);
    let words = stem
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty());
    let slug = words.collect::<Vec<_>>().join("-");
    let slug: String = slug.chars().take(SLUG_CHARS).collect();
    let slug = slug.trim_end_matches('-');
    let digest = Sha256::digest(path.as_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    match slug {
        "" => format!("note-{hex}"),
        slug => format!("{slug}-{hex}"),
    }
}

/// The text of the first H1 with any, or else the file's name.
pub(crate) fn inferred_title(sections: &[Section], path: &str) -> String {
    let headings = sections.iter().filter(|section| section.level == 1);
    let mut h1 = headings.filter_map(|section| section.heading_path.last());
    match h1.find(|text| !text.is_empty()) {
        Some(text) => text.clone(),
        None => file_stem(path).to_string(),
    }
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("md") => stem,
        _ => name,
    }
}
