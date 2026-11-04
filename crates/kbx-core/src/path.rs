use std::env;
use std::path::PathBuf;

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
