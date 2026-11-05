//! What kbx leaves alone in a vault: hidden files and folders, what `exclude`
//! in `kbx.toml` lists and what `.gitignore` files ignore, unless `include`
//! brings it back. Everything that lists files or folders asks [`Exclusions`],
//! so that a note kept from one part of kbx is kept from all of it. See
//! `docs/excluding.md`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tracing::warn;

use crate::index::{is_note_path, slash_path};

/// What a `kbx.toml` without `exclude` means, and what `kbx init` writes.
pub const DEFAULT_EXCLUDE: [&str; 9] = [
    "node_modules",
    ".git",
    ".obsidian",
    ".kbx",
    "credentials",
    "secrets",
    ".ssh",
    ".aws",
    ".gnupg",
];

const GITIGNORE: &str = ".gitignore";

/// A mask for a folder with no notes kbx may read. qmd takes no empty mask, and
/// a hidden name is one that no note can have.
const NO_NOTES: &str = ".kbx-no-notes.md";

/// Why a path is left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Excluded {
    /// Its name starts with a dot.
    Hidden,
    /// `exclude` has this pattern.
    Listed(String),
    /// A `.gitignore` file ignores it. The file, from the vault if it's inside.
    Ignored(String),
}

impl fmt::Display for Excluded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Excluded::Hidden => write!(f, "hidden"),
            Excluded::Listed(pattern) => write!(f, "matches `{pattern}` in `exclude`"),
            Excluded::Ignored(file) => write!(f, "ignored by {file}"),
        }
    }
}

/// `exclude` or `include` has a pattern that can't be read.
#[derive(Debug, thiserror::Error)]
#[error("`{key}` in kbx.toml: {source}")]
pub struct ExcludeError {
    key: &'static str,
    source: ignore::Error,
}

/// What `include` brings back from `.gitignore` files. A pattern that matches a
/// folder covers what's in it, and `!` takes a path out again.
#[derive(Debug)]
struct Included {
    patterns: Gitignore,
    /// A pattern matches at any depth, so any folder may hold a match.
    anywhere: bool,
    /// Matches the folders on the way to what the other patterns match.
    towards: Gitignore,
}

impl Included {
    fn new(root: &Path, include: &[String]) -> Result<Included, ExcludeError> {
        let error = |source| ExcludeError {
            key: "include",
            source,
        };
        let mut patterns = GitignoreBuilder::new(root);
        let mut towards = GitignoreBuilder::new(root);
        let mut anywhere = false;
        for pattern in include {
            patterns.add_line(None, pattern).map_err(error)?;
            match folders_towards(pattern) {
                // A slash in a character class can leave half of one behind.
                Some(folders) => {
                    let mut added = folders.iter().map(|f| towards.add_line(None, f).is_ok());
                    anywhere |= !added.all(|ok| ok);
                }
                None => anywhere = true,
            }
        }
        Ok(Included {
            patterns: patterns.build().map_err(error)?,
            anywhere,
            towards: towards.build().map_err(error)?,
        })
    }

    fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether `path`, from the vault root, or a folder it's in is listed.
    fn covers(&self, path: &str, is_dir: bool) -> bool {
        let found = self
            .patterns
            .matched_path_or_any_parents(Path::new(path), is_dir);
        found.is_ignore()
    }

    /// Whether something under `folder` could be listed. It errs towards yes:
    /// this only says what's worth reading.
    fn may_be_under(&self, folder: &str) -> bool {
        self.anywhere || self.towards.matched(Path::new(folder), true).is_ignore()
    }
}

/// `.gitignore` patterns for the folders that lead to what `pattern` matches,
/// or `None` if it can match at any depth.
fn folders_towards(pattern: &str) -> Option<Vec<String>> {
    let line = pattern.trim_end();
    if line.is_empty() || line.starts_with(['#', '!']) {
        return Some(Vec::new());
    }
    let from_root = line.starts_with('/');
    let line = line.trim_start_matches('/').trim_end_matches('/');
    if !from_root && !line.contains('/') {
        return None;
    }
    let parts: Vec<&str> = line.split('/').collect();
    let mut folders = Vec::new();
    for end in 0..parts.len() - 1 {
        let path = parts[..end].join("/");
        if parts[end].contains("**") {
            if end == 0 {
                return None;
            }
            folders.push(format!("/{path}/**"));
            break;
        }
        folders.push(format!("/{}", parts[..=end].join("/")));
    }
    Some(folders)
}

/// The rule for one vault, as of now: `.gitignore` files are read when first
/// needed and kept, so make a new one to see them change.
///
/// Only `.gitignore` files count. `.git/info/exclude` and git's global file are
/// left out: they belong to a clone or a machine, so two copies of a vault would
/// differ, and a vault can use them to keep nearly everything out of git.
///
/// Hidden paths and `exclude` always count. `include` wins over `.gitignore`
/// and nothing else, and this is the only place that says so: a folder that's
/// ignored but may lead to something included isn't left alone, though what's
/// in it is unless it's included too.
#[derive(Debug)]
pub struct Exclusions {
    root: PathBuf,
    listed: Gitignore,
    included: Included,
    /// The `.gitignore` files above the vault, nearest first, up to the root of
    /// the repository it's in.
    above: Vec<Gitignore>,
    /// Each folder's `.gitignore`, by path from the vault root.
    folders: RefCell<HashMap<String, Rc<Gitignore>>>,
}

impl Exclusions {
    /// The rule for the vault at `root`, with `exclude` for its list. A name in
    /// the list matches in any case: `Secrets` is as private as `secrets`.
    pub fn new(root: &Path, exclude: &[String]) -> Result<Exclusions, ExcludeError> {
        Exclusions::with_include(root, exclude, &[])
    }

    /// [`new`](Exclusions::new), with `include` for what `.gitignore` files
    /// don't get to keep out. It matches in the case it's written: unlike
    /// `exclude`, a miss here is the safe way to be wrong.
    pub fn with_include(
        root: &Path,
        exclude: &[String],
        include: &[String],
    ) -> Result<Exclusions, ExcludeError> {
        let error = |source| ExcludeError {
            key: "exclude",
            source,
        };
        let mut listed = GitignoreBuilder::new(root);
        listed.case_insensitive(true).map_err(error)?;
        for pattern in exclude {
            listed.add_line(None, pattern).map_err(error)?;
        }
        Ok(Exclusions {
            root: root.to_path_buf(),
            listed: listed.build().map_err(error)?,
            included: Included::new(root, include)?,
            above: gitignores_above(root),
            folders: RefCell::default(),
        })
    }

    /// Why `path` is left alone, if it is: because of itself or of a folder it's
    /// in. `path` is from the vault root, with `/` separators.
    pub fn why(&self, path: &str, is_dir: bool) -> Option<Excluded> {
        let path = path.trim_start_matches("./").trim_end_matches('/');
        if path.is_empty() || path == "." {
            return None;
        }
        from_the_top(path, is_dir).find_map(|(part, is_dir)| self.entry(part, is_dir))
    }

    /// Why `path` is left alone, given that the folders it's in aren't. Git
    /// goes the same way: nothing can bring back a file in an ignored folder.
    /// Only `include` can, so with one a folder may not be left alone and yet
    /// be ignored, and what's in it has to ask about the folders above.
    pub(crate) fn entry(&self, path: &str, is_dir: bool) -> Option<Excluded> {
        if let Some(why) = self.barred(path, is_dir) {
            return Some(why);
        }
        if self.included.is_empty() {
            return self.gitignored(path, is_dir);
        }
        if self.included.covers(path, is_dir) {
            return None;
        }
        let why = self.gitignored_from_the_top(path, is_dir)?;
        (!is_dir || !self.included.may_be_under(path)).then_some(why)
    }

    /// Why `path` is left alone whatever `include` says.
    pub(crate) fn barred(&self, path: &str, is_dir: bool) -> Option<Excluded> {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.starts_with('.') {
            return Some(Excluded::Hidden);
        }
        match self.listed.matched(self.root.join(path), is_dir) {
            Match::Ignore(glob) => Some(Excluded::Listed(glob.original().to_string())),
            _ => None,
        }
    }

    /// Whether `include` covers `path`, whatever else is said about it.
    pub(crate) fn includes(&self, path: &str, is_dir: bool) -> bool {
        self.included.covers(path, is_dir)
    }

    /// What the `.gitignore` files say about `path` itself, given that the
    /// folders it's in aren't ignored.
    pub(crate) fn gitignored(&self, path: &str, is_dir: bool) -> Option<Excluded> {
        let absolute = self.root.join(path);
        let folders = path.rmatch_indices('/').map(|(at, _)| &path[..at]);
        for folder in folders.chain([""]) {
            let file = self.in_folder(folder);
            match self.ignored(&file, &absolute, is_dir) {
                Some(answer) => return answer,
                None => continue,
            }
        }
        for file in &self.above {
            if let Some(answer) = self.ignored(file, &absolute, is_dir) {
                return answer;
            }
        }
        None
    }

    /// Why the `.gitignore` files ignore `path` or a folder it's in.
    fn gitignored_from_the_top(&self, path: &str, is_dir: bool) -> Option<Excluded> {
        from_the_top(path, is_dir).find_map(|(part, is_dir)| self.gitignored(part, is_dir))
    }

    /// What `file` says about a path, if anything. The nearest file with an
    /// opinion wins, so `Some(None)` is a path it brought back with `!`.
    fn ignored(&self, file: &Gitignore, path: &Path, is_dir: bool) -> Option<Option<Excluded>> {
        match file.matched(path, is_dir) {
            Match::None => None,
            Match::Whitelist(_) => Some(None),
            Match::Ignore(glob) => {
                let from = glob.from().unwrap_or(Path::new(GITIGNORE));
                let from = from.strip_prefix(&self.root).unwrap_or(from);
                Some(Some(Excluded::Ignored(slash_path(from))))
            }
        }
    }

    fn in_folder(&self, folder: &str) -> Rc<Gitignore> {
        let mut folders = self.folders.borrow_mut();
        let file = folders.entry(folder.to_string());
        file.or_insert_with(|| Rc::new(gitignore_in(&self.root.join(folder))))
            .clone()
    }

    /// A glob for the markdown under `folder`, from `folder`, for qmd, which
    /// takes a glob and no ignore list. It lists the folders that are clear
    /// all the way down, and goes file by file only around what's excluded.
    /// Hidden folders are left to qmd, which never reads them, and so is a
    /// single excluded file: its hits come to nothing, as it has no note. In a
    /// folder only `include` opens, the glob takes just the notes it brings
    /// back, so that what `.gitignore` keeps from kbx isn't given to qmd.
    pub fn note_glob(&self, folder: &str) -> String {
        let folder = folder.trim_start_matches("./").trim_end_matches('/');
        let folder = if folder == "." { "" } else { folder };
        let mut globs = Vec::new();
        if !self.around(folder, "", &mut globs) {
            return "**/*.md".to_string();
        }
        match &globs[..] {
            [] => NO_NOTES.to_string(),
            [one] => one.clone(),
            many => format!("{{{}}}", many.join(",")),
        }
    }

    /// Adds the globs for `folder`, or nothing and returns `false` if nothing
    /// under it is excluded. `inside` is `folder` from where the glob starts,
    /// with a trailing `/` unless it's empty. An opened folder has something
    /// excluded by definition, and may add nothing at all.
    fn around(&self, folder: &str, inside: &str, globs: &mut Vec<String>) -> bool {
        let mut clear = Vec::new();
        let mut narrowed = Vec::new();
        let opened = self.opened(folder);
        let mut any = opened;
        for name in visible_folders(&self.root.join(folder)) {
            let path = match folder {
                "" => name.clone(),
                _ => format!("{folder}/{name}"),
            };
            let glob = format!("{inside}{}", escape_glob(&name));
            let excluded = self.entry(&path, true).is_some();
            if excluded || self.around(&path, &format!("{glob}/"), &mut narrowed) {
                any = true;
            } else {
                clear.push(format!("{glob}/**/*.md"));
            }
        }
        if opened {
            globs.append(&mut self.restored_in(folder, inside));
        } else if any {
            globs.push(format!("{inside}*.md"));
        }
        if any {
            let mut rest: Vec<String> = clear.into_iter().chain(narrowed).collect();
            rest.sort();
            globs.append(&mut rest);
        }
        any
    }

    /// Whether `folder`, which isn't left alone, is read only for what
    /// `include` brings back from it.
    fn opened(&self, folder: &str) -> bool {
        !self.included.is_empty()
            && !folder.is_empty()
            && !self.included.covers(folder, true)
            && self.gitignored_from_the_top(folder, true).is_some()
    }

    /// Globs for the notes directly in an opened `folder`: one for all of them
    /// if `include` brings them all back, or else one for each it does.
    fn restored_in(&self, folder: &str, inside: &str) -> Vec<String> {
        let notes = visible_entries(&self.root.join(folder), |kind| kind.is_file());
        let notes = notes.into_iter().filter(|name| is_note_path(name));
        let (restored, left): (Vec<String>, Vec<String>) =
            notes.partition(|name| self.entry(&format!("{folder}/{name}"), false).is_none());
        if left.is_empty() && !restored.is_empty() {
            return vec![format!("{inside}*.md")];
        }
        let names = restored.iter().map(|name| escape_glob(name));
        names.map(|name| format!("{inside}{name}")).collect()
    }

    /// The top-level folders that `.gitignore` takes out and `include` brings
    /// notes back from. `scopes` won't have them, as `kbx init` skipped them.
    pub fn restored_folders(&self) -> Vec<String> {
        if self.included.is_empty() {
            return Vec::new();
        }
        let folders = visible_folders(&self.root).into_iter();
        let folders = folders.filter(|folder| {
            self.entry(folder, true).is_none() && self.gitignored(folder, true).is_some()
        });
        folders
            .filter(|folder| self.note_glob(folder) != NO_NOTES)
            .collect()
    }
}

/// `path` and the folders it's in, the outermost first, with whether each is a
/// folder.
fn from_the_top(path: &str, is_dir: bool) -> impl Iterator<Item = (&str, bool)> {
    let ends = path.match_indices('/').map(|(at, _)| at);
    let folders = ends.map(|end| (&path[..end], true));
    folders.chain([(path, is_dir)])
}

/// The folders directly in `dir`, sorted. Symlinks aren't followed.
fn visible_folders(dir: &Path) -> Vec<String> {
    visible_entries(dir, |kind| kind.is_dir())
}

/// What's directly in `dir` of a `kind`, sorted, leaving out hidden names.
fn visible_entries(dir: &Path, kind: fn(&fs::FileType) -> bool) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|found| kind(&found)))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// `name` as a glob matching only itself.
fn escape_glob(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for c in name.chars() {
        if "\\*?[]{}()!@+|,".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn gitignore_in(dir: &Path) -> Gitignore {
    let mut file = GitignoreBuilder::new(dir);
    let path = dir.join(GITIGNORE);
    if path.is_file()
        && let Some(error) = file.add(&path)
    {
        // As git does: the lines that can be read still count.
        warn!("{}: {error}", path.display());
    }
    file.build().unwrap_or_else(|error| {
        warn!("{}: {error}", path.display());
        Gitignore::empty()
    })
}

/// The `.gitignore` files from the folder above `root` up to the repository's
/// root, nearest first. None if `root` is a repository or isn't in one: without
/// a repository, nothing ties a folder to whatever is above it.
fn gitignores_above(root: &Path) -> Vec<Gitignore> {
    let mut folders = root.ancestors();
    if folders.next().is_some_and(|dir| dir.join(".git").exists()) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for folder in folders {
        found.push(gitignore_in(folder));
        if folder.join(".git").exists() {
            return found;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn rules(dir: &TempDir, exclude: &[&str]) -> Exclusions {
        let exclude: Vec<String> = exclude.iter().map(|name| name.to_string()).collect();
        Exclusions::new(dir.path(), &exclude).unwrap()
    }

    fn write(dir: &TempDir, path: &str, text: &str) {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn a_listed_name_matches_at_any_depth_in_any_case() {
        let dir = TempDir::new().unwrap();
        let rules = rules(&dir, &["secrets"]);
        let listed = Some(Excluded::Listed("secrets".to_string()));
        assert_eq!(rules.why("secrets", true), listed);
        assert_eq!(rules.why("a/b/Secrets/c.md", false), listed);
        assert_eq!(rules.why("a/secrets.md", false), None);
        assert_eq!(rules.why("a/mysecrets/b.md", false), None);
    }

    #[test]
    fn a_listed_path_with_a_slash_starts_at_the_vault() {
        let dir = TempDir::new().unwrap();
        let rules = rules(&dir, &["team/private"]);
        assert!(rules.why("team/private/a.md", false).is_some());
        assert_eq!(rules.why("other/team/private/a.md", false), None);
    }

    #[test]
    fn the_vault_itself_is_never_excluded() {
        let dir = TempDir::new().unwrap();
        let rules = rules(&dir, &["*"]);
        assert_eq!(rules.why(".", true), None);
        assert_eq!(rules.why("", true), None);
    }

    #[test]
    fn hidden_counts_whatever_the_list_says() {
        let dir = TempDir::new().unwrap();
        let rules = rules(&dir, &[]);
        assert_eq!(rules.why(".trash/a.md", false), Some(Excluded::Hidden));
        assert_eq!(rules.why("a/.b/c.md", false), Some(Excluded::Hidden));
    }

    #[test]
    fn a_nearer_gitignore_can_bring_a_file_back_but_not_out_of_an_ignored_folder() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "*.log.md\nout/\n");
        write(&dir, "a/.gitignore", "!keep.log.md\n");
        write(&dir, "out/.gitignore", "!*\n");
        let rules = rules(&dir, &[]);
        assert!(rules.why("b/keep.log.md", false).is_some());
        assert_eq!(rules.why("a/keep.log.md", false), None);
        assert!(rules.why("out/a.md", false).is_some());
    }

    #[test]
    fn it_says_which_gitignore() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a/.gitignore", "drafts/\n");
        let rules = rules(&dir, &[]);
        let why = rules.why("a/drafts/b.md", false).unwrap();
        assert_eq!(why, Excluded::Ignored("a/.gitignore".to_string()));
        assert_eq!(why.to_string(), "ignored by a/.gitignore");
    }

    #[test]
    fn a_folder_only_pattern_leaves_a_file_of_that_name() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "notes.md/\n");
        let rules = rules(&dir, &[]);
        assert_eq!(rules.why("notes.md", false), None);
        assert!(rules.why("notes.md/a.md", false).is_some());
    }

    #[test]
    fn a_bad_pattern_is_an_error() {
        let dir = TempDir::new().unwrap();
        let bad = ["[z-a".to_string()];
        assert!(Exclusions::new(dir.path(), &bad).is_err());
    }

    fn with_include(dir: &TempDir, exclude: &[&str], include: &[&str]) -> Exclusions {
        let strings = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Exclusions::with_include(dir.path(), &strings(exclude), &strings(include)).unwrap()
    }

    #[test]
    fn include_wins_over_a_gitignore_at_any_level() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n*.log.md\n");
        write(&dir, "personal/journal/.gitignore", "drafts/\n");
        let rules = with_include(&dir, &[], &["/personal/journal", "keep.log.md"]);
        assert_eq!(rules.why("personal/journal/a.md", false), None);
        assert_eq!(rules.why("personal/journal/drafts/b.md", false), None);
        assert_eq!(rules.why("a/keep.log.md", false), None);
        assert!(rules.why("a/other.log.md", false).is_some());
    }

    #[test]
    fn exclude_and_hidden_win_over_include() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n");
        let rules = with_include(
            &dir,
            &["secrets", "*.private.md"],
            &["/personal", "**/*.md"],
        );
        let listed = |pattern: &str| Some(Excluded::Listed(pattern.to_string()));
        assert_eq!(rules.why("personal/a.md", false), None);
        assert_eq!(rules.why("personal/secrets/b.md", false), listed("secrets"));
        assert_eq!(rules.why("secrets/b.md", false), listed("secrets"));
        assert_eq!(
            rules.why("personal/b.private.md", false),
            listed("*.private.md")
        );
        assert_eq!(
            rules.why("personal/.trash/c.md", false),
            Some(Excluded::Hidden)
        );
        assert_eq!(rules.why(".trash/c.md", false), Some(Excluded::Hidden));
        assert_eq!(rules.why("personal/.d.md", false), Some(Excluded::Hidden));
    }

    /// What's beside an included note in an ignored folder stays ignored, and
    /// says which file ignores it, though the folder itself has to be read.
    #[test]
    fn an_ignored_folder_opens_only_as_far_as_include_reaches() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\nout/\n");
        let rules = with_include(&dir, &[], &["personal/journal/*.md"]);
        let ignored = Some(Excluded::Ignored(".gitignore".to_string()));
        assert_eq!(rules.why("personal", true), None);
        assert_eq!(rules.why("personal/journal", true), None);
        assert_eq!(rules.why("personal/journal/a.md", false), None);
        assert_eq!(rules.why("personal/journal/deep/a.md", false), ignored);
        assert_eq!(rules.why("personal/journal/deep", true), ignored);
        assert_eq!(rules.why("personal/taxes", true), ignored);
        assert_eq!(rules.why("personal/taxes/a.md", false), ignored);
        assert_eq!(rules.why("personal/a.md", false), ignored);
        assert_eq!(rules.why("out", true), ignored);
        assert_eq!(rules.why("out/a.md", false), ignored);
    }

    #[test]
    fn a_pattern_with_no_folder_to_start_from_opens_every_ignored_folder() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "out/\n");
        for pattern in ["keep.md", "**/keep.md"] {
            let rules = with_include(&dir, &[], &[pattern]);
            assert_eq!(rules.why("out/deep/keep.md", false), None);
            assert!(rules.why("out/deep/other.md", false).is_some());
        }
    }

    #[test]
    fn a_double_star_opens_everything_below_it() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n");
        let rules = with_include(&dir, &[], &["personal/**/keep.md"]);
        assert_eq!(rules.why("personal/a/b/keep.md", false), None);
        assert_eq!(rules.why("personal/keep.md", false), None);
        assert!(rules.why("personal/a/b/other.md", false).is_some());
    }

    #[test]
    fn a_negated_include_stays_ignored() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n");
        let rules = with_include(&dir, &[], &["/personal", "!/personal/taxes"]);
        assert_eq!(rules.why("personal/a.md", false), None);
        assert!(rules.why("personal/taxes/b.md", false).is_some());
    }

    #[test]
    fn include_matches_in_the_case_it_is_written() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n");
        let rules = with_include(&dir, &[], &["/Personal"]);
        assert!(rules.why("personal/a.md", false).is_some());
    }

    #[test]
    fn include_adds_nothing_that_was_not_ignored_and_never_excludes() {
        let dir = TempDir::new().unwrap();
        let rules = with_include(&dir, &["secrets"], &["/notes"]);
        assert_eq!(rules.why("other/a.md", false), None);
        assert_eq!(rules.why("notes/a.md", false), None);
        assert!(rules.why("notes/secrets/a.md", false).is_some());
    }

    #[test]
    fn a_bad_include_pattern_is_an_error_that_names_the_key() {
        let dir = TempDir::new().unwrap();
        let bad = ["[z-a".to_string()];
        let error = Exclusions::with_include(dir.path(), &[], &bad).unwrap_err();
        assert!(error.to_string().starts_with("`include`"), "{error}");
    }

    #[test]
    fn a_mask_lists_only_the_restored_notes_of_an_opened_folder() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "team/private/\n");
        write(&dir, "team/a.md", "");
        write(&dir, "team/private/keep (1).md", "");
        write(&dir, "team/private/left.md", "");
        write(&dir, "team/private/rest/b.md", "");
        let rules = with_include(&dir, &[], &["team/private/keep*.md"]);
        assert_eq!(rules.note_glob("team"), r"{*.md,private/keep \(1\).md}");
    }

    #[test]
    fn a_mask_takes_a_whole_folder_that_include_covers() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "team/private/\n");
        write(&dir, "team/private/a/b.md", "");
        write(&dir, "team/private/credentials/c.md", "");
        let rules = with_include(&dir, &["credentials"], &["team/private"]);
        assert_eq!(rules.note_glob("team/private"), "{*.md,a/**/*.md}");
        assert_eq!(
            rules.note_glob("team"),
            "{*.md,private/*.md,private/a/**/*.md}"
        );
    }

    #[test]
    fn a_mask_uses_a_star_when_every_note_in_an_opened_folder_is_restored() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n");
        write(&dir, "personal/a.md", "");
        write(&dir, "personal/b.md", "");
        write(&dir, "personal/taxes/c.md", "");
        let rules = with_include(&dir, &[], &["personal/*.md"]);
        assert_eq!(rules.note_glob("personal"), "*.md");
    }

    #[test]
    fn a_mask_of_an_opened_folder_with_nothing_restored_finds_nothing() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "personal/\n");
        write(&dir, "personal/a.md", "");
        let rules = with_include(&dir, &[], &["personal/journal/*.md"]);
        assert_eq!(rules.note_glob("personal"), NO_NOTES);
    }

    #[test]
    fn a_top_level_folder_counts_as_restored_only_with_a_note_to_show() {
        let dir = TempDir::new().unwrap();
        write(&dir, ".gitignore", "kept/\nempty/\nout/\n");
        write(&dir, "kept/journal/a.md", "");
        write(&dir, "empty/a.md", "");
        write(&dir, "out/a.md", "");
        write(&dir, "open/a.md", "");
        let rules = with_include(&dir, &[], &["kept/journal", "empty/journal"]);
        assert_eq!(rules.restored_folders(), ["kept"]);
    }

    #[test]
    fn a_glob_escapes_what_means_something_in_one() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a/{b,c} (d)/e.md", "");
        write(&dir, "a/secrets/f.md", "");
        let glob = rules(&dir, &["secrets"]).note_glob("a");
        assert_eq!(glob, r"{*.md,\{b\,c\} \(d\)/**/*.md}");
    }

    #[test]
    fn a_glob_starts_at_its_folder() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a/b/secrets/c.md", "");
        write(&dir, "a/d/e.md", "");
        let rules = rules(&dir, &["secrets"]);
        assert_eq!(rules.note_glob("a"), "{*.md,b/*.md,d/**/*.md}");
        assert_eq!(rules.note_glob("a/d"), "**/*.md");
        assert_eq!(rules.note_glob("."), "{*.md,a/*.md,a/b/*.md,a/d/**/*.md}");
    }
}
