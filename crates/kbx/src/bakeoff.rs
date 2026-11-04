//! `kbx bakeoff`: how well each judge agrees with a person, on the same pairs.
//!
//! Verdicts go to `.kbx/bakeoff.sqlite`, not to the graph: `kbx review` shows
//! the latest verdict, and a bake-off's would take its place. Everything else
//! is what `kbx suggest` uses, so batching, the retry, the budget and the
//! verdict cache all apply, and a re-run costs nothing. See
//! `docs/judge-bakeoff.md`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use kbx_core::{
    Graph, IndexState, JUDGE_ERROR, NONE, Ontology, Project, cid, index_state, ordered,
};
use kbx_gen::{Budget, Proposal, QmdSearch, Run, Scope, generators, store, suggest};
use kbx_judge::{
    CandidateView, Judge, JudgeContext, JudgeId, NoteView, Plan, backend, backend_with_model,
    first_ready, judge_all, pending, views,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tokio::runtime;
use tracing::warn;

use crate::{qmd, suggest as suggesting};

/// Where `--labels` looks by default.
pub const LABELS: &str = "tests/fixtures/bakeoff/labels.jsonl";

/// Where the report goes by default.
pub const DOC: &str = "docs/judge-bakeoff.md";

/// The report replaces what's between these.
const BEGIN: &str = "<!-- bakeoff:begin -->";
const END: &str = "<!-- bakeoff:end -->";

/// The §14 line-up, and the bigger Claude to compare its default with.
const LINE_UP: [&str; 5] = [
    "claude-cli",
    "claude-cli:sonnet",
    "codex-cli",
    "ollama:qwen3:14b",
    "ollama:qwen3:8b",
];

/// Enough of a note to recognise it, and little enough to publish.
const EXCERPT_WORDS: usize = 20;

/// How many chosen pairs may share a note, unless that leaves too few. Without
/// it, a hub note would answer for most of the set.
const NOTE_LIMIT: usize = 3;

/// It finds no pairs of its own, so a pair's stratum doesn't depend on it.
const RE_RANKER: &str = "type_pair_prior";

const OVER_BUDGET: &str = "over budget";

pub struct Options {
    pub labels: PathBuf,
    pub doc: PathBuf,
    pub models: Option<Vec<String>>,
    pub batch: Option<usize>,
    pub budget_usd: Option<f64>,
}

pub struct Preparing {
    pub pairs: usize,
    pub out: Option<PathBuf>,
    pub synthetic: bool,
    pub generators: Option<Vec<String>>,
}

/// A line of a `--prepare` file: a `reviews` row with nothing decided, then
/// what helps a person read it. A labels file is either that with `decision`
/// and `rel` filled in, or `kbx export reviews --jsonl`, which has the first
/// seven fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Pair {
    src: String,
    dst: String,
    rel: Option<String>,
    decision: Option<String>,
    reason: Option<String>,
    #[serde(default)]
    strikes: i64,
    decided_at: Option<i64>,
    #[serde(skip_deserializing)]
    cid: String,
    #[serde(default)]
    generators: Vec<String>,
    score: Option<f64>,
    src_type: Option<String>,
    dst_type: Option<String>,
    src_excerpt: Option<String>,
    dst_excerpt: Option<String>,
}

/// What a person decided about a pair.
#[derive(Debug, Clone, PartialEq)]
struct Label {
    cid: String,
    /// Lower id first: a person's direction would hint at the decision.
    src: String,
    dst: String,
    accepted: bool,
    /// The relation accepted, under its forward name.
    rel: Option<String>,
    decided_at: Option<i64>,
}

/// What a labels file comes to.
#[derive(Debug, Default)]
struct Labels {
    /// In cid order, which says nothing about the decisions.
    pairs: Vec<Label>,
    /// Rows with no decision, as in a half-filled `--prepare` file.
    undecided: usize,
}

fn read_labels(text: &str, ontology: &Ontology) -> Result<Labels, Box<dyn Error>> {
    let mut latest: BTreeMap<String, Label> = BTreeMap::new();
    let mut undecided = 0;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let number = index + 1;
        let row: Pair =
            serde_json::from_str(line).map_err(|error| format!("line {number}: {error}"))?;
        let accepted = match row.decision.as_deref() {
            Some("accept") => true,
            Some("reject") => false,
            // Deferring is putting off a decision.
            Some("defer") | None => {
                undecided += 1;
                continue;
            }
            Some(other) => {
                return Err(format!(
                    "line {number}: `{other}` is not a decision; expected accept, reject or defer"
                )
                .into());
            }
        };
        let rel = match row.rel.as_deref().filter(|rel| *rel != NONE) {
            Some(rel) => {
                let key = ontology
                    .lookup(rel)
                    .ok_or_else(|| format!("line {number}: `{rel}` is not a relation"))?;
                Some(key.name.to_string())
            }
            None => None,
        };
        let (src, dst) = ordered(&row.src, &row.dst);
        let label = Label {
            cid: cid(&src, &dst),
            src,
            dst,
            accepted,
            rel: rel.filter(|_| accepted),
            decided_at: row.decided_at,
        };
        // A pair can have a row per relation. The last word stands, and on a
        // tie the accept: only that writes a link.
        match latest.get(&label.cid) {
            Some(held) if (held.decided_at, held.accepted) > (label.decided_at, label.accepted) => {
            }
            _ => {
                latest.insert(label.cid.clone(), label);
            }
        }
    }
    Ok(Labels {
        pairs: latest.into_values().collect(),
        undecided,
    })
}

/// What a judge said about a pair.
#[derive(Debug, Clone, PartialEq)]
struct Answer {
    verdict: String,
    rel: Option<String>,
    cost_usd: Option<f64>,
}

/// One judge against the labels.
#[derive(Debug, Default, Clone, PartialEq)]
struct Score {
    /// Pairs with a verdict.
    answered: usize,
    /// Pairs where its accept or reject matched.
    agreed: usize,
    /// Of those, the accepts.
    accepts_agreed: usize,
    /// Accepts where it also named the relation.
    relations_agreed: usize,
    /// Those by relation.
    by_relation: BTreeMap<String, usize>,
    unsure: usize,
    errors: usize,
    /// What its calls cost, failed ones included, if any was priced.
    cost_usd: Option<f64>,
}

fn score(labels: &[Label], answers: &HashMap<String, Answer>) -> Score {
    let mut score = Score::default();
    for label in labels {
        let Some(answer) = answers.get(&label.cid) else {
            continue;
        };
        if let Some(cost) = answer.cost_usd {
            *score.cost_usd.get_or_insert(0.0) += cost;
        }
        match answer.verdict.as_str() {
            JUDGE_ERROR => {
                score.errors += 1;
                continue;
            }
            "unsure" => score.unsure += 1,
            verdict if (verdict == "accept") == label.accepted => {
                score.agreed += 1;
                score.accepts_agreed += usize::from(label.accepted);
            }
            _ => {}
        }
        score.answered += 1;
        if let Some(rel) = label
            .rel
            .as_ref()
            .filter(|rel| answer.verdict == "accept" && answer.rel.as_ref() == Some(*rel))
        {
            score.relations_agreed += 1;
            *score.by_relation.entry(rel.clone()).or_default() += 1;
        }
    }
    score
}

/// A row of the report.
struct Entry {
    backend: JudgeId,
    model: String,
    score: Score,
    /// How long its calls took, over every run.
    wall_ms: u64,
    /// Why it didn't run, if it didn't.
    note: String,
}

fn percent(part: usize, whole: usize) -> String {
    match whole {
        0 => "-".to_string(),
        _ => format!(
            "{:.1}% ({part}/{whole})",
            part as f64 * 100.0 / whole as f64
        ),
    }
}

fn duration(ms: u64) -> String {
    match ms {
        0 => "-".to_string(),
        ms if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        ms => format!("{}m {:02}s", ms / 60_000, ms / 1000 % 60),
    }
}

/// The Markdown between the markers. It has no date or run-specific figure, so
/// an unchanged re-run writes the same text.
fn render(labels: &[Label], entries: &[Entry]) -> String {
    let total = labels.len();
    let accepts = labels.iter().filter(|label| label.accepted).count();
    let rejects = total - accepts;
    let named = labels.iter().filter(|label| label.rel.is_some()).count();
    let mut text = format!(
        "{total} labelled pairs: {accepts} accept, {rejects} reject.\n\n\
         | Backend | Model | Answered | Agreement | Accept | Reject | Relation | Unsure | Errors | Cost | Wall time | Notes |\n\
         |---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|\n"
    );
    for entry in entries {
        let score = &entry.score;
        let rejects_agreed = score.agreed - score.accepts_agreed;
        let cost = score
            .cost_usd
            .map_or("-".to_string(), |cost| format!("${cost:.4}"));
        let _ = writeln!(
            text,
            "| {} | {} | {}/{total} | {} | {} | {} | {} | {} | {} | {cost} | {} | {} |",
            entry.backend,
            entry.model,
            score.answered,
            percent(score.agreed, total),
            percent(score.accepts_agreed, accepts),
            percent(rejects_agreed, rejects),
            percent(score.relations_agreed, named),
            percent(score.unsure, total),
            score.errors,
            duration(entry.wall_ms),
            entry.note,
        );
    }

    let mut relations: BTreeMap<&str, usize> = BTreeMap::new();
    for rel in labels.iter().filter_map(|label| label.rel.as_deref()) {
        *relations.entry(rel).or_default() += 1;
    }
    if relations.is_empty() {
        return text;
    }
    text.push_str("\nAccepted pairs where the judge accepted and named the same relation:\n\n");
    let names: Vec<String> = entries
        .iter()
        .map(|entry| format!("{} {}", entry.backend, entry.model))
        .collect();
    let _ = writeln!(text, "| Relation | Pairs | {} |", names.join(" | "));
    let _ = writeln!(text, "|---|---:|{}", "---:|".repeat(names.len()));
    for (rel, pairs) in relations {
        let hits = entries.iter().map(|entry| {
            let hits = entry.score.by_relation.get(rel).copied().unwrap_or(0);
            format!("{hits}/{pairs}")
        });
        let hits: Vec<String> = hits.collect();
        let _ = writeln!(text, "| {rel} | {pairs} | {} |", hits.join(" | "));
    }
    text
}

/// Where the markers are in `doc`, or why they can't be used.
fn markers(doc: &str, path: &Path) -> Result<(usize, usize), Box<dyn Error>> {
    match (doc.find(BEGIN), doc.find(END)) {
        (Some(begin), Some(end)) if begin < end => Ok((begin + BEGIN.len(), end)),
        _ => Err(format!("{} needs {BEGIN} and then {END}", path.display()).into()),
    }
}

/// One line of `--models`.
fn contestant(entry: &str, project: &Project) -> Result<Box<dyn Judge>, Box<dyn Error>> {
    let (name, model) = match entry.split_once(':') {
        Some((name, model)) => (name, Some(model)),
        None => (entry, None),
    };
    let id = JudgeId::parse(name).ok_or_else(|| {
        let names = JudgeId::ALL.map(JudgeId::as_str).join(", ");
        format!("`{name}` is not a backend; the backends are {names}")
    })?;
    let (config, vault) = (&project.config.judge, Some(project.vault.as_path()));
    Ok(match model {
        Some(model) => backend_with_model(id, model, config, vault),
        None => backend(id, config, vault),
    })
}

/// The bake-off's own database: verdicts, and how long they took.
fn open_cache(project: &Project) -> Result<Graph, Box<dyn Error>> {
    let cache = Graph::open(&project.graph.with_file_name("bakeoff.sqlite"))?;
    cache.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS wall_time (backend TEXT, model TEXT, prompt_hash TEXT,
             ms INTEGER, PRIMARY KEY(backend, model, prompt_hash));",
    )?;
    Ok(cache)
}

fn add_wall_time(
    cache: &Graph,
    id: JudgeId,
    model: &str,
    hash: &str,
    ms: u64,
) -> rusqlite::Result<()> {
    cache.connection().execute(
        "INSERT INTO wall_time VALUES (?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET ms = ms + excluded.ms",
        params![id.as_str(), model, hash, ms as i64],
    )?;
    Ok(())
}

fn wall_time(cache: &Graph, id: JudgeId, model: &str, hash: &str) -> rusqlite::Result<u64> {
    let mut statement = cache
        .connection()
        .prepare("SELECT ms FROM wall_time WHERE backend = ? AND model = ? AND prompt_hash = ?")?;
    let mut rows = statement.query(params![id.as_str(), model, hash])?;
    Ok(match rows.next()? {
        Some(row) => row.get::<_, i64>(0)? as u64,
        None => 0,
    })
}

fn answers(
    cache: &Graph,
    id: JudgeId,
    model: &str,
    hash: &str,
) -> rusqlite::Result<HashMap<String, Answer>> {
    let mut statement = cache.connection().prepare(
        "SELECT cid, verdict, rel, cost_usd FROM verdicts
         WHERE backend = ? AND model = ? AND prompt_hash = ?",
    )?;
    let rows = statement.query_map(params![id.as_str(), model, hash], |row| {
        let answer = Answer {
            verdict: row.get(1)?,
            rel: row.get(2)?,
            cost_usd: row.get(3)?,
        };
        Ok((row.get(0)?, answer))
    })?;
    rows.collect()
}

/// Opens the index for reading, as `kbx suggest --dry-run` does.
fn open_index(project: &Project, yaml: &str) -> Result<Graph, Box<dyn Error>> {
    let graph = Graph::open_read_only(&project.graph)?;
    match index_state(&graph, yaml)? {
        IndexState::Unbuilt => return Err("the vault has no index yet; run `kbx index`".into()),
        IndexState::Stale => warn!("the index is out of date; run `kbx index`"),
        IndexState::Current => {}
    }
    Ok(graph)
}

fn read_ontology(project: &Project) -> Result<(String, Ontology), Box<dyn Error>> {
    let yaml = fs::read_to_string(&project.ontology)
        .map_err(|error| format!("{}: {error}", project.ontology.display()))?;
    let ontology = Ontology::parse(&yaml)?;
    Ok((yaml, ontology))
}

fn read_file(path: &Path) -> Result<String, Box<dyn Error>> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()).into())
}

/// A pair as a judge is shown it. Its provenance stays out: an accepted pair's
/// candidate is gone, so only rejected ones would have any to show.
fn proposal(label: &Label) -> Proposal {
    Proposal {
        cid: label.cid.clone(),
        src: label.src.clone(),
        dst: label.dst.clone(),
        generators: Vec::new(),
        features: Default::default(),
        score: 0.0,
    }
}

pub fn run(options: Options) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let size = suggesting::batch_size(&project, options.batch)?;
    let (yaml, ontology) = read_ontology(&project)?;
    let graph = open_index(&project, &yaml)?;

    let labels = read_labels(&read_file(&options.labels)?, &ontology)?;
    if labels.pairs.is_empty() {
        return Err(format!("{} has no decisions", options.labels.display()).into());
    }
    if labels.undecided > 0 {
        eprintln!(
            "note: {} no decision yet",
            match labels.undecided {
                1 => "1 pair has".to_string(),
                many => format!("{many} pairs have"),
            }
        );
    }
    let doc = read_file(&options.doc)?;
    let (begin, end) = markers(&doc, &options.doc)?;

    let names = options
        .models
        .unwrap_or_else(|| LINE_UP.map(String::from).to_vec());
    let judges: Vec<Box<dyn Judge>> = names
        .iter()
        .map(|name| contestant(name, &project))
        .collect::<Result<_, _>>()?;

    let cache = open_cache(&project)?;
    let context = JudgeContext::new(ontology, &yaml);
    let proposals: Vec<Proposal> = labels.pairs.iter().map(proposal).collect();
    let overrides = &project.config.judge.overrides;
    let mut entries = Vec::new();
    let mut spent = 0.0;

    let tokio = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for judge in judges {
        let found = views(&graph, &project.vault, &proposals, overrides, judge.id())?;
        if found.unreadable > 0 {
            eprintln!(
                "warning: {} labelled pairs have a note that isn't in this vault",
                found.unreadable
            );
        }
        if found.pinned > 0 {
            eprintln!(
                "note: {} pairs kept from {} by judge.overrides",
                found.pinned,
                judge.id()
            );
        }
        let (id, model) = (judge.id(), judge.model().to_string());
        let turn = Budgeted {
            size,
            budget_usd: options.budget_usd,
            spent: &mut spent,
        };
        let note = tokio.block_on(take_turn(judge, &cache, &found.ready, &context, turn))?;
        let hash = &context.prompt_hash;
        entries.push(Entry {
            backend: id,
            score: score(&labels.pairs, &answers(&cache, id, &model, hash)?),
            wall_ms: wall_time(&cache, id, &model, hash)?,
            model,
            note,
        });
    }

    if entries.iter().all(|entry| entry.score.answered == 0) {
        return Err("no judge answered, so the report is unchanged".into());
    }
    let report = render(&labels.pairs, &entries);
    print!("{report}");
    let written = format!("{}\n{report}{}", &doc[..begin], &doc[end..]);
    fs::write(&options.doc, written)
        .map_err(|error| format!("{}: {error}", options.doc.display()))?;
    Ok(ExitCode::SUCCESS)
}

/// How one judge's turn is limited, and what the turns have spent so far.
struct Budgeted<'a> {
    size: usize,
    budget_usd: Option<f64>,
    spent: &'a mut f64,
}

/// Gives `judge` its turn. Returns why it couldn't finish, if it couldn't. A
/// judge with nothing left to answer isn't probed, so a re-run needs none of
/// them to be up.
async fn take_turn(
    judge: Box<dyn Judge>,
    cache: &Graph,
    views: &[CandidateView],
    context: &JudgeContext,
    budgeted: Budgeted<'_>,
) -> Result<String, Box<dyn Error>> {
    if pending(judge.as_ref(), cache, views, context)?.is_empty() {
        return Ok(String::new());
    }
    let (id, model) = (judge.id(), judge.model().to_string());
    let left = budgeted.budget_usd.map(|budget| budget - *budgeted.spent);
    let selection = first_ready(vec![judge], left).await;
    *budgeted.spent += selection.probes_usd;
    let Some(judge) = selection.chosen else {
        let found = selection.passed_over.first();
        let reason = found.map_or(OVER_BUDGET.to_string(), |found| found.1.to_string());
        eprintln!("warning: {id} ({model}) skipped: {reason}");
        return Ok(reason);
    };
    let plan = Plan {
        size: budgeted.size,
        budget_usd: budgeted.budget_usd,
        spent_usd: *budgeted.spent,
    };
    let started = Instant::now();
    let tally = judge_all(judge.as_ref(), cache, views, context, plan).await?;
    let ms = started.elapsed().as_millis() as u64;
    add_wall_time(cache, id, &model, &context.prompt_hash, ms)?;
    *budgeted.spent = tally.cost_usd.unwrap_or(*budgeted.spent);
    eprintln!(
        "{id} ({model}): {} accept, {} reject, {} unsure, {} unjudged, {} error, {} already judged",
        tally.accepted, tally.rejected, tally.unsure, tally.unjudged, tally.errored, tally.cached
    );
    Ok(match tally.halted {
        true => OVER_BUDGET.to_string(),
        false => String::new(),
    })
}

/// A candidate on its way into a `--prepare` file.
struct Choice {
    proposal: Proposal,
    view: CandidateView,
}

impl Choice {
    /// What makes a pair like others: who found it, how strongly, and between
    /// which types of note.
    fn stratum(&self) -> (Vec<&str>, usize, (String, String)) {
        let found_by = self.proposal.generators.iter().map(String::as_str);
        let found_by = found_by.filter(|name| *name != RE_RANKER).collect();
        // Scores run from 0 to 1, so this is thirds.
        let band = (self.proposal.score * 3.0).min(2.0) as usize;
        let types = ordered(&self.view.src.class, &self.view.dst.class);
        (found_by, band, types)
    }

    fn pair(self, synthetic: bool) -> Pair {
        let excerpt = |note: &NoteView| match synthetic {
            true => format!("Synthetic stand-in for a {} note.", note.class),
            false => {
                let words = note.excerpt.split_whitespace().take(EXCERPT_WORDS);
                words.collect::<Vec<_>>().join(" ")
            }
        };
        let found_by = self.proposal.generators.into_iter();
        Pair {
            src_excerpt: Some(excerpt(&self.view.src)),
            dst_excerpt: Some(excerpt(&self.view.dst)),
            src_type: Some(self.view.src.class),
            dst_type: Some(self.view.dst.class),
            src: self.proposal.src,
            dst: self.proposal.dst,
            cid: self.proposal.cid,
            generators: found_by.filter(|name| name != RE_RANKER).collect(),
            score: Some(self.proposal.score),
            ..Pair::default()
        }
    }
}

/// Takes `count` pairs, one from each stratum in turn, so that a rare kind of
/// pair counts for as much as a common one. `choices` are in cid order, which
/// makes each stratum's order arbitrary and the same every time.
fn stratify(choices: Vec<Choice>, count: usize) -> Vec<Choice> {
    let mut strata: BTreeMap<_, VecDeque<usize>> = BTreeMap::new();
    for (index, choice) in choices.iter().enumerate() {
        strata.entry(choice.stratum()).or_default().push_back(index);
    }
    let mut uses: HashMap<&str, usize> = HashMap::new();
    let mut chosen = Vec::new();
    for limit in [NOTE_LIMIT, usize::MAX] {
        let mut moved = true;
        while moved && chosen.len() < count {
            moved = false;
            for stratum in strata.values_mut() {
                if chosen.len() == count {
                    break;
                }
                let free = |index: &usize| {
                    let proposal = &choices[*index].proposal;
                    let used = |id: &String| uses.get(id.as_str()).copied().unwrap_or(0);
                    used(&proposal.src) < limit && used(&proposal.dst) < limit
                };
                let Some(at) = stratum.iter().position(free) else {
                    continue;
                };
                let index = stratum.remove(at).expect("position found it");
                let proposal = &choices[index].proposal;
                *uses.entry(&proposal.src).or_default() += 1;
                *uses.entry(&proposal.dst).or_default() += 1;
                chosen.push(index);
                moved = true;
            }
        }
    }
    chosen.sort_unstable();
    let chosen: HashSet<usize> = chosen.into_iter().collect();
    let kept = choices.into_iter().enumerate();
    let kept = kept.filter(|(index, _)| chosen.contains(index));
    kept.map(|(_, choice)| choice).collect()
}

pub fn prepare(options: Preparing) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let (yaml, ontology) = read_ontology(&project)?;
    let graph = open_index(&project, &yaml)?;
    let search = QmdSearch::new(
        qmd::program(&project),
        project.qmd_index(),
        project.config.qmd.searched(),
        &graph,
    )?;
    let chosen = generators(options.generators.as_deref(), Some(Box::new(search)))?;
    let scope = Scope {
        vault: &project.vault,
        ontology: &ontology,
        note: None,
    };
    // The usual budget keeps the best scores, and the point is all of them.
    let everything = Budget {
        per_note: usize::MAX,
        per_run: usize::MAX,
    };
    let run = suggest(&graph, &chosen, &scope, everything)?;
    for (name, error) in &run.skipped {
        eprintln!("warning: {name} skipped: {error}");
    }

    // Every judge has to be able to see a pair for the comparison to be fair.
    // A pin to one backend keeps a note from the other of these two, and a pin
    // to anything else keeps it from both.
    let overrides = &project.config.judge.overrides;
    let vault = project.vault.as_path();
    let local = views(&graph, vault, &run.candidates, overrides, JudgeId::Ollama)?;
    let local: HashSet<String> = local.ready.into_iter().map(|view| view.cid).collect();
    let cloud = views(
        &graph,
        vault,
        &run.candidates,
        overrides,
        JudgeId::ClaudeCli,
    )?;
    let mut seen: HashMap<String, CandidateView> = cloud
        .ready
        .into_iter()
        .filter(|view| local.contains(&view.cid))
        .map(|view| (view.cid.clone(), view))
        .collect();
    let mut choices: Vec<Choice> = run
        .candidates
        .into_iter()
        .filter_map(|proposal| {
            let view = seen.remove(&proposal.cid)?;
            Some(Choice { proposal, view })
        })
        .collect();
    choices.sort_by(|a, b| a.proposal.cid.cmp(&b.proposal.cid));
    let found = choices.len();
    if found == 0 {
        return Err("the generators found no pairs every judge may see".into());
    }

    let chosen = stratify(choices, options.pairs);
    let mut strata: BTreeMap<String, usize> = BTreeMap::new();
    for choice in &chosen {
        let (found_by, band, (first, second)) = choice.stratum();
        let band = ["low", "mid", "high"][band];
        let stratum = format!("{} {band} {first}-{second}", found_by.join("+"));
        *strata.entry(stratum).or_default() += 1;
    }
    let rows = chosen.into_iter().map(|choice| {
        let pair = choice.pair(options.synthetic);
        serde_json::to_string(&pair).map(|line| line + "\n")
    });
    let text = rows.collect::<Result<String, _>>()?;
    match &options.out {
        Some(path) => {
            fs::write(path, &text).map_err(|error| format!("{}: {error}", path.display()))?;
        }
        None => print!("{text}"),
    }
    eprintln!(
        "{} of {found} chosen, for {} wanted",
        crate::plural(text.lines().count(), "pair"),
        options.pairs
    );
    for (stratum, pairs) in strata {
        eprintln!("{pairs:>4}  {stratum}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Stores the pairs in `file` as candidates. Nothing else in `kbx review`'s
/// queue changes, and no judge has seen them: a person labels them blind.
pub fn enqueue(file: &Path) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let graph = Graph::open(&project.graph)?;
    let mut candidates = Vec::new();
    for (index, line) in read_file(file)?.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: Pair =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        candidates.push(Proposal {
            cid: cid(&row.src, &row.dst),
            src: row.src,
            dst: row.dst,
            generators: row.generators,
            features: Default::default(),
            score: row.score.unwrap_or(0.0),
        });
    }
    let total = candidates.len();
    let cids: Vec<String> = candidates.iter().map(|c| c.cid.clone()).collect();
    let run = Run {
        candidates,
        dropped: 0,
        locked: 0,
        over_budget: 0,
        skipped: Vec::new(),
    };
    store(&graph, &run)?;
    let mut statement = graph
        .connection()
        .prepare("SELECT 1 FROM candidates WHERE cid = ?")?;
    let mut stored = 0;
    for cid in &cids {
        stored += usize::from(statement.exists([cid])?);
    }
    println!("{} in the queue", crate::plural(stored, "pair"));
    if stored < total {
        eprintln!(
            "warning: {} left out: a note isn't in this vault, or the pair is linked or suppressed",
            total - stored
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use kbx_core::EXAMPLE_ONTOLOGY;

    use super::*;

    fn read(rows: &[&str]) -> Labels {
        let ontology = Ontology::parse(EXAMPLE_ONTOLOGY).unwrap();
        read_labels(&rows.join("\n"), &ontology).unwrap()
    }

    #[test]
    fn the_latest_decision_on_a_pair_stands_in_either_direction() {
        let labels = read(&[
            r#"{"src":"a","dst":"b","rel":"see_also","decision":"reject","decided_at":1}"#,
            r#"{"src":"b","dst":"a","rel":"part_of","decision":"accept","decided_at":2}"#,
        ]);
        assert_eq!(labels.pairs.len(), 1);
        assert!(labels.pairs[0].accepted);
        assert_eq!(labels.pairs[0].rel.as_deref(), Some("part_of"));
        assert_eq!(labels.pairs[0].src, "a");
    }

    #[test]
    fn on_a_tie_the_accept_stands_whichever_comes_first() {
        let accept = r#"{"src":"a","dst":"b","rel":"part_of","decision":"accept"}"#;
        let reject = r#"{"src":"a","dst":"b","rel":"see_also","decision":"reject"}"#;
        assert!(read(&[accept, reject]).pairs[0].accepted);
        assert!(read(&[reject, accept]).pairs[0].accepted);
    }

    #[test]
    fn an_inverse_counts_as_its_relation_and_a_rejection_names_none() {
        let labels = read(&[
            r#"{"src":"a","dst":"b","rel":"has_part","decision":"accept"}"#,
            r#"{"src":"c","dst":"d","rel":"see_also","decision":"reject"}"#,
        ]);
        let rel = |src: &str| {
            let label = labels.pairs.iter().find(|label| label.src == src);
            label.unwrap().rel.clone()
        };
        assert_eq!(rel("a").as_deref(), Some("part_of"));
        assert_eq!(rel("c"), None);
    }

    #[test]
    fn a_relation_outside_the_ontology_is_refused_by_line() {
        let ontology = Ontology::parse(EXAMPLE_ONTOLOGY).unwrap();
        let row = r#"{"src":"a","dst":"b","rel":"likes","decision":"accept"}"#;
        let error = read_labels(row, &ontology).unwrap_err().to_string();
        assert!(error.contains("line 1"), "{error}");
    }

    #[test]
    fn an_unanswered_pair_agrees_with_nothing_and_an_error_is_no_answer() {
        let labels = read(&[
            r#"{"src":"a","dst":"b","rel":"part_of","decision":"accept"}"#,
            r#"{"src":"c","dst":"d","rel":"none","decision":"reject"}"#,
        ]);
        let error = Answer {
            verdict: JUDGE_ERROR.to_string(),
            rel: None,
            cost_usd: Some(0.5),
        };
        let answers = HashMap::from([(labels.pairs[0].cid.clone(), error)]);
        let score = score(&labels.pairs, &answers);
        assert_eq!((score.answered, score.agreed, score.errors), (0, 0, 1));
        assert_eq!(score.cost_usd, Some(0.5));
    }
}
