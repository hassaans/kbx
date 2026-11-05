//! `kbx suggest`: run the generators and store what they find.

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::ops::RangeInclusive;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use clap::ValueEnum;
use kbx_core::{
    Exclusions, Graph, IndexOptions, IndexState, JudgeConfig, Ontology, Project,
    Search as SearchBackend, excluded_notes, index_state, index_vault,
};
use kbx_gen::{
    Budget, Evaluation, QmdSearch, Run, Scope, Search, SearchCache, estimate_tokens, evaluate,
    forget_sections, generators, place_sections, store, suggest, vault_state,
};
use kbx_judge::{
    Availability, Judge as Backend, JudgeContext, JudgeId, Plan, Tally, backend, backends,
    judge_all, pending, pinned_backends, route, view_tokens, views,
};
use tokio::runtime;
use tracing::warn;

use crate::embedding::{Coverage, Embed, Engine};
use crate::{builtin, qmd};

/// Pairs per judge call (§6.3). Below this, start-up and the prefix dominate
/// what a call costs; above it, one failure loses more.
const BATCH_SIZES: RangeInclusive<usize> = 10..=25;

/// What `qmd query` found, next to the graph. See `docs/embedding-cache.md`.
const SEARCH_CACHE: &str = "search-cache.sqlite";

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
    pub batch: Option<usize>,
    pub budget_usd: Option<f64>,
    pub embed: Embed,
    pub dry_run: bool,
    pub eval: bool,
}

pub fn run(options: Options) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    if options.stage {
        crate::review::require_frontmatter(&project, "`kbx suggest --stage`")?;
    }
    let size = batch_size(&project, options.batch)?;
    let yaml = fs::read_to_string(&project.ontology)
        .map_err(|error| format!("{}: {error}", project.ontology.display()))?;
    let ontology = Ontology::parse(&yaml)?;
    let read_only = options.dry_run || options.eval;
    let mut graph = if read_only {
        Graph::open_read_only(&project.graph)?
    } else {
        Graph::open(&project.graph)?
    };
    match index_state(&graph, &yaml, &project.config.type_defaults())? {
        IndexState::Unbuilt => return Err("the vault has no index yet; run `kbx index`".into()),
        IndexState::Stale => warn!("the index is out of date; run `kbx index`"),
        IndexState::Current => {}
    }
    let exclusions = project.exclusions()?;
    if !read_only {
        forget_excluded(&project, &mut graph, &yaml, &exclusions)?;
    }
    // After the line above, which can be what empties the graph. The run goes
    // on, as an empty graph still has to empty the search index.
    let empty = graph.stats()?.nodes == 0;
    if empty {
        println!(
            "the graph is empty, so there is nothing to suggest; \
             run `kbx index`, which says why notes were left out"
        );
    }
    if let Some(id) = &options.note {
        crate::find_node(&graph, id)?;
    }

    let wants_knn = |names: &Vec<String>| names.iter().any(|name| name == "qmd_knn");
    let searching = options.generators.as_ref().is_none_or(wants_knn);
    let idle = match searching {
        true => nothing_to_search(&project, &graph, &exclusions)?,
        false => None,
    };
    if let Some(reason) = idle {
        println!("skipped the search: {reason}");
    }
    let embed = options.embed;
    let backend = match (idle, project.config.backend()) {
        (None, SearchBackend::Qmd) => {
            Some(qmd_backend(&project, &graph, searching, read_only, embed)?)
        }
        (None, SearchBackend::Builtin) => Some(builtin::backend(
            &project, &graph, searching, read_only, embed,
        )?),
        (Some(_), SearchBackend::Builtin) if !read_only => {
            builtin::forget(&project, &graph)?;
            None
        }
        (Some(_), _) => None,
    };
    let (search, coverage) = backend.unzip();
    let coverage = coverage.flatten();
    let chosen = generators(options.generators.as_deref(), search)?;
    let scope = Scope {
        vault: &project.vault,
        ontology: &ontology,
        note: options.note.as_deref(),
        mention_weight: project.config.generators.mention_weight,
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
    let context = JudgeContext::new(ontology, &yaml);
    if options.dry_run {
        if !empty {
            println!("would store {} candidates", run.candidates.len());
        }
        print_counts(&run);
        coverage.iter().for_each(Coverage::print);
        print_batches(
            options.judge,
            &project,
            &graph,
            &exclusions,
            &run,
            &context,
            size,
        )?;
        if options.stage {
            println!("would stage up to {} proposals", run.candidates.len());
        }
    } else {
        if !anchors {
            forget_sections(&graph)?;
        }
        store(&graph, &run)?;
        if !empty {
            println!("{} candidates", run.candidates.len());
        }
        print_counts(&run);
        coverage.iter().for_each(Coverage::print);
        let plan = Plan {
            size,
            budget_usd: options.budget_usd,
            spent_usd: 0.0,
        };
        judge(
            options.judge,
            &project,
            &graph,
            &exclusions,
            &run,
            &context,
            plan,
        )?;
        if options.stage {
            return stage(&project, &mut graph, &yaml, &run);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Takes notes that were excluded after they were indexed out of the graph,
/// before anything reads it: the generators, and the search index built from it.
fn forget_excluded(
    project: &Project,
    graph: &mut Graph,
    yaml: &str,
    exclusions: &Exclusions,
) -> Result<(), Box<dyn Error>> {
    let excluded = excluded_notes(graph, exclusions)?;
    if !excluded.is_empty() {
        eprintln!(
            "note: {} notes are now excluded; re-indexing",
            excluded.len()
        );
        index_vault(
            graph,
            &project.vault,
            yaml,
            exclusions,
            &project.config.type_defaults(),
            &IndexOptions::default(),
        )?;
    }
    Ok(())
}

/// Why no search could find a candidate, if none could. This alone decides
/// whether a run refreshes and searches, which could embed a whole vault for
/// nothing. A qmd with no collections isn't one of these: `qmd_knn` fails there,
/// and says to run `kbx init`.
fn nothing_to_search(
    project: &Project,
    graph: &Graph,
    exclusions: &Exclusions,
) -> Result<Option<&'static str>, Box<dyn Error>> {
    if graph.stats()?.nodes == 0 {
        return Ok(Some("the graph has no notes"));
    }
    let roots = project.searched(exclusions);
    if matches!(project.config.backend(), SearchBackend::Builtin) || roots.is_empty() {
        return Ok(None);
    }
    // Without its `.` and `./`, so that the vault's root is a prefix of every path.
    let folder = |root: &String| -> PathBuf {
        let normal = |part: &Component<'_>| matches!(part, Component::Normal(_));
        Path::new(root).components().filter(normal).collect()
    };
    let folders: Vec<PathBuf> = roots.values().map(folder).collect();
    let mut statement = graph.connection().prepare("SELECT path FROM files")?;
    let paths = statement.query_map([], |row| row.get::<_, String>(0))?;
    for path in paths {
        let path = path?;
        if folders
            .iter()
            .any(|root| Path::new(&path).starts_with(root))
        {
            return Ok(None);
        }
    }
    Ok(Some("no note is in a searched folder"))
}

/// QMD, with what it found kept in the search cache where that can be trusted,
/// and how much of its index has vectors, if it says.
fn qmd_backend(
    project: &Project,
    graph: &Graph,
    searching: bool,
    read_only: bool,
    embed: Embed,
) -> Result<Engine, Box<dyn Error>> {
    let roots = project.searched(&project.exclusions()?);
    let usable = searching && qmd::program(project).is_file() && !roots.is_empty();
    let mut status = usable.then(|| qmd::status(project)).flatten();
    let mut cache = status
        .as_ref()
        .and_then(|status| search_cache(project, status, read_only));
    // What a half-built index finds shouldn't outlive this run.
    if usable && !read_only {
        let (current, now) = refresh(project, graph, status, cache.as_ref(), embed);
        status = now;
        if !current {
            cache = None;
        }
    }
    let qmd_search = QmdSearch::new(
        qmd::program(project),
        project.qmd_index(),
        roots.clone(),
        graph,
    )?;
    let search: Box<dyn Search> = match cache {
        Some(cache) => {
            let inner = Box::new(qmd_search);
            Box::new(cache.around(inner, graph, &project.qmd_index(), &roots)?)
        }
        None => Box::new(qmd_search),
    };
    Ok((search, status.as_ref().and_then(qmd::Status::coverage)))
}

/// Pairs per judge call: `batch`, else `judge.batch`.
pub fn batch_size(project: &Project, batch: Option<usize>) -> Result<usize, Box<dyn Error>> {
    let size = batch.unwrap_or(project.config.judge.batch);
    if !BATCH_SIZES.contains(&size) {
        let (least, most) = (BATCH_SIZES.start(), BATCH_SIZES.end());
        return Err(format!("the batch size must be {least} to {most}, not {size}").into());
    }
    Ok(size)
}

/// The cache for what qmd reports. A vault works without one, so a failure
/// only warns. An index named in `kbx.toml` changes without kbx, so whatever it
/// found holds only while its counts do.
fn search_cache(project: &Project, status: &qmd::Status, read_only: bool) -> Option<SearchCache> {
    let path = project.graph.with_file_name(SEARCH_CACHE);
    let source = match project.config.qmd.index {
        Some(_) => format!("{}\n{}", status.models, status.index),
        None => status.models.clone(),
    };
    if read_only {
        return SearchCache::open_read_only(&path, &source);
    }
    SearchCache::open(&path, &source)
        .inspect_err(|error| warn!("{}: {error}", path.display()))
        .ok()
}

/// Runs `qmd update`, then `qmd embed` if `embed` says to, unless the cache
/// knows they have nothing to do. Says whether qmd is up to date, which an
/// index left with something to embed isn't, and what qmd reports now. A
/// refresh is recorded only once it has worked, with what qmd says after it.
fn refresh(
    project: &Project,
    graph: &Graph,
    status: Option<qmd::Status>,
    cache: Option<&SearchCache>,
    embed: Embed,
) -> (bool, Option<qmd::Status>) {
    let stamp = |status: &qmd::Status| vault_state(graph, &status.index).ok();
    if let (Some(cache), Some(state)) = (cache, status.as_ref().and_then(stamp))
        && cache.is_refreshed(&state)
    {
        return (true, status);
    }
    let failed = |error: String| eprintln!("warning: the qmd index may be out of date: {error}");
    if let Err(error) = qmd::update(project) {
        failed(error);
        return (false, status);
    }
    // Only now does qmd know what the vault's new notes add to its backlog. A
    // qmd that said nothing before won't start now.
    let mut status = status.and_then(|_| qmd::status(project));
    let named = project.config.qmd.index.is_some();
    let backlog = status.as_ref().and_then(qmd::Status::backlog);
    let embedding = embed.waits_for(backlog.as_ref().map(|backlog| backlog.seconds));
    if let Some(backlog) = backlog.as_ref().filter(|b| b.count > 0 && !named) {
        backlog.announce(embedding, &qmd::embed_by_hand(project));
    }
    if embedding {
        if let Err(error) = qmd::embed(project) {
            failed(error);
            return (false, status);
        }
        status = status.and_then(|_| qmd::status(project));
    } else if !named && backlog.is_none_or(|backlog| backlog.count > 0) {
        return (false, status);
    }
    if let Some(cache) = cache
        && let Some(state) = status.as_ref().and_then(stamp)
        && let Err(error) = cache.mark_refreshed(&state)
    {
        warn!("the search cache: {error}");
    }
    (true, status)
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
    if report.inferred > 0 {
        println!(
            "left {} notes with inferred keys alone; their candidates are in `kbx review`",
            report.inferred
        );
    }
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
        let exclusions = project.exclusions()?;
        let defaults = project.config.type_defaults();
        index_vault(
            graph,
            &project.vault,
            yaml,
            &exclusions,
            &defaults,
            &options,
        )?;
    }
    Ok(match report.refused.is_empty() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}

type Backends = Vec<Box<dyn Backend>>;

/// The backends `choice` allows, best first, those folders are pinned to, and
/// the one it names if it does.
fn eligible(
    choice: Judge,
    config: &JudgeConfig,
    project: &Project,
) -> (Backends, Backends, Option<JudgeId>) {
    let vault = Some(project.vault.as_path());
    let named = match choice {
        Judge::None => return (Vec::new(), Vec::new(), None),
        Judge::Auto => None,
        Judge::ClaudeCli => Some(JudgeId::ClaudeCli),
        Judge::CodexCli => Some(JudgeId::CodexCli),
        Judge::Ollama => Some(JudgeId::Ollama),
        Judge::LlamaServer => Some(JudgeId::LlamaServer),
    };
    match named {
        Some(id) => (vec![backend(id, config, vault)], Vec::new(), named),
        None => (
            backends(config, vault),
            pinned_backends(config, vault),
            None,
        ),
    }
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
    exclusions: &Exclusions,
    run: &Run,
    context: &JudgeContext,
    plan: Plan,
) -> Result<(), Box<dyn Error>> {
    if matches!(choice, Judge::None) {
        return Ok(());
    }
    let config = &project.config.judge;
    let (general, pinned, named) = eligible(choice, config, project);

    runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let routes = route(
                graph,
                &project.vault,
                exclusions,
                &run.candidates,
                &config.overrides,
                general,
                pinned,
                plan.budget_usd,
            )
            .await?;
            // A backend that was never installed is no surprise, and `auto` is the
            // default, unless a folder is waiting for it.
            let pinned_to = |id: JudgeId| config.overrides.values().any(|name| name == id.as_str());
            let worth_saying =
                |p: &&(JudgeId, Availability)| p.1 != Availability::NotInstalled || pinned_to(p.0);
            for (id, availability) in routes.passed_over.iter().filter(worth_saying) {
                eprintln!("warning: {id} skipped: {availability}");
            }
            if let (true, Some(budget)) = (routes.halted, plan.budget_usd) {
                let left = match routes.assignments.is_empty() {
                    true => "the candidates are unjudged",
                    false => "some pairs wait for another run",
                };
                println!(
                    "budget reached: spent ${:.4} of ${budget:.4}; {left}",
                    routes.probes_usd
                );
                if routes.assignments.is_empty() {
                    return Ok(());
                }
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
            // One budget for the run: each backend starts where the last one stopped.
            let mut plan = Plan {
                spent_usd: routes.probes_usd,
                ..plan
            };
            let mut reported = 0.0;
            for assignment in &routes.assignments {
                let judge = assignment.judge.as_ref();
                let tally = judge_all(judge, graph, &assignment.views, context, plan).await?;
                print_tally(judge.id(), judge.model(), &tally, reported, plan.budget_usd);
                plan.spent_usd = tally.cost_usd.unwrap_or(plan.spent_usd);
                reported = tally.cost_usd.unwrap_or(reported);
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

/// `reported` is what the backends before this one have already accounted for.
fn print_tally(id: JudgeId, model: &str, tally: &Tally, reported: f64, budget_usd: Option<f64>) {
    println!(
        "judged by {id} ({model}): {} accept, {} reject, {} unsure, {} unjudged, {} error, {} already judged",
        tally.accepted, tally.rejected, tally.unsure, tally.unjudged, tally.errored, tally.cached
    );
    let cost = tally.cost_usd.map(|cost| cost - reported);
    let cost = cost.map_or(String::new(), |cost| format!("${cost:.4}, "));
    println!("  {cost}{} cached tokens", tally.cached_tokens);
    if let (true, Some(budget)) = (tally.halted, budget_usd) {
        println!(
            "budget reached: spent ${:.4} of ${budget:.4}; run again to judge the {} left",
            tally.cost_usd.unwrap_or(0.0),
            tally.unjudged
        );
    }
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

/// What a real run would send. Nothing is probed, so under `auto` this is for
/// the first backend in `judge.order`, ready or not.
fn print_batches(
    choice: Judge,
    project: &Project,
    graph: &Graph,
    exclusions: &Exclusions,
    run: &Run,
    context: &JudgeContext,
    size: usize,
) -> Result<(), Box<dyn Error>> {
    let config = &project.config.judge;
    let tokens = match eligible(choice, config, project).0.first() {
        Some(judge) => {
            let found = views(
                graph,
                &project.vault,
                exclusions,
                &run.candidates,
                &config.overrides,
                judge.id(),
            )?;
            let open = pending(judge.as_ref(), graph, &found.ready, context)?;
            let id = judge.id();
            println!(
                "for {id}: {} already judged, {} kept away by judge.overrides",
                found.ready.len() - open.len(),
                found.pinned
            );
            open.iter().map(view_tokens).collect()
        }
        None => estimate_tokens(graph, &run.candidates)?,
    };
    println!("about {} judge input tokens", tokens.iter().sum::<i64>());
    for (index, batch) in tokens.chunks(size).enumerate() {
        let total: i64 = batch.iter().sum();
        println!(
            "  batch {}: {} candidates, about {total} tokens",
            index + 1,
            batch.len()
        );
    }
    Ok(())
}
