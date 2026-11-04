//! `kbx hook install`: the git pre-commit hook and the Obsidian recipe (§8).

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use kbx_core::Project;

const START: &str = "# >>> kbx >>>";
const END: &str = "# <<< kbx <<<";

/// The lint, in a subshell so that it leaves a script it's added to alone.
/// `@PREFIX@` is the vault's folder in the repository. It reads the files on
/// disk, so a file staged and then edited is linted as edited.
const BLOCK: &str = r#"# >>> kbx >>>
# From `kbx hook install`, which rewrites these lines when run again.
(
  prefix=@PREFIX@
  if ! command -v kbx >/dev/null 2>&1; then
    echo "kbx is not on PATH, so staged notes can't be linted; commit with --no-verify to skip this" >&2
    exit 1
  fi
  staged=$(git diff --cached --name-only -z --diff-filter=ACMR --relative="$prefix" | tr '\0' '\n')
  set --
  while IFS= read -r path; do
    case "$path" in
      *.[mM][dD]) [ -f "./$prefix$path" ] && set -- "$@" "$path" ;;
    esac
  done <<KBX_STAGED
$staged
KBX_STAGED
  [ "$#" -eq 0 ] && exit 0
  cd "./$prefix" && exec kbx lint -- "$@"
) || exit 1
# <<< kbx <<<
"#;

const RECIPE: &str = r#"
To lint from Obsidian as well, with the Shell commands community plugin:

  1. Install and enable "Shell commands".
  2. Add a shell command:
       kbx index {{event_file_path:relative}} && kbx lint {{event_file_path:relative}}
     If Obsidian can't find kbx, give its full path.
  3. Under the command's Events, turn on "File content modified".
  4. Show its output in a notification, to see the findings as you write.

Obsidian saves often, and the command runs on each save. `kbx watch`, left
running in a terminal, does the same with a debounce."#;

pub fn install(force: bool) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    // The hook runs kbx from the vault root, so kbx.toml has to be found there.
    if !Project::discover(&project.vault).is_ok_and(|found| found.vault == project.vault) {
        return Err(format!(
            "the hook runs in {}, where no kbx.toml leads back to this vault",
            project.vault.display()
        )
        .into());
    }
    let repository = Repository::of(&project.vault)?;
    let block = BLOCK.replace("@PREFIX@", &quote(&repository.prefix));
    let hook = repository.hooks.join("pre-commit");
    let existing = match fs::read(&hook) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", hook.display()).into()),
    };
    let existing = existing.filter(|_| !force);
    let script = match &existing {
        None => format!("#!/bin/sh\n{block}"),
        Some(bytes) => merge(bytes, &block).map_err(|why| {
            format!(
                "{} {why}; use --force to replace it, or add `kbx lint` to it yourself",
                hook.display()
            )
        })?,
    };

    fs::create_dir_all(&repository.hooks)
        .map_err(|error| format!("{}: {error}", repository.hooks.display()))?;
    fs::write(&hook, script).map_err(|error| format!("{}: {error}", hook.display()))?;
    make_executable(&hook)?;
    println!("installed the pre-commit hook at {}", hook.display());
    println!("{RECIPE}");
    Ok(ExitCode::SUCCESS)
}

/// Where a vault sits in git.
struct Repository {
    /// The folder git runs hooks from.
    hooks: PathBuf,
    /// The vault from the repository root, with `/` separators and a trailing
    /// one, empty if it's the root.
    prefix: String,
}

impl Repository {
    fn of(vault: &Path) -> Result<Repository, Box<dyn Error>> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel", "--git-path", "hooks"])
            .current_dir(vault)
            .output()
            .map_err(|error| format!("can't run git: {error}"))?;
        if !output.status.success() {
            return Err(format!("{} is not in a git repository", vault.display()).into());
        }
        let text = String::from_utf8(output.stdout)?;
        let mut lines = text.lines();
        let (Some(root), Some(hooks)) = (lines.next(), lines.next()) else {
            return Err(format!("{} is not in a git work tree", vault.display()).into());
        };
        let root = Path::new(root).canonicalize()?;
        let relative = vault.strip_prefix(&root)?;
        let prefix = relative
            .components()
            .map(|part| format!("{}/", part.as_os_str().to_string_lossy()))
            .collect();
        Ok(Repository {
            hooks: vault.join(hooks),
            prefix,
        })
    }
}

/// Puts `block` into a hook that's already there. It goes straight after the
/// shebang: at the end, an `exit 0` or `exec` would mean it never ran.
fn merge(existing: &[u8], block: &str) -> Result<String, &'static str> {
    let text = std::str::from_utf8(existing).map_err(|_| "is not a shell script")?;
    if let (Some(start), Some(end)) = (text.find(START), text.find(END))
        && start < end
    {
        let end = end + END.len();
        let end = end + usize::from(text[end..].starts_with('\n'));
        return Ok(format!("{}{block}{}", &text[..start], &text[end..]));
    }
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    if !is_shell(first) {
        return Err("is not a shell script");
    }
    Ok(format!("{first}\n{block}{rest}"))
}

fn is_shell(shebang: &str) -> bool {
    let Some(command) = shebang.strip_prefix("#!") else {
        return false;
    };
    command.split_whitespace().any(|word| {
        let name = word.rsplit('/').next().unwrap_or(word);
        matches!(name, "sh" | "bash" | "dash" | "ksh" | "zsh")
    })
}

/// Quotes text for `sh`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("{}: {error}", path.display()).into())
}

/// Git for Windows runs a hook whatever its permissions.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), Box<dyn Error>> {
    Ok(())
}
