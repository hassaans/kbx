//! What the integration tests share. Each test file compiles this separately
//! and uses only some of it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use tempfile::TempDir;
use tiny_http::{Response, Server};

pub const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");

/// An address nothing listens on.
pub const CLOSED_PORT: &str = "http://127.0.0.1:9";

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

/// An HTTP server on a free port standing in for a local model. `reply` gets
/// each request's path and body and returns a status and a body.
pub struct FakeModel {
    pub url: String,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

impl FakeModel {
    pub fn start(reply: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> FakeModel {
        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let path = request.url().to_string();
                let (status, text) = reply(&path, &body);
                log.lock().unwrap().push((path, body));
                let _ = request.respond(Response::from_string(text).with_status_code(status));
            }
        });
        FakeModel {
            url: format!("http://127.0.0.1:{port}"),
            seen,
        }
    }

    /// The bodies of the requests to `path`, in order.
    pub fn bodies(&self, path: &str) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        let matching = seen.iter().filter(|(to, _)| to == path);
        matching.map(|(_, body)| body.clone()).collect()
    }
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
        self.kbx_with(dir, args, &[])
    }

    /// Likewise, with `env` set. Unless it says otherwise the local backends
    /// point at a closed port, so that no test can reach a real model.
    pub fn kbx_with(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kbx"))
            .args(args)
            .current_dir(dir)
            .env("PATH", self.dir())
            .env_remove("KBX_LOG")
            .env_remove("KBX_CLAUDE_MODEL")
            .env_remove("KBX_CODEX_MODEL")
            .env_remove("KBX_OLLAMA_MODEL")
            .env_remove("KBX_LLAMA_SERVER_MODEL")
            .env("OLLAMA_HOST", CLOSED_PORT)
            .env("KBX_LLAMA_SERVER_HOST", CLOSED_PORT)
            .envs(env.iter().copied())
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
        fixture.ok(&["init", "--search", "qmd"]);
        fixture
    }

    /// An empty folder, before `kbx init`, with `path` for its `PATH`.
    pub fn bare(path: FakePath) -> Fixture {
        Fixture {
            dir: TempDir::new().unwrap(),
            path,
        }
    }

    /// This vault with `path` for its `PATH` from now on.
    pub fn with_path(self, path: FakePath) -> Fixture {
        Fixture { path, ..self }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn kbx(&self, args: &[&str]) -> Output {
        self.path.kbx(self.root(), args)
    }

    pub fn kbx_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.path.kbx_with(self.root(), args, env)
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
