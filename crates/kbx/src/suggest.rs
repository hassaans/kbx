//! `kbx suggest`: run the generators and store what they find.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::process::ExitCode;

use clap::ValueEnum;
use kbx_core::{Graph, IndexOptions, IndexState, Ontology, Project, index_state, index_vault};
use kbx_gen::{
    Budget, Evaluation, QmdSearch, Run, Scope, estimate_tokens, evaluate, forget_sections,
    generators, place_sections, store, suggest,
};
use kbx_judge::{
    Availability, JudgeContext, JudgeId, Tally, backend, backends, judge_all, pinned_backends,
    route,
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
    /// A local Ollama server.
    Ollama,
    /// A local llama-server.
    LlamaServer,
    /// Nobody: stop at the candidates.
    None,
}

pub struct Options {
    pub note: Option<String>,
    pub generators: Option<Vec<String>>,
    pub judge: Judge,
    pub budget: Option<usize>,
    pub stage: bool,
    pub dry_run: bool,
    pub eval: bool,
}

pub fn run(options: Options) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let yaml = fs::read_to_string(&project.ontology)
        .map_err(|error| format!("{}: {error}", project.ontology.display()))?;
    let ontology = Ontology::parse(&yaml)?;
    let read_only = options.dry_run || options.eval;
    let mut graph = if read_only {
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
        && !read_only
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
    if options.eval {
        let named = options.generators.is_some();
        return print_evaluation(evaluate(&graph, chosen, &scope)?, named);
    }
    let mut budget = Budget::default();
    budget.per_run = options.budget.unwrap_or(budget.per_run);
    let mut run = suggest(&graph, &chosen, &scope, budget)?;
    let anchors = project.config.write.section_anchors;
    if anchors {
        place_sections(&graph, &project.vault, &mut run)?;
    }

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
        if options.stage {
            println!("would stage up to {} proposals", run.candidates.len());
        }
    } else {
        if !anchors {
            forget_sections(&graph)?;
        }
        store(&graph, &run)?;
        println!("{} candidates", run.candidates.len());
        print_counts(&run);
        let context = JudgeContext::new(ontology, &yaml);
        judge(options.judge, &project, &graph, &run, &context)?;
        if options.stage {
            return stage(&project, &mut graph, &yaml, &run);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Writes the run's candidates to `kbx.proposed`, after the judge so that they
/// carry its relation. The index follows, as it does after `kbx accept`.
fn stage(
    project: &Project,
    graph: &mut Graph,
    yaml: &str,
    run: &Run,
) -> Result<ExitCode, Box<dyn Error>> {
    let ontology = Ontology::parse(yaml)?;
    let cids: Vec<String> = run.candidates.iter().map(|c| c.cid.clone()).collect();
    let report = kbx_core::stage(graph, &project.vault, &ontology, &cids)?;
    println!(
        "staged {} proposals in {} notes",
        report.staged,
        report.written.len()
    );
    for (note, error) in &report.refused {
        eprintln!("error: {note}: {error}");
    }
    if !report.written.is_empty() {
        let options = IndexOptions {
            paths: report
                .written
                .iter()
                .map(|p| project.vault.join(p))
                .collect(),
            ..IndexOptions::default()
        };
        index_vault(graph, &project.vault, yaml, &options)?;
    }
    Ok(match report.refused.is_empty() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}

/// Judges what's already stored, so that a backend that can't be used leaves
/// the candidates in place. Under `auto` that's no failure: §6.1 ends in a run
/// with no judge. Asking for a backend by name means its verdicts are the point,
/// and that it alone runs: a folder pinned elsewhere waits for a run that can
/// reach its backend, which `auto` is.
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
        Judge::Ollama => Some(JudgeId::Ollama),
        Judge::LlamaServer => Some(JudgeId::LlamaServer),
    };
    let (general, pinned) = match named {
        Some(id) => (vec![backend(id, config, vault)], Vec::new()),
        None => (backends(config, vault), pinned_backends(config, vault)),
    };

    runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let routes = route(
                graph,
                &project.vault,
                &run.candidates,
                &config.overrides,
                general,
                pinned,
            )
            .await?;
            for (id, availability) in &routes.passed_over {
                eprintln!("warning: {id} skipped: {availability}");
            }
            if routes.assignments.is_empty() {
                if let Some(id) = named {
                    let reason = routes
                        .passed_over
                        .first()
                        .map_or(Availability::Unreachable, |p| p.1);
                    let stored = "the candidates are stored, unjudged";
                    return Err(format!("{id} can't judge: {reason}; {stored}").into());
                }
                println!("no judge available; the candidates are unjudged");
                return Ok(());
            }

            if let Some(id) = routes.chosen.filter(|_| routes.pinned > 0) {
                eprintln!(
                    "note: {} pairs kept from {id} by judge.overrides",
                    routes.pinned
                );
            }
            if routes.unreadable > 0 {
                eprintln!(
                    "warning: {} pairs have a note that can't be read",
                    routes.unreadable
                );
            }
            for assignment in &routes.assignments {
                let judge = assignment.judge.as_ref();
                let tally = judge_all(judge, graph, &assignment.views, context, BATCH).await?;
                print_tally(judge.id(), judge.model(), &tally);
            }
            Ok(())
        })
}

fn print_evaluation(evaluation: Evaluation, named: bool) -> Result<ExitCode, Box<dyn Error>> {
    for (name, error) in &evaluation.skipped {
        eprintln!("warning: {name} skipped: {error}");
    }
    if named && !evaluation.skipped.is_empty() {
        return Err("a generator failed, so it has no score".into());
    }
    let rate = |rate: Option<f64>| rate.map_or("-".to_string(), |rate| format!("{rate:.2}"));
    println!(
        "{:<18}{:>9}{:>9}{:>6}{:>11}{:>8}",
        "generator", "proposed", "reviewed", "hits", "precision", "recall"
    );
    for score in &evaluation.scores {
        println!(
            "{:<18}{:>9}{:>9}{:>6}{:>11}{:>8}",
            score.name,
            score.proposed,
            score.reviewed,
            score.hits,
            rate(score.precision),
            rate(score.recall)
        );
    }
    println!(
        "against {} and {}",
        crate::plural(evaluation.accepted, "accepted pair"),
        crate::plural(evaluation.rejected, "rejected pair")
    );
    Ok(ExitCode::SUCCESS)
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
    if run.locked > 0 {
        println!("left out {} pairs at reviewed notes", run.locked);
    }
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
