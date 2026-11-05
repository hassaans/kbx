use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use toml::Value;
use toml_edit::{Array, DocumentMut, InlineTable, Key, value};

use crate::exclude::{DEFAULT_EXCLUDE, ExcludeError, Excluded, Exclusions};
use crate::graph::Graph;
use crate::infer::{DEFAULT_CLASS, TypeDefaults};
use crate::ontology::{Ontology, OntologyError};
use crate::path::canonical_path;

/// The config file's name.
pub const CONFIG_FILE: &str = "kbx.toml";

/// The ontology `kbx init` starts a vault with. A copy of
/// `schema/ontology.example.yaml`, because `cargo publish` packages only the
/// crate.
pub const EXAMPLE_ONTOLOGY: &str = include_str!("../assets/ontology.example.yaml");

/// The folder kbx keeps its own files in, at the vault root.
const KBX_DIR: &str = ".kbx";

const DEFAULT_ONTOLOGY: &str = ".kbx/ontology.yaml";
const GRAPH_FILE: &str = ".kbx/graph.sqlite";

/// The parts of `kbx.toml` that are read so far. Other keys are left for the
/// tasks that use them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    /// The vault root. `~` is the home folder, and a relative path starts at
    /// the folder holding `kbx.toml`, or at the one holding `.kbx/` if the file
    /// is in there.
    pub vault: String,
    /// The top-level folders that are ACL scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// What kbx leaves alone, as `.gitignore` patterns from the vault root.
    /// Use [`exclude`](Config::exclude), which says what a file without this
    /// key means.
    #[serde(default)]
    pub exclude: Option<Vec<String>>,
    /// The class of a note with no `type` key, unless its scope has its own.
    #[serde(default = "default_type")]
    pub default_type: String,
    /// What's set per scope, by folder.
    #[serde(default)]
    pub scope: BTreeMap<String, ScopeConfig>,
    /// The ontology file. A relative path starts at the vault root.
    pub ontology: String,
    /// The search backend. Use [`backend`](Config::backend), which says what
    /// a file without this key means.
    #[serde(default)]
    pub search: Option<Search>,
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

impl Config {
    /// What kbx leaves alone. A file from before `exclude` gets the defaults.
    pub fn exclude(&self) -> Vec<String> {
        self.exclude.clone().unwrap_or_else(default_exclude)
    }

    /// The classes of notes with no `type` key.
    pub fn type_defaults(&self) -> TypeDefaults {
        let scopes = self.scope.iter().filter_map(|(folder, scope)| {
            let class = scope.default_type.clone()?;
            Some((folder.clone(), class))
        });
        TypeDefaults {
            global: self.default_type.clone(),
            scopes: scopes.collect(),
        }
    }

    /// The search backend in use. `kbx init` always writes the key, so a file
    /// without one was written by hand: if it lists qmd collections it was set
    /// up for QMD, and stays there. Anything else gets the built-in engine.
    pub fn backend(&self) -> Search {
        let for_qmd = !self.qmd.collections.is_empty() || !self.qmd.roots.is_empty();
        self.search.unwrap_or(match for_qmd {
            true => Search::Qmd,
            false => Search::Builtin,
        })
    }
}

fn default_exclude() -> Vec<String> {
    DEFAULT_EXCLUDE.map(String::from).to_vec()
}

fn default_type() -> String {
    DEFAULT_CLASS.to_string()
}

/// One folder's table under `[scope]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ScopeConfig {
    /// The class of a note here with no `type` key. Unset means the global one.
    pub default_type: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JudgeConfig {
    /// Backends to try, best first.
    #[serde(default = "default_judge_order")]
    pub order: Vec<String>,
    /// Pairs per call, unless `--batch` says otherwise.
    #[serde(default = "default_judge_batch")]
    pub batch: usize,
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
            batch: default_judge_batch(),
            claude: JudgeBackend::default(),
            codex: JudgeBackend::default(),
            ollama: JudgeBackend::default(),
            llama_server: JudgeBackend::default(),
            overrides: BTreeMap::new(),
        }
    }
}

/// One backend's settings under `[judge]`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct JudgeBackend {
    /// The model to ask for. Unset means the backend's default.
    pub model: Option<String>,
    /// Where a local backend listens. Unset means its usual address.
    pub host: Option<String>,
    /// What the model costs. Unset means the backend's default.
    pub rates: Option<Rates>,
}

/// A model's prices, in USD per million tokens. Every field is required: one
/// left out would price those tokens at nothing.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rates {
    /// Input tokens the provider had to read.
    pub input: f64,
    /// Input tokens served from the provider's cache.
    pub cached_input: f64,
    /// Output tokens.
    pub output: f64,
}

impl Rates {
    /// The price of a call. `input` includes `cached`, as `input_tokens` does.
    pub fn price(&self, input: i64, cached: i64, output: i64) -> f64 {
        let fresh = (input - cached).max(0) as f64;
        let total = fresh * self.input + cached as f64 * self.cached_input;
        (total + output as f64 * self.output) / 1_000_000.0
    }
}

fn default_judge_batch() -> usize {
    20
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
    /// BM25 and a local embedding model, inside kbx.
    Builtin,
}

/// A `kbx.toml` with its paths worked out.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// The file as written.
    pub config: Config,
    /// Where it was read from.
    pub config_file: PathBuf,
    /// The vault root, from [`canonical_path`].
    pub vault: PathBuf,
    /// The ontology file.
    pub ontology: PathBuf,
    /// The graph database.
    pub graph: PathBuf,
}

/// Where [`init`] puts a new `kbx.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLocation {
    /// `kbx.toml`, next to the notes.
    Root,
    /// `.kbx/kbx.toml`, which leaves the vault's root as it was.
    KbxFolder,
}

impl ConfigLocation {
    fn path(self, dir: &Path) -> PathBuf {
        match self {
            ConfigLocation::Root => dir.join(CONFIG_FILE),
            ConfigLocation::KbxFolder => dir.join(KBX_DIR).join(CONFIG_FILE),
        }
    }
}

/// What [`init`] created, as opposed to found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// `kbx.toml` was written.
    pub config: bool,
    /// The example ontology was written.
    pub ontology: bool,
    /// The folders that a new `kbx.toml` didn't make scopes of, and why.
    pub skipped: Vec<(String, Excluded)>,
}

/// Why a project could not be set up or loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No `kbx.toml` here or above.
    #[error(
        "no {CONFIG_FILE} or {KBX_DIR}/{CONFIG_FILE} here or in any parent folder; run `kbx init`"
    )]
    NotInitialised,
    /// A folder has both config files, and neither can be said to win.
    #[error("two config files: {0} and {1}; keep one")]
    TwoConfigs(PathBuf, PathBuf),
    /// `kbx init --in-kbx` found a `kbx.toml` at the root, and a second would
    /// be [`ConfigError::TwoConfigs`].
    #[error("{0} already exists; move it to {KBX_DIR}/{CONFIG_FILE} instead")]
    ConfigAtRoot(PathBuf),
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
    /// `exclude` could not be read.
    #[error(transparent)]
    Exclude(#[from] ExcludeError),
}

impl Project {
    /// Finds the config file of `start` or the nearest parent folder that has
    /// one. `.kbx/` itself is passed over: what's in it belongs to its parent,
    /// which also has to be checked for a second file.
    pub fn discover(start: &Path) -> Result<Project, ConfigError> {
        let vaults = start.ancestors();
        for dir in vaults.filter(|dir| dir.file_name() != Some(OsStr::new(KBX_DIR))) {
            if let Some(config) = config_in(dir)? {
                return Project::load(&config);
            }
        }
        Err(ConfigError::NotInitialised)
    }

    /// Reads a `kbx.toml`.
    pub fn load(path: &Path) -> Result<Project, ConfigError> {
        let text = fs::read_to_string(path).map_err(|e| ConfigError::Io(path.into(), e))?;
        let config: Config =
            toml::from_str(&text).map_err(|e| ConfigError::Toml(path.into(), e))?;
        let vault = config_base(path).join(expand_home(&config.vault));
        let vault = canonical_path(&vault).map_err(|e| ConfigError::Io(vault, e))?;
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

    /// What this vault leaves alone, as of now.
    pub fn exclusions(&self) -> Result<Exclusions, ExcludeError> {
        Exclusions::new(&self.vault, &self.config.exclude())
    }

    /// The QMD settings once every scope has a collection and nothing excluded
    /// has one.
    pub fn planned_qmd(&self) -> Result<Qmd, ConfigError> {
        let exclusions = self.exclusions()?;
        let allowed = |folder: &String| exclusions.why(folder, true).is_none();
        let mut scopes = self.config.scopes.clone();
        scopes.retain(allowed);
        let mut qmd = self.config.qmd.clone();
        let roots = &self.config.qmd.roots;
        qmd.collections
            .retain(|name| roots.get(name).is_none_or(allowed));
        qmd.roots.retain(|_, root| allowed(root));
        Ok(qmd.with_scopes(&scopes))
    }

    /// The collections `kbx suggest` searches: [`Qmd::searched`], without any
    /// that `exclusions` has since ruled out.
    pub fn searched(&self, exclusions: &Exclusions) -> BTreeMap<String, String> {
        let mut roots = self.config.qmd.searched();
        roots.retain(|_, root| exclusions.why(root, true).is_none());
        roots
    }

    /// Records [`planned_qmd`](Project::planned_qmd) in `kbx.toml`, keeping
    /// the rest of the file as it is. Returns whether anything changed.
    pub fn record_collections(&mut self) -> Result<bool, ConfigError> {
        let planned = self.planned_qmd()?;
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

/// The config file of `dir`: `.kbx/kbx.toml`, or else `kbx.toml`.
fn config_in(dir: &Path) -> Result<Option<PathBuf>, ConfigError> {
    let inside = ConfigLocation::KbxFolder.path(dir);
    let root = ConfigLocation::Root.path(dir);
    match (inside.is_file(), root.is_file()) {
        (true, true) => Err(ConfigError::TwoConfigs(inside, root)),
        (true, false) => Ok(Some(inside)),
        (false, true) => Ok(Some(root)),
        (false, false) => Ok(None),
    }
}

/// The folder a config file's relative paths start at. The file means the same
/// in `.kbx/` as at the root, so moving it there takes no edit.
fn config_base(path: &Path) -> &Path {
    let folder = path.parent().unwrap_or(Path::new("."));
    match folder.file_name() == Some(OsStr::new(KBX_DIR)) {
        true => folder.parent().unwrap_or(folder),
        false => folder,
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
/// `ontology` is used where it is, relative to `dir`, rather than copied, and
/// `search` is what a new `kbx.toml` says. `location` says where a new one goes:
/// one already in `.kbx/` is used whatever it says.
pub fn init(
    dir: &Path,
    ontology: Option<&Path>,
    search: Search,
    location: ConfigLocation,
) -> Result<InitReport, ConfigError> {
    let found = config_in(dir)?;
    let root = ConfigLocation::Root.path(dir);
    if location == ConfigLocation::KbxFolder && found.as_ref() == Some(&root) {
        return Err(ConfigError::ConfigAtRoot(root));
    }
    let exists = found.is_some();
    let config_path = found.unwrap_or_else(|| location.path(dir));
    if exists && ontology.is_some() {
        return Err(ConfigError::AlreadyInitialised);
    }
    if let Some(path) = ontology {
        let full = dir.join(path);
        let yaml = fs::read_to_string(&full).map_err(|e| ConfigError::Io(full.clone(), e))?;
        Ontology::parse(&yaml).map_err(|e| ConfigError::Ontology(full, e))?;
    }

    let kbx_dir = dir.join(KBX_DIR);
    fs::create_dir_all(&kbx_dir).map_err(|e| ConfigError::Io(kbx_dir, e))?;
    let mut report = InitReport {
        config: false,
        ontology: false,
        skipped: Vec::new(),
    };
    if !exists {
        let name = ontology.map_or(DEFAULT_ONTOLOGY.into(), |path| path.to_string_lossy());
        let exclude = default_exclude();
        let exclusions = Exclusions::new(dir, &exclude)?;
        let mut scopes = Vec::new();
        for folder in top_level_folders(dir)? {
            match exclusions.why(&folder, true) {
                Some(why) => report.skipped.push((folder, why)),
                None => scopes.push(folder),
            }
        }
        let text = config_text(&name, &scopes, &exclude, search);
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
fn config_text(ontology: &str, scopes: &[String], exclude: &[String], search: Search) -> String {
    let qmd = Qmd::default().with_scopes(scopes);
    let scope_table = scope_text(scopes);
    let default_type = Value::from(DEFAULT_CLASS);
    let collections = name_list(&qmd.collections);
    let roots = root_table(&qmd.roots);
    let scopes = Value::from(scopes.to_vec());
    let exclude = Value::from(exclude.to_vec());
    let ontology = Value::from(ontology);
    let search = match search {
        Search::Qmd => "qmd",
        Search::Builtin => "builtin",
    };
    format!(
        r#"vault = "."
scopes = {scopes}  # top-level folders = ACL scopes
# Never read, indexed or shown to a judge, like .gitignore and hidden folders.
# .gitignore patterns from the vault root; see docs/excluding.md.
exclude = {exclude}
ontology = {ontology}
search = "{search}"  # or "qmd" or "builtin"
qmd = {{ bin = "qmd", collections = {collections}, roots = {roots} }}
default_type = {default_type}  # the class of a note with no `type` key
{scope_table}
[judge]
order = ["claude-cli", "codex-cli", "ollama", "llama-server"]
batch = 20
# Budgets price Codex's tokens at `rates`, in USD per million tokens. To set them:
# codex = {{ model = "gpt-6-luna", rates = {{ input = 0.10, cached_input = 0.01, output = 0.50 }} }}
claude = {{ model = "haiku" }}
codex  = {{ model = "gpt-6-luna" }}
ollama = {{ model = "qwen3:14b", host = "http://127.0.0.1:11434" }}
llama_server = {{ host = "http://127.0.0.1:8080" }}

[judge.overrides]
"#
    )
}

/// A `[scope]` table giving each scope the global class, for editing later.
fn scope_text(scopes: &[String]) -> String {
    if scopes.is_empty() {
        return String::new();
    }
    let class = Value::from(DEFAULT_CLASS);
    let mut text = "\n[scope]  # per-scope `default_type`\n".to_string();
    for scope in scopes {
        let key = Key::new(scope.as_str());
        text.push_str(&format!("{key} = {{ default_type = {class} }}\n"));
    }
    text
}

/// The folders directly in `dir`, but for the one kbx made.
fn top_level_folders(dir: &Path) -> Result<Vec<String>, ConfigError> {
    let entries = fs::read_dir(dir).map_err(|e| ConfigError::Io(dir.into(), e))?;
    let mut folders = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ConfigError::Io(dir.into(), e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && name != ".kbx" {
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
