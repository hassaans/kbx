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
        fs::write(&program, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        }
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
