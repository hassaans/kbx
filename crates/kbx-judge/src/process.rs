use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

/// How long a login check or probe may take. A logged-out `codex exec` spends
/// about 40 s on retries before it fails.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long one batch may take.
pub(crate) const BATCH_TIMEOUT: Duration = Duration::from_secs(600);

/// The longest backend message worth repeating.
const BRIEF_CHARS: usize = 200;

/// What a finished program left behind.
pub(crate) struct Finished {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Why a program didn't finish.
pub(crate) enum RunError {
    Spawn(io::Error),
    TimedOut,
}

/// Where a backend runs: an empty folder, so that no project instructions,
/// hooks or MCP config are picked up, beside the files its flags point at.
/// Those hold the stable prefix and the schema and never a note, which goes
/// over stdin.
pub(crate) struct Workspace {
    dir: TempDir,
}

impl Workspace {
    pub fn new(system: &str, schema: &str) -> io::Result<Workspace> {
        let workspace = Workspace {
            dir: TempDir::new()?,
        };
        fs::create_dir(workspace.cwd())?;
        fs::write(workspace.system_file(), system)?;
        fs::write(workspace.schema_file(), schema)?;
        Ok(workspace)
    }

    pub fn cwd(&self) -> PathBuf {
        self.dir.path().join("empty")
    }

    pub fn system_file(&self) -> PathBuf {
        self.dir.path().join("system.md")
    }

    pub fn schema_file(&self) -> PathBuf {
        self.dir.path().join("schema.json")
    }
}

/// Runs `program` in `cwd`, feeding it `input`. Killed if it runs over.
pub(crate) async fn run<S: AsRef<OsStr>>(
    program: &Path,
    args: impl IntoIterator<Item = S>,
    cwd: &Path,
    input: &[u8],
    limit: Duration,
) -> Result<Finished, RunError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        // Otherwise `claude` leaves a `memory` folder under `~/.claude` for
        // each temp dir it's run in.
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let mut stdin = child.stdin.take();
    let feed = async {
        if let Some(stdin) = &mut stdin {
            // A program that exits without reading all of it says why on its
            // own, so a broken pipe here adds nothing.
            let _ = stdin.write_all(input).await;
            let _ = stdin.shutdown().await;
        }
        drop(stdin);
    };
    // Together, or a full stdout pipe could stall a program still being fed.
    let finish = async { tokio::join!(feed, child.wait_with_output()).1 };
    let output = timeout(limit, finish)
        .await
        .map_err(|_| RunError::TimedOut)?
        .map_err(RunError::Spawn)?;
    Ok(Finished {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Whether a backend's error message means its login doesn't work.
pub(crate) fn means_logged_out(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "not logged in",
        "authentication_failed",
        "unauthorized",
        "invalid api key",
        "/login",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

/// The first line of a backend's message, kept short.
pub(crate) fn brief(message: &str) -> String {
    let line = message.lines().map(str::trim).find(|l| !l.is_empty());
    line.unwrap_or("no message")
        .chars()
        .take(BRIEF_CHARS)
        .collect()
}
