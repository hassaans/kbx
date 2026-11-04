use std::collections::HashSet;
use std::env;
use std::path::Path;

use kbx_core::{Graph, JudgeBackend, JudgeConfig};
use sysinfo::System;
use tracing::warn;

use crate::ledger::Ledger;
use crate::verdict::{judged, promote, store, store_error};
use crate::{
    Availability, CandidateView, ClaudeCli, CodexCli, Decision, Judge, JudgeContext, JudgeError,
    JudgeId, LlamaServer, Ollama, ProbeCache, Verdict, claude, codex, llama_server, ollama,
};

/// How [`judge_all`] goes about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// Pairs per call.
    pub size: usize,
    /// The most this run may spend, in USD.
    pub budget_usd: Option<f64>,
    /// What the run has already spent, in USD: the probes, see
    /// [`Selection::probes_usd`], and any backend that went before this one.
    pub spent_usd: f64,
}

/// What [`first_ready`] came to.
#[derive(Default)]
pub struct Selection {
    /// The first backend that's ready.
    pub chosen: Option<Box<dyn Judge>>,
    /// What was found of the ones passed over.
    pub passed_over: Vec<(JudgeId, Availability)>,
    /// What the probes cost, in USD, those of the ones passed over included.
    pub probes_usd: f64,
    /// Whether the budget stopped a probe from being made.
    pub halted: bool,
}

/// What [`judge_all`] came to.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tally {
    /// Pairs accepted.
    pub accepted: usize,
    /// Pairs rejected.
    pub rejected: usize,
    /// Pairs left for a person.
    pub unsure: usize,
    /// Pairs that failed twice, now marked `error`.
    pub errored: usize,
    /// Pairs that never got an answer, because the run stopped first.
    pub unjudged: usize,
    /// Pairs skipped, already having a verdict under this exact key.
    pub cached: usize,
    /// Whether the budget ended the run.
    pub halted: bool,
    /// What the run has spent by now, [`Plan::spent_usd`] and failed calls
    /// included, when the backend says.
    pub cost_usd: Option<f64>,
    /// Input tokens served from the provider's cache.
    pub cached_tokens: i64,
}

/// The backend called `id`, set up from `kbx.toml` and the environment. Probes
/// are cached in the vault at `vault`, if there is one.
pub fn backend(id: JudgeId, config: &JudgeConfig, vault: Option<&Path>) -> Box<dyn Judge> {
    build(id, None, config, vault)
}

/// [`backend`], asking for `model` whatever the environment and `kbx.toml` say
/// and however little memory is free: comparing models means knowing which one
/// answered.
pub fn backend_with_model(
    id: JudgeId,
    model: &str,
    config: &JudgeConfig,
    vault: Option<&Path>,
) -> Box<dyn Judge> {
    build(id, Some(model), config, vault)
}

fn build(
    id: JudgeId,
    named: Option<&str>,
    config: &JudgeConfig,
    vault: Option<&Path>,
) -> Box<dyn Judge> {
    let cache = ProbeCache::new(vault);
    let model = |variable: &str, backend: &JudgeBackend, default: &str| match named {
        Some(model) => model.to_string(),
        None => model(variable, backend, default),
    };
    match id {
        JudgeId::ClaudeCli => {
            let model = model("KBX_CLAUDE_MODEL", &config.claude, claude::DEFAULT_MODEL);
            let rates = config.claude.rates.unwrap_or(claude::DEFAULT_RATES);
            Box::new(ClaudeCli::new(model, rates, cache))
        }
        JudgeId::CodexCli => {
            let model = model("KBX_CODEX_MODEL", &config.codex, codex::DEFAULT_MODEL);
            let rates = config.codex.rates.unwrap_or(codex::DEFAULT_RATES);
            Box::new(CodexCli::new(model, rates, cache))
        }
        JudgeId::Ollama => {
            let configured = || config.ollama.model.clone();
            let default = || ollama::DEFAULT_MODEL.to_string();
            let chosen = named.map(String::from);
            let model = chosen
                .or_else(|| from_env("KBX_OLLAMA_MODEL"))
                .unwrap_or_else(|| {
                    ollama::fit(configured().unwrap_or_else(default), free_memory())
                });
            let host = from_env("OLLAMA_HOST").or_else(|| config.ollama.host.clone());
            Box::new(Ollama::new(model, host.as_deref()))
        }
        JudgeId::LlamaServer => {
            let backend = &config.llama_server;
            let model = model(
                "KBX_LLAMA_SERVER_MODEL",
                backend,
                llama_server::DEFAULT_MODEL,
            );
            let host = from_env("KBX_LLAMA_SERVER_HOST").or_else(|| backend.host.clone());
            Box::new(LlamaServer::new(model, host.as_deref()))
        }
    }
}

/// Every backend in `judge.order` that this build has.
pub fn backends(config: &JudgeConfig, vault: Option<&Path>) -> Vec<Box<dyn Judge>> {
    let ids = config.order.iter().filter_map(|name| JudgeId::parse(name));
    ids.map(|id| backend(id, config, vault)).collect()
}

/// The backends `[judge.overrides]` names, once each. `none` isn't one: it
/// keeps a folder from every judge. Any other name this build doesn't know
/// keeps it from every judge too, which is the safe reading of a typo.
pub fn pinned_backends(config: &JudgeConfig, vault: Option<&Path>) -> Vec<Box<dyn Judge>> {
    let mut ids = Vec::new();
    for name in config.overrides.values() {
        match JudgeId::parse(name) {
            Some(id) if !ids.contains(&id) => ids.push(id),
            None if name != "none" => warn!("judge.overrides names no backend `{name}`"),
            _ => {}
        }
    }
    ids.into_iter()
        .map(|id| backend(id, config, vault))
        .collect()
}

fn model(variable: &str, config: &JudgeBackend, default: &str) -> String {
    from_env(variable)
        .or_else(|| config.model.clone())
        .unwrap_or_else(|| default.to_string())
}

fn from_env(variable: &str) -> Option<String> {
    env::var(variable).ok().filter(|value| !value.is_empty())
}

/// What the machine has free, in bytes, if it can say.
fn free_memory() -> Option<u64> {
    let mut system = System::new().ok()?;
    system.refresh_memory();
    Some(system.available_memory()).filter(|free| *free > 0)
}

/// Checks on `backend` and adds what that cost to `spent`, or returns `None` if
/// `budget_usd` can't cover it: a probe is a paid call like any other.
pub(crate) async fn probe(
    backend: &dyn Judge,
    spent: &mut f64,
    budget_usd: Option<f64>,
) -> Option<Availability> {
    let forecast = *spent + backend.probe_quote();
    if budget_usd.is_some_and(|budget| forecast > budget) {
        return None;
    }
    let availability = backend.available().await;
    *spent += backend.probe_spend();
    Some(availability)
}

/// Goes down `backends`, stopping at the first that's ready, or at one whose
/// probe `budget_usd` can't cover: a probe is a paid call like any other.
pub async fn first_ready(backends: Vec<Box<dyn Judge>>, budget_usd: Option<f64>) -> Selection {
    let mut selection = Selection::default();
    for backend in backends {
        match probe(backend.as_ref(), &mut selection.probes_usd, budget_usd).await {
            None => {
                selection.halted = true;
                break;
            }
            Some(Availability::Ready) => {
                selection.chosen = Some(backend);
                break;
            }
            Some(availability) => selection.passed_over.push((backend.id(), availability)),
        }
    }
    selection
}

/// The pairs of `views` with no verdict yet under `(cid, backend, model,
/// prompt_hash)`. Those are the only ones worth paying for: change the ontology
/// or the prompt and the hash changes, so every pair is asked again.
pub fn pending(
    judge: &dyn Judge,
    g: &Graph,
    views: &[CandidateView],
    ctx: &JudgeContext,
) -> rusqlite::Result<Vec<CandidateView>> {
    let done = judged(g, judge.id(), judge.model(), &ctx.prompt_hash)?;
    let open = views.iter().filter(|view| !done.contains(&view.cid));
    Ok(open.cloned().collect())
}

/// Judges the [`pending`] pairs of `views` in batches, storing each batch as it
/// comes, so that nothing already paid for depends on how the run ends.
///
/// A batch that fails is retried once, in halves, and a half that fails again
/// is marked `error`, for the next run to retry. The next batch runs regardless,
/// unless the login went, which would fail them all, or the next call would
/// take the run over [`Plan::budget_usd`].
pub async fn judge_all(
    judge: &dyn Judge,
    g: &Graph,
    views: &[CandidateView],
    ctx: &JudgeContext,
    plan: Plan,
) -> Result<Tally, JudgeError> {
    let open = pending(judge, g, views, ctx)?;
    let waiting: HashSet<&str> = open.iter().map(|view| view.cid.as_str()).collect();
    let answered = views.iter().map(|view| view.cid.as_str());
    let answered: Vec<&str> = answered.filter(|cid| !waiting.contains(cid)).collect();
    promote(g, judge.id(), judge.model(), &ctx.prompt_hash, &answered)?;
    let mut session = Session {
        judge,
        g,
        ctx,
        ledger: Ledger::new(plan.budget_usd, plan.spent_usd),
        tally: Tally {
            cached: views.len() - open.len(),
            ..Tally::default()
        },
    };
    let mut batches = open.chunks(plan.size);
    for batch in batches.by_ref() {
        if !session.batch(batch).await? {
            break;
        }
    }
    let mut tally = session.tally;
    tally.unjudged += batches.map(<[_]>::len).sum::<usize>();
    tally.cost_usd = session.ledger.spent();
    Ok(tally)
}

/// What became of one call.
enum Call {
    /// Its verdicts are stored.
    Stored,
    /// It failed, having cost this much.
    Failed(JudgeError, Option<f64>),
    /// It wasn't made or the login went, and no other should be made either.
    Stopped,
}

struct Session<'a> {
    judge: &'a dyn Judge,
    g: &'a Graph,
    ctx: &'a JudgeContext,
    ledger: Ledger,
    tally: Tally,
}

impl Session<'_> {
    /// Runs one batch, with its retry. Returns whether the run should go on.
    async fn batch(&mut self, batch: &[CandidateView]) -> Result<bool, JudgeError> {
        let error = match self.call(batch).await? {
            Call::Stored => return Ok(true),
            Call::Failed(error, _) => error,
            Call::Stopped => {
                self.tally.unjudged += batch.len();
                return Ok(false);
            }
        };
        warn!("{error}; retrying at half size");
        let mut halves = batch.chunks(batch.len().div_ceil(2));
        for half in halves.by_ref() {
            match self.call(half).await? {
                Call::Stored => {}
                Call::Failed(error, cost) => self.mark_error(half, &error, cost)?,
                Call::Stopped => {
                    self.tally.unjudged += half.len();
                    self.tally.unjudged += halves.map(<[_]>::len).sum::<usize>();
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Makes one call, if the budget allows it, and records what it comes to.
    async fn call(&mut self, batch: &[CandidateView]) -> Result<Call, JudgeError> {
        let quote = self.judge.quote(batch, self.ctx);
        if !self.ledger.allows(batch.len(), quote) {
            self.tally.halted = true;
            return Ok(Call::Stopped);
        }
        let (error, cost) = match self.judge.judge(batch, self.ctx).await {
            Ok(verdicts) => {
                self.ledger.record(batch.len(), total_cost(&verdicts));
                self.store(&verdicts)?;
                return Ok(Call::Stored);
            }
            Err(error) => error.unbilled(),
        };
        self.ledger.record(batch.len(), cost);
        match error {
            JudgeError::NotLoggedIn(_) => {
                warn!("{error}; stopping");
                Ok(Call::Stopped)
            }
            JudgeError::Database(_) => Err(error),
            error => Ok(Call::Failed(error, cost)),
        }
    }

    fn store(&mut self, verdicts: &[Verdict]) -> rusqlite::Result<()> {
        let (id, model, hash) = (self.judge.id(), self.judge.model(), &self.ctx.prompt_hash);
        store(self.g, id, model, hash, verdicts)?;
        for verdict in verdicts {
            match verdict.decision {
                Decision::Accept => self.tally.accepted += 1,
                Decision::Reject => self.tally.rejected += 1,
                Decision::Unsure => self.tally.unsure += 1,
            }
            self.tally.cached_tokens += verdict.cached_tokens.unwrap_or(0);
        }
        Ok(())
    }

    fn mark_error(
        &mut self,
        batch: &[CandidateView],
        error: &JudgeError,
        cost: Option<f64>,
    ) -> rusqlite::Result<()> {
        warn!("{error}; marking {} pairs as error", batch.len());
        let (id, model, hash) = (self.judge.id(), self.judge.model(), &self.ctx.prompt_hash);
        store_error(self.g, id, model, hash, batch, &error.to_string(), cost)?;
        self.tally.errored += batch.len();
        Ok(())
    }
}

/// What a batch's rows add up to. `None` if none of them was priced.
fn total_cost(verdicts: &[Verdict]) -> Option<f64> {
    let costs = verdicts.iter().filter_map(|verdict| verdict.cost_usd);
    costs.reduce(|total, cost| total + cost)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use kbx_core::{EXAMPLE_ONTOLOGY, JUDGE_ERROR, Ontology};
    use tempfile::TempDir;

    use super::*;
    use crate::{NoteView, SYSTEM_PROMPT};

    type Answer = dyn Fn(usize, &[CandidateView]) -> Result<Vec<Verdict>, JudgeError> + Send + Sync;

    /// A backend that counts its calls. `answer` gets the call's number, from 0,
    /// and the batch.
    struct Fake {
        model: &'static str,
        quote: f64,
        probe_quote: f64,
        probe_cost: f64,
        ready: bool,
        probes: Arc<Mutex<usize>>,
        answer: Box<Answer>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl Fake {
        fn new(
            answer: impl Fn(usize, &[CandidateView]) -> Result<Vec<Verdict>, JudgeError>
            + Send
            + Sync
            + 'static,
        ) -> Fake {
            Fake {
                model: "fake",
                quote: 0.0,
                probe_quote: 0.0,
                probe_cost: 0.0,
                ready: true,
                probes: Arc::default(),
                answer: Box::new(answer),
                calls: Mutex::default(),
            }
        }

        /// Answers everything, at `cost` a call.
        fn costing(cost: f64) -> Fake {
            Fake::new(move |_, batch| Ok(rejected(batch, Some(cost))))
        }

        /// The size of each call so far.
        fn sizes(&self) -> Vec<usize> {
            let calls = self.calls.lock().unwrap();
            calls.iter().map(Vec::len).collect()
        }
    }

    #[async_trait]
    impl Judge for Fake {
        fn id(&self) -> JudgeId {
            JudgeId::ClaudeCli
        }

        fn model(&self) -> &str {
            self.model
        }

        async fn available(&self) -> Availability {
            *self.probes.lock().unwrap() += 1;
            match self.ready {
                true => Availability::Ready,
                false => Availability::NotLoggedIn,
            }
        }

        async fn judge(
            &self,
            batch: &[CandidateView],
            _ctx: &JudgeContext,
        ) -> Result<Vec<Verdict>, JudgeError> {
            let number = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(batch.iter().map(|view| view.cid.clone()).collect());
                calls.len() - 1
            };
            (self.answer)(number, batch)
        }

        fn quote(&self, _batch: &[CandidateView], _ctx: &JudgeContext) -> f64 {
            self.quote
        }

        fn probe_quote(&self) -> f64 {
            self.probe_quote
        }

        fn probe_spend(&self) -> f64 {
            self.probe_cost * *self.probes.lock().unwrap() as f64
        }
    }

    /// A rejection of every pair, with `cost` shared out across them.
    fn rejected(batch: &[CandidateView], cost: Option<f64>) -> Vec<Verdict> {
        answered(batch, Decision::Reject, cost)
    }

    fn answered(batch: &[CandidateView], decision: Decision, cost: Option<f64>) -> Vec<Verdict> {
        let verdict = |view: &CandidateView| Verdict {
            cid: view.cid.clone(),
            decision,
            rel: "none".to_string(),
            confidence: 0.5,
            rationale: "r".to_string(),
            cost_usd: cost.map(|total| total / batch.len() as f64),
            cached_tokens: None,
        };
        batch.iter().map(verdict).collect()
    }

    fn failure() -> JudgeError {
        JudgeError::Malformed(JudgeId::ClaudeCli, "no structured_output".to_string())
    }

    fn views(count: usize) -> Vec<CandidateView> {
        let note = || NoteView {
            id: "n".to_string(),
            class: "Concept".to_string(),
            title: "t".to_string(),
            excerpt: "e".to_string(),
        };
        let view = |n| CandidateView {
            cid: format!("cid-{n:02}"),
            src: note(),
            dst: note(),
            why_proposed: Vec::new(),
        };
        (0..count).map(view).collect()
    }

    fn graph() -> (TempDir, Graph) {
        let dir = TempDir::new().unwrap();
        let graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
        (dir, graph)
    }

    fn context() -> JudgeContext {
        JudgeContext::new(Ontology::parse(EXAMPLE_ONTOLOGY).unwrap(), EXAMPLE_ONTOLOGY)
    }

    fn plan(size: usize) -> Plan {
        Plan {
            size,
            budget_usd: None,
            spent_usd: 0.0,
        }
    }

    fn budget(size: usize, usd: f64) -> Plan {
        Plan {
            size,
            budget_usd: Some(usd),
            spent_usd: 0.0,
        }
    }

    /// The stored rows with this verdict, by cid.
    fn rows(g: &Graph, verdict: &str) -> Vec<String> {
        let connection = g.connection();
        let mut statement = connection
            .prepare("SELECT cid FROM verdicts WHERE verdict = ? ORDER BY cid")
            .unwrap();
        let found = statement.query_map([verdict], |row| row.get(0)).unwrap();
        found.map(Result::unwrap).collect()
    }

    #[tokio::test]
    async fn pairs_go_out_in_batches_of_the_planned_size() {
        let (_dir, g) = graph();
        let fake = Fake::costing(0.0);
        let tally = judge_all(&fake, &g, &views(25), &context(), plan(10))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [10, 10, 5]);
        assert_eq!(tally.rejected, 25);
    }

    // AC1.
    #[tokio::test]
    async fn an_unchanged_rerun_makes_no_calls_and_a_changed_prompt_makes_them_again() {
        let (_dir, g) = graph();
        let pairs = views(12);
        let ctx = context();
        let first = Fake::costing(0.01);
        judge_all(&first, &g, &pairs, &ctx, plan(10)).await.unwrap();
        assert_eq!(first.sizes(), [10, 2]);

        let again = Fake::costing(0.01);
        let tally = judge_all(&again, &g, &pairs, &ctx, plan(10)).await.unwrap();
        assert!(again.sizes().is_empty());
        assert_eq!(tally.cached, 12);
        assert_eq!(tally.cost_usd, None);

        let ontology = Ontology::parse(EXAMPLE_ONTOLOGY).unwrap();
        let prompt = format!("{SYSTEM_PROMPT}\nOne more rule.\n");
        let edited = JudgeContext::with_prompt(&prompt, ontology, EXAMPLE_ONTOLOGY);
        let changed = Fake::costing(0.01);
        let tally = judge_all(&changed, &g, &pairs, &edited, plan(10))
            .await
            .unwrap();
        assert_eq!(changed.sizes(), [10, 2]);
        assert_eq!(tally.cached, 0);
    }

    #[tokio::test]
    async fn every_part_of_the_key_counts() {
        let (_dir, g) = graph();
        let pairs = views(3);
        let ctx = context();
        judge_all(&Fake::costing(0.0), &g, &pairs, &ctx, plan(10))
            .await
            .unwrap();

        let mut other_model = Fake::costing(0.0);
        other_model.model = "other";
        judge_all(&other_model, &g, &pairs, &ctx, plan(10))
            .await
            .unwrap();
        assert_eq!(other_model.sizes(), [3]);

        let yaml = format!("{EXAMPLE_ONTOLOGY}\n# edited\n");
        let ontology = Ontology::parse(&yaml).unwrap();
        let other_ontology = Fake::costing(0.0);
        let edited = JudgeContext::new(ontology, &yaml);
        judge_all(&other_ontology, &g, &pairs, &edited, plan(10))
            .await
            .unwrap();
        assert_eq!(other_ontology.sizes(), [3]);
    }

    #[tokio::test]
    async fn only_new_pairs_are_judged() {
        let (_dir, g) = graph();
        let ctx = context();
        judge_all(&Fake::costing(0.0), &g, &views(3), &ctx, plan(10))
            .await
            .unwrap();
        let fake = Fake::costing(0.0);
        let tally = judge_all(&fake, &g, &views(5), &ctx, plan(10))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [2]);
        assert_eq!(tally.cached, 3);
    }

    #[tokio::test]
    async fn an_unsure_verdict_is_an_answer_and_is_not_asked_again() {
        let (_dir, g) = graph();
        let ctx = context();
        let unsure = Fake::new(|_, batch| Ok(answered(batch, Decision::Unsure, None)));
        judge_all(&unsure, &g, &views(2), &ctx, plan(10))
            .await
            .unwrap();
        let again = Fake::costing(0.0);
        judge_all(&again, &g, &views(2), &ctx, plan(10))
            .await
            .unwrap();
        assert!(again.sizes().is_empty());
    }

    // AC2.
    #[tokio::test]
    async fn the_budget_stops_before_the_next_batch_would_cross_it() {
        let (_dir, g) = graph();
        let fake = Fake::costing(0.04);
        let tally = judge_all(&fake, &g, &views(50), &context(), budget(10, 0.10))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [10, 10]);
        assert!(tally.halted);
        assert_eq!(tally.rejected, 20);
        assert_eq!(tally.unjudged, 30);
        assert!((tally.cost_usd.unwrap() - 0.08).abs() < 1e-9);
        assert_eq!(rows(&g, "reject").len(), 20);
    }

    #[tokio::test]
    async fn the_first_batch_goes_by_the_quote() {
        let (_dir, g) = graph();
        let mut fake = Fake::costing(0.20);
        fake.quote = 0.20;
        let tally = judge_all(&fake, &g, &views(10), &context(), budget(10, 0.10))
            .await
            .unwrap();
        assert!(fake.sizes().is_empty());
        assert!(tally.halted);
        assert_eq!(tally.unjudged, 10);
        assert_eq!(tally.cost_usd, None);
    }

    #[tokio::test]
    async fn a_shorter_last_batch_is_forecast_by_its_size() {
        let (_dir, g) = graph();
        let fake = Fake::new(|_, batch| Ok(rejected(batch, Some(0.004 * batch.len() as f64))));
        let tally = judge_all(&fake, &g, &views(22), &context(), budget(10, 0.09))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [10, 10, 2]);
        assert!(!tally.halted);
    }

    #[tokio::test]
    async fn a_failure_that_was_billed_counts_against_the_budget() {
        let (_dir, g) = graph();
        let fake = Fake::new(|_, _| Err(failure().billed(Some(0.06))));
        let tally = judge_all(&fake, &g, &views(20), &context(), budget(10, 0.10))
            .await
            .unwrap();
        // The batch cost $0.06, and a half at that rate would cost $0.03. The
        // first fits; the second, at the half's dearer rate, doesn't.
        assert_eq!(fake.sizes(), [10, 5]);
        assert!(tally.halted);
        assert!(tally.cost_usd.unwrap() <= 0.12 + 1e-9);
        assert_eq!(tally.errored, 5);
        assert_eq!(tally.unjudged, 15);
    }

    #[tokio::test]
    async fn a_second_run_has_a_budget_of_its_own_and_picks_up_where_the_first_stopped() {
        let (_dir, g) = graph();
        let pairs = views(30);
        let ctx = context();
        judge_all(&Fake::costing(0.04), &g, &pairs, &ctx, budget(10, 0.10))
            .await
            .unwrap();
        let fake = Fake::costing(0.04);
        let tally = judge_all(&fake, &g, &pairs, &ctx, budget(10, 0.10))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [10]);
        assert_eq!(tally.cached, 20);
        assert!(!tally.halted);
    }

    #[tokio::test]
    async fn a_probe_the_budget_cant_cover_is_not_made() {
        let mut fake = Fake::costing(0.0);
        fake.probe_quote = 0.001;
        let probes = fake.probes.clone();
        let selection = first_ready(vec![Box::new(fake)], Some(0.0)).await;
        assert_eq!(*probes.lock().unwrap(), 0);
        assert!(selection.halted);
        assert!(selection.chosen.is_none());
    }

    #[tokio::test]
    async fn every_probe_counts_and_opens_the_ledger() {
        let mut logged_out = Fake::costing(0.0);
        logged_out.ready = false;
        logged_out.probe_cost = 0.03;
        let mut ready = Fake::costing(0.04);
        ready.probe_cost = 0.03;
        let selection = first_ready(vec![Box::new(logged_out), Box::new(ready)], Some(1.0)).await;
        assert!((selection.probes_usd - 0.06).abs() < 1e-9);
        assert_eq!(selection.passed_over.len(), 1);

        let (_dir, g) = graph();
        let judge = selection.chosen.unwrap();
        let plan = Plan {
            spent_usd: selection.probes_usd,
            ..budget(10, 0.10)
        };
        let tally = judge_all(judge.as_ref(), &g, &views(20), &context(), plan)
            .await
            .unwrap();
        // $0.06 of probes and one batch at $0.04. The next would cross the line.
        assert_eq!(tally.rejected, 10);
        assert!(tally.halted);
        assert!((tally.cost_usd.unwrap() - 0.10).abs() < 1e-9);
    }

    /// The verdict the queue would read for `cid`.
    fn latest(g: &Graph, cid: &str) -> String {
        g.connection()
            .query_row(
                "SELECT model FROM verdicts WHERE cid = ? AND verdict <> 'error'
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [cid],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn going_back_to_a_judge_makes_its_cached_verdict_current_for_free() {
        let (_dir, g) = graph();
        let (pairs, ctx) = (views(2), context());
        let mut first = Fake::costing(0.0);
        first.model = "first";
        judge_all(&first, &g, &pairs, &ctx, plan(10)).await.unwrap();
        let mut second = Fake::costing(0.0);
        second.model = "second";
        judge_all(&second, &g, &pairs, &ctx, plan(10))
            .await
            .unwrap();
        assert_eq!(latest(&g, "cid-00"), "second");

        let mut back = Fake::costing(0.0);
        back.model = "first";
        let tally = judge_all(&back, &g, &pairs, &ctx, plan(10)).await.unwrap();
        assert!(back.sizes().is_empty());
        assert_eq!(tally.cached, 2);
        assert_eq!(latest(&g, "cid-00"), "first");
        assert_eq!(latest(&g, "cid-01"), "first");
    }

    #[tokio::test]
    async fn an_unchanged_rerun_writes_nothing() {
        let (_dir, g) = graph();
        let (pairs, ctx) = (views(2), context());
        judge_all(&Fake::costing(0.0), &g, &pairs, &ctx, plan(10))
            .await
            .unwrap();
        let rowids = |g: &Graph| -> Vec<i64> {
            let connection = g.connection();
            let mut statement = connection
                .prepare("SELECT rowid FROM verdicts ORDER BY cid")
                .unwrap();
            let found = statement.query_map([], |row| row.get(0)).unwrap();
            found.map(Result::unwrap).collect()
        };
        let before = rowids(&g);
        judge_all(&Fake::costing(0.0), &g, &pairs, &ctx, plan(10))
            .await
            .unwrap();
        assert_eq!(rowids(&g), before);
    }

    // AC3.
    #[tokio::test]
    async fn a_failed_batch_is_retried_in_halves_then_marked_error_and_others_survive() {
        let (_dir, g) = graph();
        // The second batch of 4 fails, and so do both of its halves.
        let fake = Fake::new(|call, batch| match call {
            1..=3 => Err(failure()),
            _ => Ok(rejected(batch, None)),
        });
        let tally = judge_all(&fake, &g, &views(12), &context(), plan(4))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [4, 4, 2, 2, 4]);
        assert_eq!(tally.rejected, 8);
        assert_eq!(tally.errored, 4);
        assert_eq!(tally.unjudged, 0);
        assert_eq!(rows(&g, "reject").len(), 8);
        assert_eq!(
            rows(&g, JUDGE_ERROR),
            ["cid-04", "cid-05", "cid-06", "cid-07"]
        );
    }

    #[tokio::test]
    async fn a_half_that_works_keeps_its_verdicts() {
        let (_dir, g) = graph();
        let fake = Fake::new(|call, batch| match call {
            0 | 1 => Err(failure()),
            _ => Ok(rejected(batch, None)),
        });
        let tally = judge_all(&fake, &g, &views(5), &context(), plan(5))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [5, 3, 2]);
        assert_eq!(tally.errored, 3);
        assert_eq!(tally.rejected, 2);
    }

    #[tokio::test]
    async fn a_single_pair_gets_one_retry() {
        let (_dir, g) = graph();
        let fake = Fake::new(|_, _| Err(failure()));
        let tally = judge_all(&fake, &g, &views(1), &context(), plan(10))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [1, 1]);
        assert_eq!(tally.errored, 1);
    }

    #[tokio::test]
    async fn an_error_is_no_verdict_so_the_next_run_asks_again_and_replaces_it() {
        let (_dir, g) = graph();
        let ctx = context();
        let failing = Fake::new(|_, _| Err(failure()));
        judge_all(&failing, &g, &views(2), &ctx, plan(10))
            .await
            .unwrap();
        assert_eq!(rows(&g, JUDGE_ERROR).len(), 2);

        let working = Fake::costing(0.0);
        let tally = judge_all(&working, &g, &views(2), &ctx, plan(10))
            .await
            .unwrap();
        assert_eq!(working.sizes(), [2]);
        assert_eq!(tally.cached, 0);
        assert!(rows(&g, JUDGE_ERROR).is_empty());
        assert_eq!(rows(&g, "reject").len(), 2);
    }

    #[tokio::test]
    async fn a_lost_login_stops_the_run_with_no_retry() {
        let (_dir, g) = graph();
        let fake = Fake::new(|call, batch| match call {
            0 => Ok(rejected(batch, None)),
            _ => Err(JudgeError::NotLoggedIn(JudgeId::ClaudeCli)),
        });
        let tally = judge_all(&fake, &g, &views(30), &context(), plan(10))
            .await
            .unwrap();
        assert_eq!(fake.sizes(), [10, 10]);
        assert_eq!(tally.rejected, 10);
        assert_eq!(tally.unjudged, 20);
        assert!(!tally.halted);
        assert!(rows(&g, JUDGE_ERROR).is_empty());
    }

    #[tokio::test]
    async fn an_error_row_carries_its_share_of_a_billed_call_and_never_a_relation() {
        let (_dir, g) = graph();
        let fake = Fake::new(|_, _| Err(failure().billed(Some(0.02))));
        judge_all(&fake, &g, &views(2), &context(), plan(10))
            .await
            .unwrap();
        let (rel, cost, reason): (Option<String>, f64, String) = g
            .connection()
            .query_row(
                "SELECT rel, cost_usd, rationale FROM verdicts WHERE cid = 'cid-00'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(rel, None);
        assert!((cost - 0.02).abs() < 1e-9, "{cost}");
        assert!(reason.contains("no structured_output"), "{reason}");
    }
}
