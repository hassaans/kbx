//! Drives `kbx hook install` and real commits in temporary git repositories.

mod common;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{FIXTURE, FakePath, copy_dir, id, stderr, stdout, write_executable};
use tempfile::TempDir;

const CLEAN: u32 = 100;
const FAILING: u32 = 101;

fn note(n: u32, class: &str) -> String {
    format!(
        "---\nid: {}\ntype: {class}\ntitle: Note {n}\n---\nBody.\n",
        id(n)
    )
}

fn clean() -> String {
    note(CLEAN, "Concept")
}

fn failing() -> String {
    note(FAILING, "Recipe")
}

/// `PATH` with the built kbx first, so that the hook finds it.
fn path_with_kbx() -> OsString {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_kbx"));
    let mut dirs = vec![binary.parent().unwrap().to_path_buf()];
    dirs.extend(env::split_paths(&env::var_os("PATH").unwrap()));
    env::join_paths(dirs).unwrap()
}

/// `PATH` as it is, less anywhere a kbx is installed.
fn path_without_kbx() -> OsString {
    let path = env::var_os("PATH").unwrap();
    let dirs = env::split_paths(&path)
        .filter(|dir| !dir.join("kbx").exists() && !dir.join("kbx.exe").exists());
    env::join_paths(dirs).unwrap()
}

/// A git repository whose vault sits at `vault`, relative to its root.
struct Repo {
    dir: TempDir,
    vault: PathBuf,
    /// Stands in for the user's git config, which may set `core.hooksPath`.
    config: TempDir,
}

impl Repo {
    fn new() -> Repo {
        Repo::with_vault_at("")
    }

    fn with_vault_at(vault: &str) -> Repo {
        let repo = Repo {
            dir: TempDir::new().unwrap(),
            vault: PathBuf::from(vault),
            config: TempDir::new().unwrap(),
        };
        fs::write(repo.config.path().join("gitconfig"), "").unwrap();
        repo.git(&["init", "--quiet"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "core.autocrlf", "false"]);

        copy_dir(Path::new(FIXTURE), &repo.vault_dir());
        let output = FakePath::with_qmd().kbx(&repo.vault_dir(), &["init"]);
        assert!(output.status.success(), "{}", stderr(&output));
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "--quiet", "--no-verify", "--message", "start"]);
        repo
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn vault_dir(&self) -> PathBuf {
        self.root().join(&self.vault)
    }

    fn hook(&self) -> PathBuf {
        self.root().join(".git/hooks/pre-commit")
    }

    fn command(&self, program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(self.root())
            .env("GIT_CONFIG_GLOBAL", self.config.path().join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PATH", path_with_kbx())
            .env_remove("KBX_LOG");
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git", args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            stderr(&output)
        );
        stdout(&output)
    }

    fn install(&self, args: &[&str]) -> Output {
        let mut command = self.command(env!("CARGO_BIN_EXE_kbx"), &["hook", "install"]);
        command.args(args).current_dir(self.vault_dir());
        command.output().unwrap()
    }

    fn installed(&self) -> String {
        let output = self.install(&[]);
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
    }

    /// Writes a file in the vault and stages it.
    fn stage(&self, relative: &str, text: &str) {
        let path = self.vault_dir().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        let path = path.to_str().unwrap();
        self.git(&["add", "--", path]);
    }

    fn commit(&self) -> Output {
        self.command("git", &["commit", "--message", "change"])
            .output()
            .unwrap()
    }

    fn commits(&self) -> usize {
        self.git(&["rev-list", "--count", "HEAD"])
            .trim()
            .parse()
            .unwrap()
    }
}

/// Everything a commit printed. Git sends a hook's stdout to stderr.
fn shown(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn assert_blocked(repo: &Repo, output: &Output) {
    assert!(!output.status.success(), "{}", shown(output));
    assert_eq!(repo.commits(), 1, "{}", shown(output));
}

fn assert_committed(repo: &Repo, output: &Output) {
    assert!(output.status.success(), "{}", shown(output));
    assert_eq!(repo.commits(), 2, "{}", shown(output));
}

#[test]
fn a_commit_with_a_note_failing_lint_is_blocked_and_shows_the_findings() {
    let repo = Repo::new();
    repo.installed();

    repo.stage("notes/bad.md", &failing());
    let output = repo.commit();

    assert_blocked(&repo, &output);
    let text = shown(&output);
    assert!(text.contains("notes/bad.md"), "{text}");
    assert!(text.contains("type.unknown"), "{text}");
}

#[test]
fn the_hook_lints_a_vault_whose_config_is_in_kbx() {
    let repo = Repo::new();
    let root = repo.vault_dir();
    fs::rename(root.join("kbx.toml"), root.join(".kbx/kbx.toml")).unwrap();
    repo.installed();

    repo.stage("notes/bad.md", &failing());

    assert_blocked(&repo, &repo.commit());
}

#[test]
fn a_commit_of_clean_notes_goes_through() {
    let repo = Repo::new();
    repo.installed();

    repo.stage("notes/good.md", &clean());

    assert_committed(&repo, &repo.commit());
}

#[test]
fn only_staged_notes_are_linted() {
    let repo = Repo::new();
    repo.installed();

    // The fixture vault is full of notes that fail; none of them are staged.
    fs::write(repo.vault_dir().join("notes-unstaged.md"), failing()).unwrap();
    repo.stage("notes/good.md", &clean());

    assert_committed(&repo, &repo.commit());
}

#[test]
fn a_commit_without_markdown_never_lints_the_whole_vault() {
    let repo = Repo::new();
    repo.installed();

    repo.stage("data.txt", "not a note");

    assert_committed(&repo, &repo.commit());
}

#[test]
fn a_commit_that_deletes_a_note_goes_through() {
    let repo = Repo::new();
    repo.installed();

    repo.git(&["rm", "--quiet", "lint/legacy-runbook.md"]);

    assert_committed(&repo, &repo.commit());
}

#[test]
fn awkward_file_names_are_linted() {
    for name in ["notes/two words.md", "notes/café.md", "notes/UPPER.MD"] {
        let repo = Repo::new();
        repo.installed();

        repo.stage(name, &failing());
        let output = repo.commit();

        assert_blocked(&repo, &output);
        assert!(
            shown(&output).contains("type.unknown"),
            "{}",
            shown(&output)
        );
    }
}

#[test]
fn a_vault_in_a_subfolder_is_linted_from_its_own_root() {
    let repo = Repo::with_vault_at("my notes/vault");
    repo.installed();

    repo.stage("notes/bad.md", &failing());
    let output = repo.commit();
    assert_blocked(&repo, &output);
    assert!(
        shown(&output).contains("notes/bad.md"),
        "{}",
        shown(&output)
    );

    repo.git(&["reset", "--quiet", "--hard"]);
    fs::write(repo.root().join("outside.md"), failing()).unwrap();
    repo.git(&["add", "outside.md"]);
    assert_committed(&repo, &repo.commit());
}

#[test]
fn a_commit_is_stopped_when_kbx_is_not_on_the_path() {
    let repo = Repo::new();
    repo.installed();

    repo.stage("notes/good.md", &clean());
    let output = repo
        .command("git", &["commit", "--message", "change"])
        .env("PATH", path_without_kbx())
        .output()
        .unwrap();

    assert_blocked(&repo, &output);
    assert!(shown(&output).contains("--no-verify"), "{}", shown(&output));
}

#[cfg(unix)]
#[test]
fn the_hook_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let repo = Repo::new();
    repo.installed();

    let mode = fs::metadata(repo.hook()).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111);
}

#[test]
fn installing_twice_changes_nothing() {
    let repo = Repo::new();
    repo.installed();
    let first = fs::read_to_string(repo.hook()).unwrap();

    repo.installed();

    assert_eq!(fs::read_to_string(repo.hook()).unwrap(), first);
}

#[test]
fn an_existing_shell_hook_keeps_running_and_the_lint_still_applies() {
    let repo = Repo::new();
    fs::create_dir_all(repo.hook().parent().unwrap()).unwrap();
    // The `exit 0` would make anything added after it dead code.
    let theirs = "#!/bin/sh\necho ran > theirs.txt\nexit 0\n";
    write_executable(&repo.hook(), theirs);

    repo.installed();

    let text = fs::read_to_string(repo.hook()).unwrap();
    assert!(text.starts_with("#!/bin/sh\n"), "{text}");
    assert!(text.contains("echo ran > theirs.txt\nexit 0\n"), "{text}");

    repo.stage("notes/bad.md", &failing());
    assert_blocked(&repo, &repo.commit());

    repo.git(&["reset", "--quiet", "--hard"]);
    repo.stage("notes/good.md", &clean());
    assert_committed(&repo, &repo.commit());
    assert!(repo.root().join("theirs.txt").exists());
}

#[test]
fn a_zsh_hook_can_still_find_kbx() {
    let repo = Repo::new();
    if repo.command("zsh", &["--version"]).output().is_err() {
        return;
    }
    fs::create_dir_all(repo.hook().parent().unwrap()).unwrap();
    write_executable(&repo.hook(), "#!/usr/bin/env zsh\n");
    repo.installed();

    repo.stage("notes/good.md", &clean());

    assert_committed(&repo, &repo.commit());
}

#[test]
fn reinstalling_over_a_merged_hook_keeps_one_block() {
    let repo = Repo::new();
    fs::create_dir_all(repo.hook().parent().unwrap()).unwrap();
    write_executable(&repo.hook(), "#!/bin/bash\necho theirs\n");

    repo.installed();
    let first = fs::read_to_string(repo.hook()).unwrap();
    repo.installed();

    assert_eq!(fs::read_to_string(repo.hook()).unwrap(), first);
    assert_eq!(first.matches("kbx lint").count(), 1, "{first}");
}

#[test]
fn a_hook_that_is_not_a_shell_script_is_refused() {
    let repo = Repo::new();
    fs::create_dir_all(repo.hook().parent().unwrap()).unwrap();
    let theirs = "#!/usr/bin/env python3\nprint('theirs')\n";
    write_executable(&repo.hook(), theirs);

    let output = repo.install(&[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("--force"), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(repo.hook()).unwrap(), theirs);
}

#[test]
fn force_replaces_an_existing_hook() {
    let repo = Repo::new();
    fs::create_dir_all(repo.hook().parent().unwrap()).unwrap();
    write_executable(&repo.hook(), "#!/usr/bin/env python3\nprint('theirs')\n");

    let output = repo.install(&["--force"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let text = fs::read_to_string(repo.hook()).unwrap();
    assert!(!text.contains("theirs"), "{text}");
    repo.stage("notes/bad.md", &failing());
    assert_blocked(&repo, &repo.commit());
}

#[test]
fn the_hook_goes_where_git_looks_for_hooks() {
    let repo = Repo::new();
    repo.git(&["config", "core.hooksPath", "shared-hooks"]);

    repo.installed();

    assert!(!repo.hook().exists());
    assert!(repo.root().join("shared-hooks/pre-commit").exists());
    repo.stage("notes/bad.md", &failing());
    assert_blocked(&repo, &repo.commit());
}

#[test]
fn install_prints_the_obsidian_recipe() {
    let repo = Repo::new();

    let text = repo.installed();

    assert!(text.contains("Shell commands"), "{text}");
    assert!(text.contains("File content modified"), "{text}");
    assert!(text.contains("kbx lint"), "{text}");
}

#[test]
fn install_needs_a_git_repository() {
    let dir = TempDir::new().unwrap();
    let path = FakePath::with_qmd();
    assert!(path.kbx(dir.path(), &["init"]).status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
        .args(["hook", "install"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("git repository"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn install_needs_a_project() {
    let dir = TempDir::new().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
        .args(["hook", "install"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("kbx init"), "{}", stderr(&output));
}
