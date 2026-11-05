//! `kbx doctor`: one row per check. A failed check doesn't stop the others.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use kbx_core::{
    ConfigError, Graph, IndexState, JudgeConfig, Ontology, Project, Search, index_state,
};
use kbx_gen::MODEL;
use kbx_judge::{Availability, JudgeId, backend};
use sysinfo::System;
use tokio::runtime;

use crate::qmd;

const GIB: f64 = (1u64 << 30) as f64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// Worth acting on, but nothing is broken.
    Warn,
    Fail,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        })
    }
}

type Check = (Status, String);

pub fn run(fix: bool) -> Result<ExitCode, Box<dyn Error>> {
    let mut healthy = true;
    let mut report = |name: &str, (status, detail): Check| {
        println!("{name:<14}{status:<5}{detail}");
        healthy &= status != Status::Fail;
    };

    let mut found = Project::discover(&env::current_dir()?);
    match &found {
        Ok(project) => {
            report("vault", (Status::Ok, project.vault.display().to_string()));
            let yaml = fs::read_to_string(&project.ontology).ok();
            let ontology = check_ontology(project, yaml.as_deref());
            // Staleness means nothing against a file `kbx index` would refuse.
            let usable = yaml.as_deref().filter(|_| ontology.0 == Status::Ok);
            let index = check_index(project, usable);
            report("ontology", ontology);
            report("exclude", check_exclude(project));
            report("index", index);
        }
        Err(ConfigError::NotInitialised) => {
            let detail = "no kbx.toml here or in any parent folder; run `kbx init`";
            report("vault", (Status::Warn, detail.to_string()));
        }
        Err(error) => report("vault", (Status::Fail, error.to_string())),
    }
    report("ram", check_ram());

    let project = found.as_ref().ok();
    let default = JudgeConfig::default();
    let config = project.map_or(&default, |project| &project.config.judge);
    let vault = project.map(|project| project.vault.as_path());
    for (name, check) in check_judges(config, vault)? {
        report(&name, check);
    }

    // Last, so that its report is the end of the output. Only a vault on QMD
    // has anything to do with qmd: it's Node, and nothing else needs it.
    let backend = found
        .as_ref()
        .map_or(Search::Builtin, |p| p.config.backend());
    report("search", check_search(found.as_ref().ok(), backend));
    if backend == Search::Qmd {
        match qmd::ensure() {
            Ok(path) => {
                report("qmd", (Status::Ok, path.display().to_string()));
                if let Ok(project) = &mut found {
                    report("collections", check_collections(project, fix));
                }
            }
            Err(error) => {
                report("qmd", (Status::Fail, "not installed".to_string()));
                eprintln!("error: {error}");
            }
        }
    }

    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn check_ontology(project: &Project, yaml: Option<&str>) -> Check {
    let path = project.ontology.display();
    let Some(yaml) = yaml else {
        return (Status::Fail, format!("{path} can't be read"));
    };
    match Ontology::parse(yaml) {
        Ok(ontology) => {
            let (classes, relations) = (ontology.classes.len(), ontology.relations.len());
            let detail = format!("{path}: {classes} classes, {relations} relations");
            (Status::Ok, detail)
        }
        Err(error) => (Status::Fail, format!("{path}: {error}")),
    }
}

fn check_index(project: &Project, ontology: Option<&str>) -> Check {
    if !project.graph.is_file() {
        return (Status::Warn, "not built; run `kbx index`".to_string());
    }
    let read = Graph::open(&project.graph).and_then(|graph| {
        let state = ontology.map(|yaml| index_state(&graph, yaml));
        Ok((graph.stats()?, state.transpose()?))
    });
    let (stats, state) = match read {
        Ok(found) => found,
        Err(error) => {
            return (
                Status::Fail,
                format!("{}: {error}", project.graph.display()),
            );
        }
    };
    let counts = format!(
        "{} notes, {} edges, {} sections, {} hidden links",
        stats.nodes, stats.edges, stats.sections, stats.hidden
    );
    match state {
        Some(IndexState::Unbuilt) => (Status::Warn, "not built; run `kbx index`".to_string()),
        Some(IndexState::Stale) => (Status::Warn, format!("{counts}; stale, run `kbx index`")),
        Some(IndexState::Current) | None => (Status::Ok, counts),
    }
}

/// Whether `exclude` says what it's taken to say. A scope that's excluded is
/// no scope, and a file with no list has the defaults, which it doesn't show.
fn check_exclude(project: &Project) -> Check {
    let exclusions = match project.exclusions() {
        Ok(exclusions) => exclusions,
        Err(error) => return (Status::Fail, error.to_string()),
    };
    let barred = project.config.scopes.iter();
    let barred: Vec<&str> = barred
        .filter(|scope| exclusions.why(scope, true).is_some())
        .map(String::as_str)
        .collect();
    let mut notes = Vec::new();
    if project.config.exclude.is_none() {
        let defaults = project.config.exclude().join(", ");
        notes.push(format!(
            "kbx.toml has no `exclude`, so the defaults apply: {defaults}"
        ));
    }
    if !barred.is_empty() {
        let scopes = barred.join(", ");
        notes.push(format!("excluded, so left out: scope {scopes}"));
    }
    if notes.is_empty() {
        let count = project.config.exclude().len();
        return (
            Status::Ok,
            format!("{count} patterns, and .gitignore files"),
        );
    }
    (Status::Warn, notes.join("; "))
}

/// A row per backend, whatever is chosen and whether or not `judge.order` has
/// it, then the one that would be chosen.
fn check_judges(
    config: &JudgeConfig,
    vault: Option<&Path>,
) -> Result<Vec<(String, Check)>, Box<dyn Error>> {
    let runtime = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let order: Vec<JudgeId> = config
        .order
        .iter()
        .filter_map(|name| JudgeId::parse(name))
        .collect();
    let rest = JudgeId::ALL.into_iter().filter(|id| !order.contains(id));
    let mut rows = Vec::new();
    let mut chosen = None;
    for id in order.iter().copied().chain(rest) {
        let backend = backend(id, config, vault);
        let availability = runtime.block_on(backend.available());
        let ready = availability == Availability::Ready;
        let status = if ready { Status::Ok } else { Status::Warn };
        // Ollama's depends on the memory free, so it's worth seeing.
        let detail = match id {
            JudgeId::Ollama => format!("{availability} ({})", backend.model()),
            _ => availability.to_string(),
        };
        rows.push((id.to_string(), (status, detail)));
        if ready && order.contains(&id) {
            chosen = chosen.or(Some(id));
        }
    }
    rows.push((
        "judge".to_string(),
        match chosen {
            Some(id) => (Status::Ok, format!("{id} would be chosen")),
            None => {
                let detail = "none ready; `kbx suggest` would leave candidates unjudged";
                (Status::Warn, detail.to_string())
            }
        },
    ));
    Ok(rows)
}

/// Which backend is active, and for the built-in engine where its model stands.
fn check_search(project: Option<&Project>, backend: Search) -> Check {
    let chosen = project.is_some_and(|project| project.config.search.is_some());
    let detail = match backend {
        Search::Qmd if chosen => "qmd".to_string(),
        Search::Qmd => "qmd, as kbx.toml lists qmd collections and sets no `search`".to_string(),
        Search::Builtin if MODEL.is_present() => "builtin, model ready".to_string(),
        Search::Builtin => format!(
            "builtin; `kbx suggest` downloads its model ({} MB) once",
            MODEL.megabytes
        ),
    };
    (Status::Ok, detail)
}

/// Missing collections stop `qmd_knn` alone, so they are worth a warning, and
/// so is one reading what's excluded, though nothing it finds goes further.
fn check_collections(project: &mut Project, fix: bool) -> Check {
    if fix {
        return match qmd::set_up_collections(project) {
            Ok(changes) if changes.is_empty() => collections_present(project),
            Ok(changes) => (
                Status::Ok,
                describe(&changes, "created", "removed", "narrowed"),
            ),
            Err(error) => (Status::Warn, error),
        };
    }
    match qmd::collection_changes(project) {
        Ok(changes) if changes.is_empty() => collections_present(project),
        Ok(changes) if changes.barred.is_empty() && changes.narrowing.is_empty() => {
            let names = changes.missing.join(", ");
            let detail = format!("missing {names}; run `kbx doctor --fix` to create them");
            (Status::Warn, detail)
        }
        Ok(changes) => {
            let found = describe(&changes, "missing", "excluded", "mask out of date for");
            (Status::Warn, format!("{found}; run `kbx doctor --fix`"))
        }
        Err(error) => (Status::Warn, format!("can't be listed: {error}")),
    }
}

/// The collections in `changes`, each kind under its label.
fn describe(changes: &qmd::Changes, missing: &str, barred: &str, narrowing: &str) -> String {
    let kinds = [
        (missing, &changes.missing),
        (barred, &changes.barred),
        (narrowing, &changes.narrowing),
    ];
    let kinds = kinds.iter().filter(|(_, names)| !names.is_empty());
    let parts = kinds.map(|(label, names)| format!("{label} {}", names.join(", ")));
    parts.collect::<Vec<_>>().join("; ")
}

fn collections_present(project: &Project) -> Check {
    let qmd = &project.config.qmd;
    let searched = qmd.searched();
    let unrooted: Vec<&str> = qmd
        .collections
        .iter()
        .filter(|name| !searched.contains_key(*name))
        .map(String::as_str)
        .collect();
    if unrooted.is_empty() {
        let detail = format!("{} in index {}", searched.len(), project.qmd_index());
        return (Status::Ok, detail);
    }
    let names = unrooted.join(", ");
    let detail = format!("no root in kbx.toml for {names}, so they find nothing");
    (Status::Warn, detail)
}

fn check_ram() -> Check {
    let unknown = (Status::Warn, "can't be read on this machine".to_string());
    let Ok(mut system) = System::new() else {
        return unknown;
    };
    system.refresh_memory();
    let total = system.total_memory() as f64 / GIB;
    let available = system.available_memory() as f64 / GIB;
    if total == 0.0 {
        return unknown;
    }
    let detail = format!("{total:.1} GiB total, {available:.1} GiB available");
    (Status::Ok, detail)
}
