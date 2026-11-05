use std::env;
use std::io;
use std::path::{Path, PathBuf};

/// Finds a program on `PATH`.
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    // Windows has an `npm` beside `npm.cmd`, a shell script it can't run.
    let names = if cfg!(windows) {
        vec![format!("{program}.exe"), format!("{program}.cmd")]
    } else {
        vec![program.to_string()]
    };
    let folders = env::split_paths(&env::var_os("PATH")?).collect::<Vec<_>>();
    folders
        .iter()
        .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
        .find(|path| path.is_file())
}

/// `path` resolved as [`Path::canonicalize`] does, in the form every other
/// program can read. On Windows `canonicalize` adds a `\\?\` prefix, which
/// Node and much else reject, so kbx resolves all its paths with this.
pub fn canonical_path(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().map(|path| plain_path(&path))
}

/// `path` without a Windows verbatim prefix. Anywhere else `\\?\` is just a
/// strange name, and stays.
pub(crate) fn plain_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(text) if cfg!(windows) => without_verbatim_prefix(text).into(),
        _ => path.into(),
    }
}

/// Drops the prefix from a drive or a network share. Nothing else has a plain
/// form: without its prefix, `\\?\Volume{…}\` wouldn't be a path at all.
pub(crate) fn without_verbatim_prefix(path: &str) -> String {
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{share}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_and_shares_lose_the_verbatim_prefix() {
        assert_eq!(without_verbatim_prefix(r"\\?\C:\a b\c"), r"C:\a b\c");
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\host\share\c"),
            r"\\host\share\c"
        );
    }

    #[test]
    fn paths_without_a_plain_form_or_a_prefix_stay() {
        for path in [r"\\?\Volume{1234}\c", r"C:\a\c", r"\\host\share\c", "/a/c"] {
            assert_eq!(without_verbatim_prefix(path), path);
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_prefix_goes_on_windows() {
        assert_eq!(plain_path(Path::new(r"\\?\C:\a\c")), Path::new(r"C:\a\c"));
        let dir = tempfile::TempDir::new().unwrap();
        let verbatim = dir.path().canonicalize().unwrap();
        assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));
        let canonical = canonical_path(dir.path()).unwrap();
        assert!(!canonical.to_str().unwrap().starts_with(r"\\?\"));
        assert!(canonical.is_dir());
    }

    #[cfg(not(windows))]
    #[test]
    fn paths_are_left_alone_elsewhere() {
        for path in ["/a/c", r"\\?\C:\a\c", "relative"] {
            assert_eq!(plain_path(Path::new(path)), Path::new(path));
        }
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            canonical_path(dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }
}
