//! `kbx suggest`: run the generators and store what they find.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::process::ExitCode;

use clap::ValueEnum;
use kbx_core::{Graph, IndexState, Ontology, Project, index_state};
use kbx_gen::{Budget, QmdSearch, Run, Scope, estimate_tokens, generators, store, suggest};
use kbx_judge::{
    Availability, JudgeContext, JudgeId, Tally, backend, backends, first_ready, judge_all, views,
};
use tokio::runtime;
use tracing::warn;

use crate::qmd;

/// Candidates per judge call, the default of `--batch` (§10).
const BATCH: usize = 20;

/// Who judges the candidates.
#[derive(Clone, Copy, ValueEnum)]
pub enum Judge {
    /// The first backend in `judge.order` that is ready.
    Auto,
    /// The Claude Code CLI.
    ClaudeCli,
    /// The Codex CLI.
    CodexCli,
    /// Nobody: stop at the candidates.
    None,
}

pub struct Options {
    pub note: Option<String>,
    pub generators: Option<Vec<String>>,
    pub judge: Judge,
    pub budget: Option<usize>,
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

    let roots = project.config.qmd.searched();
    let wants_knn = |names: &Vec<String>| names.iter().any(|name| name == "qmd_knn");
    let searching = options.generators.as_ref().is_none_or(wants_knn);
    let installed = qmd::program(&project).is_file();
    if searching
        && installed
        && !roots.is_empty()
        && !options.dry_run
        && let Err(error) = qmd::refresh(&project)
    {
        eprintln!("warning: the qmd index may be out of date: {error}");
    }
    let search = QmdSearch::new(qmd::program(&project), project.qmd_index(), roots, &graph)?;
    let chosen = generators(options.generators.as_deref(), Some(Box::new(search)))?;
    let scope = Scope {
        vault: &project.vault,
        ontology: &ontology,
        note: options.note.as_deref(),
    };
    let mut budget = Budget::default();
    budget.per_run = options.budget.unwrap_or(budget.per_run);
    let run = suggest(&graph, &chosen, &scope, budget)?;

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
        let context = JudgeContext::new(ontology, &yaml);
        judge(options.judge, &project, &graph, &run, &context)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Judges what's already stored, so that a backend that can't be used leaves
/// the candidates in place. Under `auto` that's no failure: §6.1 ends in a run
/// with no judge. Asking for a backend by name means its verdicts are the point.
fn judge(
    choice: Judge,
    project: &Project,
    graph: &Graph,
    run: &Run,
    context: &JudgeContext,
) -> Result<(), Box<dyn Error>> {
    let config = &project.config.judge;
    let vault = Some(project.vault.as_path());
    let named = match choice {
        Judge::None => return Ok(()),
        Judge::Auto => None,
        Judge::ClaudeCli => Some(JudgeId::ClaudeCli),
        Judge::CodexCli => Some(JudgeId::CodexCli),
    };
    let candidates = match named {
        Some(id) => vec![backend(id, config, vault)],
        None => backends(config, vault),
    };

    runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let (chosen, passed_over) = first_ready(candidates).await;
            for (id, availability) in &passed_over {
                eprintln!("warning: {id} skipped: {availability}");
            }
            let Some(chosen) = chosen else {
                if let Some(id) = named {
                    let reason = passed_over
                        .first()
                        .map_or(Availability::Unreachable, |p| p.1);
                    let stored = "the candidates are stored, unjudged";
                    return Err(format!("{id} can't judge: {reason}; {stored}").into());
                }
                println!("no judge available; the candidates are unjudged");
                return Ok(());
            };

            let found = views(
                graph,
                &project.vault,
                &run.candidates,
                &config.overrides,
                chosen.id(),
            )?;
            if found.pinned > 0 {
                let id = chosen.id();
                eprintln!(
                    "note: {} pairs kept from {id} by judge.overrides",
                    found.pinned
                );
            }
            if found.unreadable > 0 {
                eprintln!(
                    "warning: {} pairs have a note that can't be read",
                    found.unreadable
                );
            }
            let tally = judge_all(chosen.as_ref(), graph, &found.ready, context, BATCH).await?;
            print_tally(chosen.id(), chosen.model(), &tally);
            Ok(())
        })
}

fn print_tally(id: JudgeId, model: &str, tally: &Tally) {
    println!(
        "judged by {id} ({model}): {} accept, {} reject, {} unsure, {} unjudged",
        tally.accepted, tally.rejected, tally.unsure, tally.unjudged
    );
    let cost = tally
        .cost_usd
        .map_or(String::new(), |cost| format!("${cost:.4}, "));
    println!("  {cost}{} cached tokens", tally.cached_tokens);
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
