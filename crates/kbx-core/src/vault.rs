use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_yaml::Value;

use crate::index::slash_path;
use crate::link::Link;
use crate::note::Note;
use crate::ontology::{Ontology, RelationKey, admits};

/// A set of parsed notes that links resolve against.
#[derive(Debug, Clone)]
pub struct Vault {
    notes: Vec<Note>,
    resolver: Resolver,
    /// The notes claiming each id, lowest path first.
    holders: HashMap<String, Vec<usize>>,
}

/// Finds what a link target names, from ids, paths and aliases alone. The
/// index resolves stored links with this, without parsing the notes again.
#[derive(Debug, Clone, Default)]
pub struct Resolver {
    by_id: HashMap<String, BTreeSet<usize>>,
    by_filename: HashMap<String, BTreeSet<usize>>,
    by_alias: HashMap<String, BTreeSet<usize>>,
    /// Paths without `.md`, as written, under their last two components: any
    /// partial path ends with those, and storing every ending costs too much.
    by_tail: HashMap<String, Vec<(usize, String)>>,
}

/// Where a link landed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved<'a> {
    /// The note the link points at.
    pub note: &'a Note,
    /// Which step matched.
    pub via: ResolvedVia,
}

/// Which step of the resolution order matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedVia {
    /// The target is the note's id, or the id followed by a slug
    /// (`{id}-tokens`), the shape §3 uses. The slug is ignored, so renaming
    /// the file doesn't break the link.
    Id,
    /// The target is the file's name, its vault path, or the end of that path
    /// from a folder boundary. Breaks on rename, so it's flagged
    /// [`FindingCode::LinkUnstable`].
    Filename,
    /// The target is one of the note's aliases.
    Alias,
}

/// A validation problem at a place in a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What kind of problem this is.
    pub code: FindingCode,
    /// The path of the note, as given to [`parse_note`](crate::parse_note).
    pub path: PathBuf,
    /// 1-based line.
    pub line: usize,
    /// A human-readable explanation.
    pub message: String,
}

/// The finding codes from §3 and §8, and three §3 has no rule for:
/// `rel.domain`, `note.invalid` and `id.duplicate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindingCode {
    /// `type` isn't a class in the ontology.
    TypeUnknown,
    /// A key holds wikilinks but isn't a relation or inverse in the ontology,
    /// or a relation key holds something other than wikilinks.
    RelUnknown,
    /// A link's target has a class outside the relation's `range`.
    RelRange,
    /// The note's class is outside the `domain` of a relation it holds.
    RelDomain,
    /// A relation the note's class `requires` is missing or empty.
    SchemaGap,
    /// An `exclusive` relation has more than one target.
    RelCardinality,
    /// A link resolved by filename only, to a note with an id of its own. A
    /// note whose id comes from its path has nothing steadier to link by.
    LinkUnstable,
    /// A link resolved to no note, or to more than one.
    LinkUnresolved,
    /// A file can't be read as a note, so `kbx index` fails it and it has no
    /// rows.
    NoteInvalid,
    /// Several files claim one id. The lowest path keeps it, and the others
    /// answer to nothing.
    IdDuplicate,
}

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Something is wrong, and `kbx lint` exits 1.
    Error,
    /// Worth fixing, but normal in a real vault, so it fails only under
    /// `kbx lint --strict`.
    Warning,
}

impl Severity {
    /// The severity as printed, such as `warning`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FindingCode {
    /// How much this code matters. Filename links are how most Obsidian vaults
    /// link, so `link.unstable` alone is a warning.
    pub fn severity(self) -> Severity {
        match self {
            FindingCode::LinkUnstable => Severity::Warning,
            _ => Severity::Error,
        }
    }

    /// The code as printed, such as `rel.range`.
    pub fn as_str(self) -> &'static str {
        match self {
            FindingCode::TypeUnknown => "type.unknown",
            FindingCode::RelUnknown => "rel.unknown",
            FindingCode::RelRange => "rel.range",
            FindingCode::RelDomain => "rel.domain",
            FindingCode::SchemaGap => "schema.gap",
            FindingCode::RelCardinality => "rel.cardinality",
            FindingCode::LinkUnstable => "link.unstable",
            FindingCode::LinkUnresolved => "link.unresolved",
            FindingCode::NoteInvalid => "note.invalid",
            FindingCode::IdDuplicate => "id.duplicate",
        }
    }
}

impl fmt::Display for FindingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Resolver {
    /// Registers a note under `index`, the caller's handle for it.
    pub fn add<S: AsRef<str>>(
        &mut self,
        index: usize,
        id: &str,
        path: &Path,
        aliases: impl IntoIterator<Item = S>,
    ) {
        self.by_id.entry(id.to_string()).or_default().insert(index);
        for name in filename_keys(path) {
            self.by_filename.entry(name).or_default().insert(index);
        }
        for alias in aliases {
            self.by_alias
                .entry(alias.as_ref().to_lowercase())
                .or_default()
                .insert(index);
        }
        let stored = path_key(&path.to_string_lossy());
        if let Some(tail) = tail(&stored) {
            let notes = self.by_tail.entry(tail.to_string()).or_default();
            notes.push((index, stored));
        }
    }

    /// Resolves by id first, then by filename, then by alias, then by partial
    /// path; filename and alias ignore case, as Obsidian does, and a partial
    /// path keeps it. The first step that matches decides, so a target is
    /// `None` if that step matches several notes.
    pub fn resolve(&self, target: &str) -> Option<(usize, ResolvedVia)> {
        let (matches, via) = self.matches(target)?;
        match matches[..] {
            [index] => Some((index, via)),
            _ => None,
        }
    }

    /// How many notes the deciding step of [`Resolver::resolve`] matches.
    pub fn match_count(&self, target: &str) -> usize {
        self.matches(target).map_or(0, |(matches, _)| matches.len())
    }

    /// What the first step to match anything found.
    fn matches(&self, target: &str) -> Option<(Vec<usize>, ResolvedVia)> {
        let id_prefix = target.split_once('-').map_or(target, |(id, _)| id);
        let name = normalize_name(target);
        let steps = [
            (self.by_id.get(target), ResolvedVia::Id),
            (self.by_id.get(id_prefix), ResolvedVia::Id),
            (self.by_filename.get(&name), ResolvedVia::Filename),
            (
                self.by_alias.get(&target.to_lowercase()),
                ResolvedVia::Alias,
            ),
        ];
        let found = steps.into_iter().find_map(|(hit, via)| Some((hit?, via)));
        if let Some((matches, via)) = found {
            return Some((matches.iter().copied().collect(), via));
        }
        let matches = self.path_endings(target);
        (!matches.is_empty()).then_some((matches, ResolvedVia::Filename))
    }

    /// The notes whose path ends with `target` at a folder boundary, which is
    /// how Obsidian reads a link with a folder in it. A full path never gets
    /// here, so the `/` before the target is always there to match.
    fn path_endings(&self, target: &str) -> Vec<usize> {
        let target = path_key(target);
        let Some(notes) = tail(&target).and_then(|tail| self.by_tail.get(tail)) else {
            return Vec::new();
        };
        let ending = format!("/{target}");
        let found = notes.iter().filter(|(_, path)| path.ends_with(&ending));
        found.map(|(index, _)| *index).collect()
    }
}

impl Vault {
    /// Indexes `notes` by id, filename and alias. Links to a name several
    /// notes share don't resolve. Of notes that share an id, only the one with
    /// the lowest path is registered, by id or any other name: `kbx index`
    /// gives the others no rows, and the two must agree on where a link goes.
    pub fn new(notes: Vec<Note>) -> Vault {
        let mut holders: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, note) in notes.iter().enumerate() {
            holders.entry(note.id.clone()).or_default().push(index);
        }
        let mut resolver = Resolver::default();
        for indexes in holders.values_mut() {
            // As strings, like the index: `Path` order puts `a/x` before `a-b/x`.
            indexes.sort_by_key(|&index| slash_path(&notes[index].path));
            let note = &notes[indexes[0]];
            resolver.add(indexes[0], &note.id, &note.path, &note.aliases);
        }
        Vault {
            notes,
            resolver,
            holders,
        }
    }

    /// The notes, in the order given to [`Vault::new`].
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// Resolves a link as [`Resolver::resolve`] does.
    pub fn resolve(&self, link: &Link) -> Option<Resolved<'_>> {
        let (index, via) = self.resolver.resolve(&link.target)?;
        Some(Resolved {
            note: &self.notes[index],
            via,
        })
    }

    /// Validates every note, ordered by path and then line.
    pub fn validate(&self, ontology: &Ontology) -> Vec<Finding> {
        let mut findings: Vec<Finding> = self
            .notes
            .iter()
            .flat_map(|note| self.validate_note(note, ontology))
            .collect();
        findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        findings
    }

    /// Validates one note against the whole vault. §8 reports
    /// `link.unresolved` only on files the author changed, so `kbx lint` calls
    /// this on those alone.
    pub fn validate_note(&self, note: &Note, ontology: &Ontology) -> Vec<Finding> {
        self.validate_note_with(note, ontology, &HashSet::new())
    }

    /// [`Vault::validate_note`], with `held` the `(id, key)` pairs kept in the
    /// database rather than in the note.
    pub(crate) fn validate_note_with(
        &self,
        note: &Note,
        ontology: &Ontology,
        held: &HashSet<(String, String)>,
    ) -> Vec<Finding> {
        let mut found = Vec::new();
        let mut report = |code, line, message| {
            found.push(Finding {
                code,
                path: note.path.clone(),
                line,
                message,
            });
        };

        if let Some(message) = self.duplicate_message(note) {
            report(FindingCode::IdDuplicate, note.id_line, message);
        }

        match ontology.classes.get(&note.class) {
            None => {
                let origin = if note.inferred.class { "default " } else { "" };
                let class = &note.class;
                let message = format!("{origin}type `{class}` is not a class in the ontology");
                report(FindingCode::TypeUnknown, note.class_line, message);
            }
            Some(class) => {
                for key in &class.requires {
                    let in_database = held.contains(&(note.id.clone(), key.clone()));
                    if note.relation(key).is_none() && !in_database {
                        let message = format!("{} requires `{key}`", note.class);
                        report(FindingCode::SchemaGap, note.class_line, message);
                    }
                }
            }
        }

        for entry in &note.relations {
            let relation = ontology.lookup(&entry.key);
            if relation.is_none() {
                let message = format!("`{}` is not a relation in the ontology", entry.key);
                report(FindingCode::RelUnknown, entry.line, message);
            }
            // A note of unknown class already has its own type.unknown.
            if let Some(relation) = relation
                && ontology.classes.contains_key(&note.class)
                && !admits(relation.domain(), &note.class)
            {
                let message = format!("{} is outside the domain of `{}`", note.class, entry.key);
                report(FindingCode::RelDomain, entry.line, message);
            }
            let mut targets = BTreeSet::new();
            for link in &entry.links {
                targets.insert(self.check_link(link, relation, ontology, &mut report));
            }
            if relation.is_some_and(|r| r.exclusive()) && targets.len() > 1 {
                let message = format!("`{}` allows only one target", entry.key);
                report(FindingCode::RelCardinality, entry.line, message);
            }
        }

        for property in &note.properties {
            let empty = match &property.value {
                Value::Null => true,
                Value::Sequence(items) => items.is_empty(),
                _ => false,
            };
            if !empty && ontology.lookup(&property.key).is_some() {
                let message = format!("`{}` must be a list of quoted wikilinks", property.key);
                report(FindingCode::RelUnknown, property.line, message);
            }
        }

        for link in note.mentions() {
            self.check_link(link, None, ontology, &mut report);
        }
        found
    }

    /// Says who else claims this note's id, if anyone does.
    fn duplicate_message(&self, note: &Note) -> Option<String> {
        let holders = self.holders.get(&note.id)?;
        let paths = holders.iter().map(|&index| &self.notes[index].path);
        let others: Vec<_> = paths
            .filter(|path| **path != note.path)
            .map(|path| path.display().to_string())
            .collect();
        if others.is_empty() {
            return None;
        }
        let keeper = self.notes[holders[0]].path.display();
        Some(format!(
            "id `{}` is also used by {}; links go to {keeper}",
            note.id,
            others.join(", ")
        ))
    }

    /// Reports on one link and returns what it points at, so that spellings
    /// of the same note count as one target.
    fn check_link(
        &self,
        link: &Link,
        relation: Option<RelationKey<'_>>,
        ontology: &Ontology,
        report: &mut impl FnMut(FindingCode, usize, String),
    ) -> String {
        let Some(resolved) = self.resolve(link) else {
            let mut message = format!("`{}` does not resolve to exactly one note", link.target);
            let count = self.resolver.match_count(&link.target);
            if count > 1 {
                message.push_str(&format!(": {count} notes match"));
            }
            report(FindingCode::LinkUnresolved, link.line, message);
            return link.target.clone();
        };
        if resolved.via == ResolvedVia::Filename && !resolved.note.inferred.id {
            let message = format!("`{}` links by filename, not id", link.target);
            report(FindingCode::LinkUnstable, link.line, message);
        }
        let class = &resolved.note.class;
        // A target of unknown class already has its own type.unknown.
        if let Some(relation) = relation
            && ontology.classes.contains_key(class)
            && !admits(relation.range(), class)
        {
            let message = format!("{} is outside the range of `{}`", class, relation.name);
            report(FindingCode::RelRange, link.line, message);
        }
        resolved.note.id.clone()
    }
}

/// The names a file answers to: its stem and its path without the extension.
fn filename_keys(path: &Path) -> BTreeSet<String> {
    let full = normalize_name(&path.to_string_lossy());
    let stem = full.rsplit('/').next().unwrap_or(&full).to_string();
    BTreeSet::from([stem, full])
}

fn normalize_name(name: &str) -> String {
    let name = name.replace('\\', "/").to_lowercase();
    name.strip_suffix(".md").map(str::to_string).unwrap_or(name)
}

/// A path as partial paths compare it: `/` separators, its case, and no `.md`
/// in any case, since the walk takes `.MD` for a note too.
fn path_key(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    if path.to_lowercase().ends_with(".md") {
        path.truncate(path.len() - ".md".len());
    }
    path
}

/// The last two components of `path`, if it has that many.
fn tail(path: &str) -> Option<&str> {
    let (rest, _) = path.rsplit_once('/')?;
    let start = rest.rfind('/').map_or(0, |slash| slash + 1);
    Some(&path[start..])
}
