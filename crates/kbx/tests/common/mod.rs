//! What the integration tests share. Each test file compiles this separately
//! and uses only some of it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

pub const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");

pub fn id(n: u32) -> String {
    format!("01KBX{n:021}")
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Writes a script that is run straight away. On Linux, exec fails with
/// ETXTBSY while anything holds the file open for writing, and a child that
/// another test thread has just forked holds a copy of this process's
/// descriptors until it execs. So this process never opens the file for
/// writing: `cp` does, and has exited by the time this returns.
#[cfg(unix)]
pub fn write_executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    use tempfile::NamedTempFile;

    let staged = NamedTempFile::new().unwrap();
    fs::write(staged.path(), text).unwrap();
    let copied = Command::new("/bin/cp")
        .arg(staged.path())
        .arg(path)
        .status()
        .unwrap();
    assert!(copied.success(), "cp to {}: {copied}", path.display());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
pub fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A folder that stands in for `PATH`, so that no test can reach a real `npm`.
pub struct FakePath {
    dir: TempDir,
}

impl FakePath {
    /// Nothing on it, as on a machine without Node.
    pub fn empty() -> FakePath {
        FakePath {
            dir: TempDir::new().unwrap(),
        }
    }

    /// `qmd` is already installed, so nothing tries to install it.
    pub fn with_qmd() -> FakePath {
        let path = FakePath::empty();
        path.tool("qmd", "");
        path
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The file a command named `name` runs from.
    pub fn program(&self, name: &str) -> PathBuf {
        let file = if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.to_string()
        };
        self.dir().join(file)
    }

    /// Puts a command on the path. `script` is `sh` on Unix and batch on
    /// Windows.
    pub fn tool(&self, name: &str, script: &str) {
        let program = self.program(name);
        let text = if cfg!(windows) {
            format!("@echo off\r\n{script}\r\n")
        } else {
            format!("#!/bin/sh\n{script}\n")
        };
        write_executable(&program, &text);
    }

    /// Runs kbx in `dir` with this as its whole `PATH`.
    pub fn kbx(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kbx"))
            .args(args)
            .current_dir(dir)
            .env("PATH", self.dir())
            .env_remove("KBX_LOG")
            .env_remove("KBX_CLAUDE_MODEL")
            .env_remove("KBX_CODEX_MODEL")
            .output()
            .expect("kbx should run")
    }
}

/// A copy of the fixture vault after `kbx init`.
pub struct Fixture {
    dir: TempDir,
    pub path: FakePath,
}

impl Fixture {
    pub fn new() -> Fixture {
        let fixture = Fixture {
            dir: TempDir::new().unwrap(),
            path: FakePath::with_qmd(),
        };
        copy_dir(Path::new(FIXTURE), fixture.root());
        fixture.ok(&["init"]);
        fixture
    }

    /// An empty folder, before `kbx init`, with `path` for its `PATH`.
    pub fn bare(path: FakePath) -> Fixture {
        Fixture {
            dir: TempDir::new().unwrap(),
            path,
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn kbx(&self, args: &[&str]) -> Output {
        self.path.kbx(self.root(), args)
    }

    /// Runs kbx, requires success, and returns stdout.
    pub fn ok(&self, args: &[&str]) -> String {
        let output = self.kbx(args);
        assert!(
            output.status.success(),
            "kbx {args:?} failed: {}",
            stderr(&output)
        );
        stdout(&output)
    }

    pub fn write(&self, relative: &str, text: &str) {
        let path = self.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}
