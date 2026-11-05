//! `kbx export` and `kbx import`. The formats are in `docs/export.md`.

use std::env;
use std::error::Error;
use std::fs;
use std::io::{BufWriter, Read, Write, stdin, stdout};
use std::path::Path;
use std::process::ExitCode;

use kbx_core::{Graph, IndexState, Project, index_state, parse_reviews, turtle};
use serde::Serialize;
use tracing::warn;

use crate::plural;
use crate::review::{NO_INDEX, Session};

pub fn edges() -> Result<ExitCode, Box<dyn Error>> {
    let session = indexed()?;
    print_lines(&session.graph.edges()?)
}

/// Needs no index: reviews imported into a new vault are worth exporting.
pub fn reviews() -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    if !project.graph.is_file() {
        return Err(NO_INDEX.into());
    }
    print_lines(&Graph::open_read_only(&project.graph)?.reviews()?)
}

pub fn rdf() -> Result<ExitCode, Box<dyn Error>> {
    let session = indexed()?;
    let text = turtle(&session.graph, &session.ontology)?;
    stdout().write_all(text.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

pub fn import_reviews(file: Option<&Path>) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let text = match file {
        Some(path) => {
            fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?
        }
        None => {
            let mut text = String::new();
            stdin().read_to_string(&mut text)?;
            text
        }
    };
    let reviews = parse_reviews(&text)?;
    Graph::open(&project.graph)?.import_reviews(&reviews)?;
    println!("imported {}", plural(reviews.len(), "review"));
    Ok(ExitCode::SUCCESS)
}

fn indexed() -> Result<Session, Box<dyn Error>> {
    let session = Session::read_only()?;
    let defaults = session.project.config.type_defaults();
    if index_state(&session.graph, &session.yaml, &defaults)? == IndexState::Stale {
        warn!("the index is out of date; run `kbx index`");
    }
    Ok(session)
}

fn print_lines<T: Serialize>(rows: &[T]) -> Result<ExitCode, Box<dyn Error>> {
    let mut out = BufWriter::new(stdout().lock());
    for row in rows {
        serde_json::to_writer(&mut out, row)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}
