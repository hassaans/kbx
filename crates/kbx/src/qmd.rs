//! Finding QMD, and installing it when it's missing.

use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tracing::info;

const PACKAGE: &str = "@tobilu/qmd";
const INSTALL: &str = "npm i -g @tobilu/qmd";

/// Finds `qmd` on `PATH`, installing it first if it isn't there. Says nothing
/// on success. The error is the whole report for the user.
pub fn ensure() -> Result<PathBuf, String> {
    if let Some(path) = find_on_path("qmd") {
        return Ok(path);
    }
    let (Some(_), Some(npm)) = (find_on_path("node"), find_on_path("npm")) else {
        return Err(format!(
            "qmd is not installed, and installing it needs Node >= 22 and npm, \
             which are not on PATH. Install Node >= 22, then run:\n  {INSTALL}"
        ));
    };

    info!("installing qmd with {}", npm.display());
    // By the path that was found: on Windows npm is a `.cmd`, which a bare
    // `npm` wouldn't run. Nothing reads stdin, so nothing can prompt.
    let output = Command::new(&npm)
        .args(["i", "-g", PACKAGE])
        .stdin(Stdio::null())
        .output();
    let failure = match output {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "{}\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) => Some(error.to_string()),
    };
    if let Some(failure) = failure {
        return Err(format!(
            "`{INSTALL}` failed: {}\nTo install qmd by hand, run:\n  {INSTALL}",
            failure.trim_end()
        ));
    }
    find_on_path("qmd").ok_or_else(|| {
        format!(
            "`{INSTALL}` finished, but qmd is not on PATH. \
             Add the bin folder under `npm prefix -g` to PATH."
        )
    })
}

fn find_on_path(program: &str) -> Option<PathBuf> {
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
