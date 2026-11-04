//! `kbx suggest`: run the generators and store what they find.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::ValueEnum;
use kbx_core::{Graph, IndexState, Ontology, Project, index_state};
use kbx_gen::{Budget, QmdSearch, Run, Scope, estimate_tokens, generators, store, suggest};
use tracing::warn;

use crate::qmd;

/// Candidates per judge call, the default of `--batch` (§10).
const BATCH: usize = 20;

/// Who judges the candidates. The backends arrive with `kbx-judge`.
#[derive(Clone, Copy, ValueEnum)]
pub enum Judge {
    /// Nobody: stop at the candidates.
    None,
}

pub struct Options {
    pub note: Option<String>,
    pub generators: Option<Vec<String>>,
    pub dry_run: bool,
}

pub fn run(options: Options) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let yaml = fs::read_to_string(&project.ontology)
        .map_err(|error| format!("{}: {error}", project.ontology.display()))?;
    let ontology = Ontology::parse(&yaml)?;
    let graph = if options.dry_run {
        Graph::open_read_only(&project.graph)?
    } else {
        Graph::open(&project.graph)?
    };
    match index_state(&graph, &yaml)? {
        IndexState::Unbuilt => return Err("the vault has no index yet; run `kbx index`".into()),
        IndexState::Stale => warn!("the index is out of date; run `kbx index`"),
        IndexState::Current => {}
    }
    if let Some(id) = &options.note {
        crate::find_node(&graph, id)?;
    }

    let qmd = &project.config.qmd;
    let bin = qmd::find_on_path(&qmd.bin).unwrap_or_else(|| PathBuf::from(&qmd.bin));
    let search = QmdSearch::new(bin, qmd.collections.clone(), &graph)?;
    let chosen = generators(options.generators.as_deref(), Some(Box::new(search)))?;
    let scope = Scope {
        vault: &project.vault,
        ontology: &ontology,
        note: options.note.as_deref(),
    };
    let run = suggest(&graph, &chosen, &scope, Budget::default())?;

    for (name, error) in &run.skipped {
        eprintln!("warning: {name} skipped: {error}");
    }
    // Asking for a generator by name means its candidates are the point.
    if options.generators.is_some() && !run.skipped.is_empty() {
        return Err("a generator failed, so nothing was stored".into());
    }
    if options.dry_run {
        println!("would store {} candidates", run.candidates.len());
        print_counts(&run);
        print_batches(&graph, &run)?;
    } else {
        store(&graph, &run)?;
        println!("{} candidates", run.candidates.len());
        print_counts(&run);
    }
    Ok(ExitCode::SUCCESS)
}

fn print_counts(run: &Run) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for name in run.candidates.iter().flat_map(|c| &c.generators) {
        *counts.entry(name).or_default() += 1;
    }
    for (name, count) in counts {
        println!("  {name} {count}");
    }
    println!(
        "dropped {} linked or rejected, {} over budget",
        run.dropped, run.over_budget
    );
}

fn print_batches(graph: &Graph, run: &Run) -> Result<(), Box<dyn Error>> {
    let tokens = estimate_tokens(graph, &run.candidates)?;
    println!("about {} judge input tokens", tokens.iter().sum::<i64>());
    for (index, batch) in tokens.chunks(BATCH).enumerate() {
        let total: i64 = batch.iter().sum();
        println!(
            "  batch {}: {} candidates, about {total} tokens",
            index + 1,
            batch.len()
        );
    }
    Ok(())
}
