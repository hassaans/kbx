use std::env;
use std::path::Path;

use kbx_core::{Graph, JudgeBackend, JudgeConfig};
use tracing::warn;

use crate::verdict::store;
use crate::{
    Availability, CandidateView, ClaudeCli, CodexCli, Decision, Judge, JudgeContext, JudgeError,
    JudgeId, ProbeCache, claude, codex,
};

/// What [`judge_all`] came to.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tally {
    /// Pairs accepted.
    pub accepted: usize,
    /// Pairs rejected.
    pub rejected: usize,
    /// Pairs left for a person.
    pub unsure: usize,
    /// Pairs in batches that failed or never ran.
    pub unjudged: usize,
    /// What the batches cost, when the backend says.
    pub cost_usd: Option<f64>,
    /// Input tokens served from the provider's cache.
    pub cached_tokens: i64,
}

/// The backend called `id`, set up from `kbx.toml` and the environment. Probes
/// are cached in the vault at `vault`, if there is one.
pub fn backend(id: JudgeId, config: &JudgeConfig, vault: Option<&Path>) -> Box<dyn Judge> {
    let cache = ProbeCache::new(vault);
    match id {
        JudgeId::ClaudeCli => {
            let model = model("KBX_CLAUDE_MODEL", &config.claude, claude::DEFAULT_MODEL);
            Box::new(ClaudeCli::new(model, cache))
        }
        JudgeId::CodexCli => {
            let model = model("KBX_CODEX_MODEL", &config.codex, codex::DEFAULT_MODEL);
            Box::new(CodexCli::new(model, cache))
        }
    }
}

/// Every backend in `judge.order` that this build has.
pub fn backends(config: &JudgeConfig, vault: Option<&Path>) -> Vec<Box<dyn Judge>> {
    let ids = config.order.iter().filter_map(|name| JudgeId::parse(name));
    ids.map(|id| backend(id, config, vault)).collect()
}

fn model(variable: &str, config: &JudgeBackend, default: &str) -> String {
    let set = env::var(variable).ok().filter(|model| !model.is_empty());
    set.or_else(|| config.model.clone())
        .unwrap_or_else(|| default.to_string())
}

/// Goes down `backends`, stopping at the first that's ready. Also returns what
/// was found of the ones passed over.
pub async fn first_ready(
    backends: Vec<Box<dyn Judge>>,
) -> (Option<Box<dyn Judge>>, Vec<(JudgeId, Availability)>) {
    let mut passed_over = Vec::new();
    for backend in backends {
        match backend.available().await {
            Availability::Ready => return (Some(backend), passed_over),
            availability => passed_over.push((backend.id(), availability)),
        }
    }
    (None, passed_over)
}

/// Judges `views` in batches of `size`, storing each batch's verdicts as they
/// come. A batch that fails is counted in [`Tally::unjudged`] and the next one
/// runs, unless the login went, which would fail them all.
pub async fn judge_all(
    judge: &dyn Judge,
    g: &Graph,
    views: &[CandidateView],
    ctx: &JudgeContext,
    size: usize,
) -> Result<Tally, JudgeError> {
    let mut tally = Tally::default();
    let mut batches = views.chunks(size);
    for batch in batches.by_ref() {
        let verdicts = match judge.judge(batch, ctx).await {
            Ok(verdicts) => verdicts,
            Err(error @ JudgeError::NotLoggedIn(_)) => {
                warn!("{error}; stopping");
                tally.unjudged += batch.len();
                break;
            }
            Err(error) => {
                warn!("{error}");
                tally.unjudged += batch.len();
                continue;
            }
        };
        store(g, judge.id(), judge.model(), &ctx.prompt_hash, &verdicts)?;
        for verdict in &verdicts {
            match verdict.decision {
                Decision::Accept => tally.accepted += 1,
                Decision::Reject => tally.rejected += 1,
                Decision::Unsure => tally.unsure += 1,
            }
            tally.cached_tokens += verdict.cached_tokens.unwrap_or(0);
            if let Some(cost) = verdict.cost_usd {
                *tally.cost_usd.get_or_insert(0.0) += cost;
            }
        }
    }
    tally.unjudged += batches.map(<[_]>::len).sum::<usize>();
    Ok(tally)
}
