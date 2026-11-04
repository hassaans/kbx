//! `kbx doctor`: one row per check. A failed check doesn't stop the others.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::process::ExitCode;

use kbx_core::{ConfigError, Graph, IndexState, Ontology, Project, index_state};
use sysinfo::System;

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

pub fn run() -> Result<ExitCode, Box<dyn Error>> {
    let mut healthy = true;
    let mut report = |name: &str, (status, detail): Check| {
        println!("{name:<9}{status:<5}{detail}");
        healthy &= status != Status::Fail;
    };

    match Project::discover(&env::current_dir()?) {
        Ok(project) => {
            report("vault", (Status::Ok, project.vault.display().to_string()));
            let yaml = fs::read_to_string(&project.ontology).ok();
            let ontology = check_ontology(&project, yaml.as_deref());
            // Staleness means nothing against a file `kbx index` would refuse.
            let usable = yaml.as_deref().filter(|_| ontology.0 == Status::Ok);
            let index = check_index(&project, usable);
            report("ontology", ontology);
            report("index", index);
        }
        Err(ConfigError::NotInitialised) => {
            let detail = "no kbx.toml here or in any parent folder; run `kbx init`";
            report("vault", (Status::Warn, detail.to_string()));
        }
        Err(error) => report("vault", (Status::Fail, error.to_string())),
    }
    report("ram", check_ram());

    // Last, so that its report is the end of the output.
    match qmd::ensure() {
        Ok(path) => report("qmd", (Status::Ok, path.display().to_string())),
        Err(error) => {
            report("qmd", (Status::Fail, "not installed".to_string()));
            eprintln!("error: {error}");
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
