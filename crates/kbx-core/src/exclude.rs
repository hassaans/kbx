//! What kbx leaves alone in a vault: hidden files and folders, what `exclude`
//! in `kbx.toml` lists and what `.gitignore` files ignore. Everything that
//! lists files or folders asks [`Exclusions`], so that a note kept from one
//! part of kbx is kept from all of it. See `docs/excluding.md`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tracing::warn;

use crate::index::slash_path;

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

/// `exclude` has a pattern that can't be read.
#[derive(Debug, thiserror::Error)]
#[error("`exclude` in kbx.toml: {0}")]
pub struct ExcludeError(#[from] ignore::Error);

/// The rule for one vault, as of now: `.gitignore` files are read when first
/// needed and kept, so make a new one to see them change.
///
/// Only `.gitignore` files count. `.git/info/exclude` and git's global file are
/// left out: they belong to a clone or a machine, so two copies of a vault would
/// differ, and a vault can use them to keep nearly everything out of git.
#[derive(Debug)]
pub struct Exclusions {
    root: PathBuf,
    listed: Gitignore,
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
        let mut listed = GitignoreBuilder::new(root);
        listed.case_insensitive(true)?;
        for pattern in exclude {
            listed.add_line(None, pattern)?;
        }
        Ok(Exclusions {
            root: root.to_path_buf(),
            listed: listed.build()?,
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
        let ends = path.match_indices('/').map(|(at, _)| at);
        let folders = ends.map(|end| (&path[..end], true));
        folders
            .chain([(path, is_dir)])
            .find_map(|(part, is_dir)| self.entry(part, is_dir))
    }

    /// Why `path` is left alone, given that the folders it's in aren't. Git
    /// goes the same way: nothing can bring back a file in an ignored folder.
    pub(crate) fn entry(&self, path: &str, is_dir: bool) -> Option<Excluded> {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.starts_with('.') {
            return Some(Excluded::Hidden);
        }
        let absolute = self.root.join(path);
        if let Match::Ignore(glob) = self.listed.matched(&absolute, is_dir) {
            return Some(Excluded::Listed(glob.original().to_string()));
        }
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
    /// single excluded file: its hits come to nothing, as it has no note.
    pub fn note_glob(&self, folder: &str) -> String {
        let folder = folder.trim_start_matches("./").trim_end_matches('/');
        let folder = if folder == "." { "" } else { folder };
        let mut globs = Vec::new();
        if !self.around(folder, "", &mut globs) {
            return "**/*.md".to_string();
        }
        match &globs[..] {
            [one] => one.clone(),
            many => format!("{{{}}}", many.join(",")),
        }
    }

    /// Adds the globs for `folder`, or nothing and returns `false` if nothing
    /// under it is excluded. `inside` is `folder` from where the glob starts,
    /// with a trailing `/` unless it's empty.
    fn around(&self, folder: &str, inside: &str, globs: &mut Vec<String>) -> bool {
        let mut clear = Vec::new();
        let mut narrowed = Vec::new();
        let mut any = false;
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
        if any {
            globs.push(format!("{inside}*.md"));
            let mut rest: Vec<String> = clear.into_iter().chain(narrowed).collect();
            rest.sort();
            globs.append(&mut rest);
        }
        any
    }
}

/// The folders directly in `dir`, sorted. Symlinks aren't followed.
fn visible_folders(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
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
