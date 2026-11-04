use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use toml::Value;
use toml_edit::{Array, DocumentMut, InlineTable, value};

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
    /// What accepting writes.
    #[serde(default)]
    pub write: WriteConfig,
}

/// The `[write]` table of `kbx.toml`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct WriteConfig {
    /// Propose section-level pairs and write their anchors into relation
    /// targets. Off by default: an anchored link breaks some tools, see
    /// `docs/sections.md`.
    #[serde(default)]
    pub section_anchors: bool,
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
    /// The `ollama` backend.
    #[serde(default)]
    pub ollama: JudgeBackend,
    /// The `llama-server` backend.
    #[serde(default)]
    pub llama_server: JudgeBackend,
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
            ollama: JudgeBackend::default(),
            llama_server: JudgeBackend::default(),
            overrides: BTreeMap::new(),
        }
    }
}

/// One backend's settings under `[judge]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct JudgeBackend {
    /// The model to ask for. Unset means the backend's default.
    pub model: Option<String>,
    /// Where a local backend listens. Unset means its usual address.
    pub host: Option<String>,
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
    /// The QMD index to use. Unset means one of the vault's own, which kbx
    /// keeps up to date; a named one is left to whoever named it.
    #[serde(default)]
    pub index: Option<String>,
    /// The QMD collections to search.
    #[serde(default)]
    pub collections: Vec<String>,
    /// Each collection's folder, relative to the vault root. A hit maps back
    /// to a note through this, so a collection without a root finds nothing.
    #[serde(default)]
    pub roots: BTreeMap<String, String>,
}

impl Qmd {
    /// The configured collections that have a root, with their roots.
    pub fn searched(&self) -> BTreeMap<String, String> {
        let rooted = self.collections.iter().filter_map(|name| {
            let root = self.roots.get(name)?;
            Some((name.clone(), root.clone()))
        });
        rooted.collect()
    }

    /// `self` with a collection for each of `scopes`, or for the whole vault
    /// if there are none. A list entry naming a scope without a root is what
    /// an older `kbx init` wrote, and gives way to the new collection.
    fn with_scopes(&self, scopes: &[String]) -> Qmd {
        let vault = [".".to_string()];
        let folders = if scopes.is_empty() {
            &vault[..]
        } else {
            scopes
        };
        let mut qmd = self.clone();
        let stale = |name: &String| scopes.contains(name) && !self.roots.contains_key(name);
        qmd.collections.retain(|name| !stale(name));
        for folder in folders {
            if !qmd.roots.values().any(|root| root == folder) {
                let name = unused_name(folder, &qmd);
                qmd.roots.insert(name, folder.clone());
            }
        }
        for name in qmd.roots.keys() {
            if !qmd.collections.contains(name) {
                qmd.collections.push(name.clone());
            }
        }
        qmd
    }
}

/// A collection name for `folder`: qmd puts it in a URL, so plain ASCII.
fn unused_name(folder: &str, qmd: &Qmd) -> String {
    let lower = folder.to_lowercase();
    let parts = lower.split(|c: char| !c.is_ascii_alphanumeric());
    let slug = parts
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "vault".to_string()
    } else {
        slug
    };
    let taken = |name: &String| qmd.roots.contains_key(name) || qmd.collections.contains(name);
    let numbered = (2..).map(|n| format!("{slug}-{n}"));
    std::iter::once(slug.clone())
        .chain(numbered)
        .find(|name| !taken(name))
        .unwrap_or(slug)
}

impl Default for Qmd {
    fn default() -> Qmd {
        Qmd {
            bin: default_qmd_bin(),
            index: None,
            collections: Vec::new(),
            roots: BTreeMap::new(),
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
    /// Where it was read from.
    pub config_file: PathBuf,
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
    /// `kbx.toml` could not be parsed for editing.
    #[error("{0}: {1}")]
    TomlEdit(PathBuf, toml_edit::TomlError),
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
            config_file: path.into(),
        })
    }

    /// The QMD index this vault uses. The vault's own is named after where it
    /// is, not in `kbx.toml`, so two clones of a vault never share one.
    pub fn qmd_index(&self) -> String {
        self.config.qmd.index.clone().unwrap_or_else(|| {
            let digest = Sha256::digest(self.vault.to_string_lossy().as_bytes());
            let hex: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
            format!("kbx-{hex}")
        })
    }

    /// The QMD settings once every scope has a collection.
    pub fn planned_qmd(&self) -> Qmd {
        self.config.qmd.with_scopes(&self.config.scopes)
    }

    /// Records [`planned_qmd`](Project::planned_qmd) in `kbx.toml`, keeping
    /// the rest of the file as it is. Returns whether anything changed.
    pub fn record_collections(&mut self) -> Result<bool, ConfigError> {
        let planned = self.planned_qmd();
        if planned == self.config.qmd {
            return Ok(false);
        }
        let path = &self.config_file;
        let text = fs::read_to_string(path).map_err(|e| ConfigError::Io(path.clone(), e))?;
        let mut document: DocumentMut = text
            .parse()
            .map_err(|e| ConfigError::TomlEdit(path.clone(), e))?;
        let entry = document.entry("qmd");
        let qmd = entry.or_insert_with(|| value(InlineTable::new()));
        if let Some(table) = qmd.as_table_like_mut() {
            table.insert("collections", value(name_list(&planned.collections)));
            table.insert("roots", value(root_table(&planned.roots)));
        }
        fs::write(path, document.to_string()).map_err(|e| ConfigError::Io(path.clone(), e))?;
        self.config.qmd = planned;
        Ok(true)
    }
}

fn name_list(names: &[String]) -> Array {
    names.iter().map(String::as_str).collect()
}

fn root_table(roots: &BTreeMap<String, String>) -> InlineTable {
    let mut table = InlineTable::new();
    for (name, root) in roots {
        table.insert(name, root.as_str().into());
    }
    table
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
    let qmd = Qmd::default().with_scopes(scopes);
    let collections = name_list(&qmd.collections);
    let roots = root_table(&qmd.roots);
    let scopes = Value::from(scopes.to_vec());
    let ontology = Value::from(ontology);
    format!(
        r#"vault = "."
scopes = {scopes}  # top-level folders = ACL scopes
ontology = {ontology}
search = "qmd"
qmd = {{ bin = "qmd", collections = {collections}, roots = {roots} }}

[judge]
order = ["claude-cli", "codex-cli", "ollama", "llama-server"]
batch = 20
claude = {{ model = "haiku" }}
codex  = {{ model = "gpt-6-luna" }}
ollama = {{ model = "qwen3:14b", host = "http://127.0.0.1:11434" }}
llama_server = {{ host = "http://127.0.0.1:8080" }}

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
