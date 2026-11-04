use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Value;

use crate::graph::Graph;
use crate::ontology::{Ontology, OntologyError};

/// The config file's name.
pub const CONFIG_FILE: &str = "kbx.toml";

/// The ontology `kbx init` starts a vault with. A copy of
/// `schema/ontology.example.yaml`, because `cargo publish` packages only the
/// crate.
pub const EXAMPLE_ONTOLOGY: &str = include_str!("../assets/ontology.example.yaml");

const DEFAULT_ONTOLOGY: &str = ".kbx/ontology.yaml";
const GRAPH_FILE: &str = ".kbx/graph.sqlite";

/// The parts of `kbx.toml` that are read so far. Other keys are left for the
/// tasks that use them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Config {
    /// The vault root. `~` is the home folder, and a relative path starts at
    /// the folder holding `kbx.toml`.
    pub vault: String,
    /// The top-level folders that are ACL scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The ontology file. A relative path starts at the vault root.
    pub ontology: String,
    /// The search backend.
    pub search: Search,
    /// How to reach QMD.
    #[serde(default)]
    pub qmd: Qmd,
    /// How to judge candidates.
    #[serde(default)]
    pub judge: JudgeConfig,
}

/// The `[judge]` table of `kbx.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JudgeConfig {
    /// Backends to try, best first.
    #[serde(default = "default_judge_order")]
    pub order: Vec<String>,
    /// The `claude-cli` backend.
    #[serde(default)]
    pub claude: JudgeBackend,
    /// The `codex-cli` backend.
    #[serde(default)]
    pub codex: JudgeBackend,
    /// Folders pinned to one backend, by path from the vault root. Their notes
    /// are never shown to another.
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

impl Default for JudgeConfig {
    fn default() -> JudgeConfig {
        JudgeConfig {
            order: default_judge_order(),
            claude: JudgeBackend::default(),
            codex: JudgeBackend::default(),
            overrides: BTreeMap::new(),
        }
    }
}

/// One backend's settings under `[judge]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct JudgeBackend {
    /// The model to ask for. Unset means the backend's default.
    pub model: Option<String>,
}

fn default_judge_order() -> Vec<String> {
    ["claude-cli", "codex-cli", "ollama", "llama-server"]
        .map(String::from)
        .to_vec()
}

/// The `qmd` key of `kbx.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Qmd {
    /// The program to run.
    #[serde(default = "default_qmd_bin")]
    pub bin: String,
    /// The QMD collections to search. Empty means QMD's default ones.
    #[serde(default)]
    pub collections: Vec<String>,
}

impl Default for Qmd {
    fn default() -> Qmd {
        Qmd {
            bin: default_qmd_bin(),
            collections: Vec::new(),
        }
    }
}

fn default_qmd_bin() -> String {
    "qmd".to_string()
}

/// A search backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Search {
    /// QMD, shelled out to.
    Qmd,
}

/// A `kbx.toml` with its paths worked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// The file as written.
    pub config: Config,
    /// The vault root, canonicalized.
    pub vault: PathBuf,
    /// The ontology file.
    pub ontology: PathBuf,
    /// The graph database.
    pub graph: PathBuf,
}

/// What [`init`] created, as opposed to found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitReport {
    /// `kbx.toml` was written.
    pub config: bool,
    /// The example ontology was written.
    pub ontology: bool,
}

/// Why a project could not be set up or loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No `kbx.toml` here or above.
    #[error("no {CONFIG_FILE} here or in any parent folder; run `kbx init`")]
    NotInitialised,
    /// `--ontology` was passed but a `kbx.toml` already says which one to use.
    #[error("{CONFIG_FILE} already exists; edit its `ontology` key instead")]
    AlreadyInitialised,
    /// `kbx.toml` is not valid.
    #[error("{0}: {1}")]
    Toml(PathBuf, toml::de::Error),
    /// The ontology given to `kbx init` is not valid.
    #[error("{0}: {1}")]
    Ontology(PathBuf, OntologyError),
    /// A file or folder could not be read or written.
    #[error("{0}: {1}")]
    Io(PathBuf, io::Error),
    /// The database could not be created.
    #[error("{0}: {1}")]
    Database(PathBuf, rusqlite::Error),
}

impl Project {
    /// Finds `kbx.toml` in `start` or the nearest parent folder that has one.
    pub fn discover(start: &Path) -> Result<Project, ConfigError> {
        let config = start
            .ancestors()
            .map(|dir| dir.join(CONFIG_FILE))
            .find(|path| path.is_file())
            .ok_or(ConfigError::NotInitialised)?;
        Project::load(&config)
    }

    /// Reads a `kbx.toml`.
    pub fn load(path: &Path) -> Result<Project, ConfigError> {
        let text = fs::read_to_string(path).map_err(|e| ConfigError::Io(path.into(), e))?;
        let config: Config =
            toml::from_str(&text).map_err(|e| ConfigError::Toml(path.into(), e))?;
        let base = path.parent().unwrap_or(Path::new("."));
        let vault = base.join(expand_home(&config.vault));
        let vault = vault
            .canonicalize()
            .map_err(|e| ConfigError::Io(vault, e))?;
        Ok(Project {
            ontology: vault.join(expand_home(&config.ontology)),
            graph: vault.join(GRAPH_FILE),
            vault,
            config,
        })
    }
}

/// Sets up `dir` as a vault: `.kbx/`, `kbx.toml`, the ontology and the
/// database. Never overwrites a file, so running it again changes nothing.
/// `ontology` is used where it is, relative to `dir`, rather than copied.
pub fn init(dir: &Path, ontology: Option<&Path>) -> Result<InitReport, ConfigError> {
    let config_path = dir.join(CONFIG_FILE);
    let exists = config_path.is_file();
    if exists && ontology.is_some() {
        return Err(ConfigError::AlreadyInitialised);
    }
    if let Some(path) = ontology {
        let full = dir.join(path);
        let yaml = fs::read_to_string(&full).map_err(|e| ConfigError::Io(full.clone(), e))?;
        Ontology::parse(&yaml).map_err(|e| ConfigError::Ontology(full, e))?;
    }

    let kbx_dir = dir.join(".kbx");
    fs::create_dir_all(&kbx_dir).map_err(|e| ConfigError::Io(kbx_dir, e))?;
    let mut report = InitReport {
        config: false,
        ontology: false,
    };
    if !exists {
        let name = ontology.map_or(DEFAULT_ONTOLOGY.into(), |path| path.to_string_lossy());
        let text = config_text(&name, &top_level_folders(dir)?);
        fs::write(&config_path, text).map_err(|e| ConfigError::Io(config_path.clone(), e))?;
        report.config = true;

        let example = dir.join(DEFAULT_ONTOLOGY);
        if ontology.is_none() && !example.exists() {
            fs::write(&example, EXAMPLE_ONTOLOGY).map_err(|e| ConfigError::Io(example, e))?;
            report.ontology = true;
        }
    }

    let project = Project::load(&config_path)?;
    Graph::open(&project.graph).map_err(|e| ConfigError::Database(project.graph, e))?;
    Ok(report)
}

/// The §10 layout. Written by hand to keep its order and spacing, which
/// `toml` serialization would lose.
fn config_text(ontology: &str, scopes: &[String]) -> String {
    let scopes = Value::from(scopes.to_vec());
    let ontology = Value::from(ontology);
    format!(
        r#"vault = "."
scopes = {scopes}  # top-level folders = ACL scopes
ontology = {ontology}
search = "qmd"
qmd = {{ bin = "qmd", collections = {scopes} }}

[judge]
order = ["claude-cli", "codex-cli", "ollama", "llama-server"]
batch = 20
claude = {{ model = "haiku" }}
codex  = {{ model = "gpt-6-luna" }}
ollama = {{ model = "qwen3:14b", host = "http://127.0.0.1:11434" }}

[judge.overrides]
"#
    )
}

/// The folders directly in `dir`, without hidden ones such as `.obsidian`.
fn top_level_folders(dir: &Path) -> Result<Vec<String>, ConfigError> {
    let entries = fs::read_dir(dir).map_err(|e| ConfigError::Io(dir.into(), e))?;
    let mut folders = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ConfigError::Io(dir.into(), e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !name.starts_with('.') {
            folders.push(name);
        }
    }
    folders.sort();
    Ok(folders)
}

fn expand_home(path: &str) -> PathBuf {
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}
