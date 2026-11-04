use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kbx_core::{Rates, find_on_path};
use serde_json::Value;
use tracing::warn;

use crate::ledger::quote;
use crate::process::{
    BATCH_TIMEOUT, Finished, PROBE_TIMEOUT, RunError, Workspace, brief, means_logged_out, run,
};
use crate::verdict::{Spend, read_answer};
use crate::{
    Availability, CandidateView, Judge, JudgeContext, JudgeError, JudgeId, ProbeCache, Verdict,
};

const PROGRAM: &str = "codex";
const ID: JudgeId = JudgeId::CodexCli;

/// The model when neither `KBX_CODEX_MODEL` nor `kbx.toml` names one. The
/// spec's `gpt-6-luna-fast` is refused under a ChatGPT login.
pub(crate) const DEFAULT_MODEL: &str = "gpt-6-luna";

/// `gpt-6-luna`'s prices. Codex reports tokens, not money, so a budget's spend
/// is these times the tokens. `[judge.codex] rates` replaces them.
pub(crate) const DEFAULT_RATES: Rates = Rates {
    input: 0.10,
    cached_input: 0.01,
    output: 0.50,
};

/// About what a probe reads: `codex exec` loads the user's config and
/// `AGENTS.md`.
const PROBE_INPUT_TOKENS: i64 = 20_000;
const PROBE_OUTPUT_TOKENS: i64 = 100;

const PROBE_PROMPT: &[u8] = b"reply with ok";
const PROBE_SCHEMA: &str = r#"{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}"#;

/// The Codex CLI, `codex exec`.
#[derive(Debug, Clone)]
pub struct CodexCli {
    model: String,
    rates: Rates,
    cache: ProbeCache,
    probes_usd: Arc<Mutex<f64>>,
}

/// What a `--json` event stream came to.
#[derive(Debug, Default, PartialEq)]
struct Turn {
    /// The last agent message.
    answer: Option<String>,
    /// `input_tokens` from `turn.completed`, cached ones included.
    input_tokens: Option<i64>,
    /// `cached_input_tokens` from `turn.completed`.
    cached_tokens: Option<i64>,
    /// `output_tokens` from `turn.completed`.
    output_tokens: Option<i64>,
    completed: bool,
    /// Why the turn failed. The `error` events before it may be retries.
    failure: Option<String>,
}

impl CodexCli {
    /// A backend that asks for `model`, priced at `rates`, with its probes
    /// kept in `cache`.
    pub fn new(model: String, rates: Rates, cache: ProbeCache) -> CodexCli {
        CodexCli {
            model,
            rates,
            cache,
            probes_usd: Arc::default(),
        }
    }

    /// The §6.2 invocation. The prompt comes on stdin.
    fn args(&self, workspace: &Workspace) -> Vec<String> {
        let schema = workspace.schema_file().display().to_string();
        let cwd = workspace.cwd().display().to_string();
        [
            "exec",
            "--json",
            "--output-schema",
            &schema,
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "-C",
            &cwd,
            "--model",
            &self.model,
            "-",
        ]
        .map(String::from)
        .to_vec()
    }

    /// Only ever rules a login out. The credentials may be in `auth.json` or a
    /// keyring, which `login status` knows and a file check wouldn't.
    async fn logged_out(&self, program: &Path, workspace: &Workspace) -> bool {
        let cwd = workspace.cwd();
        let status = run(program, ["login", "status"], &cwd, b"", PROBE_TIMEOUT);
        status.await.is_ok_and(|finished| {
            let said = format!("{}{}", finished.stdout, finished.stderr);
            !finished.success && said.to_lowercase().contains("not logged in")
        })
    }

    /// Adds a probe's cost to what's been spent, and reads its result.
    fn charge_probe(&self, finished: &Finished) -> Result<Value, JudgeError> {
        let cost = priced(&events(&finished.stdout), &self.rates).unwrap_or(0.0);
        *self.probes_usd.lock().expect("never poisoned") += cost;
        read_turn(finished).and_then(answer_of)
    }

    async fn probe(&self, program: &Path, workspace: &Workspace) -> Availability {
        let args = self.args(workspace);
        let cwd = workspace.cwd();
        match run(program, args, &cwd, PROBE_PROMPT, PROBE_TIMEOUT).await {
            Ok(finished) => match self.charge_probe(&finished) {
                Ok(_) => Availability::Ready,
                Err(error) => {
                    warn!("the codex-cli probe failed: {error}");
                    Availability::NotLoggedIn
                }
            },
            Err(RunError::TimedOut) => Availability::Unreachable,
            Err(RunError::Spawn(_)) => Availability::NotInstalled,
        }
    }
}

#[async_trait]
impl Judge for CodexCli {
    fn id(&self) -> JudgeId {
        ID
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn available(&self) -> Availability {
        let Some(program) = find_on_path(PROGRAM) else {
            return Availability::NotInstalled;
        };
        let Ok(workspace) = Workspace::new("", PROBE_SCHEMA) else {
            return Availability::Unreachable;
        };
        if self.logged_out(&program, &workspace).await {
            return Availability::NotLoggedIn;
        }
        if self.cache.ready(ID) {
            return Availability::Ready;
        }
        let availability = self.probe(&program, &workspace).await;
        if availability == Availability::Ready {
            self.cache.remember(ID);
        }
        availability
    }

    async fn judge(
        &self,
        batch: &[CandidateView],
        ctx: &JudgeContext,
    ) -> Result<Vec<Verdict>, JudgeError> {
        let program = find_on_path(PROGRAM)
            .ok_or_else(|| JudgeError::Failed(ID, format!("{PROGRAM} is not on PATH")))?;
        let workspace = Workspace::new(&ctx.system, &ctx.schema)
            .map_err(|error| JudgeError::Failed(ID, error.to_string()))?;
        let input = stdin_for(batch, ctx);
        let args = self.args(&workspace);

        let finished = run(&program, args, &workspace.cwd(), &input, BATCH_TIMEOUT)
            .await
            .map_err(|error| match error {
                RunError::TimedOut => JudgeError::TimedOut(ID),
                RunError::Spawn(error) => JudgeError::Failed(ID, error.to_string()),
            })?;
        let turn = read_turn(&finished).inspect_err(|error| {
            if matches!(error, JudgeError::NotLoggedIn(_)) {
                self.cache.forget(ID);
            }
        })?;
        let spend = Spend {
            cost_usd: priced(&turn, &self.rates),
            cached_tokens: turn.cached_tokens,
        };
        let answer = answer_of(turn).map_err(|error| error.billed(spend.cost_usd))?;
        read_answer(answer, batch, ctx, spend)
            .map_err(|reason| JudgeError::Malformed(ID, reason).billed(spend.cost_usd))
    }

    fn quote(&self, batch: &[CandidateView], ctx: &JudgeContext) -> f64 {
        quote(&self.rates, batch, ctx)
    }

    fn probe_quote(&self) -> f64 {
        match self.cache.ready(ID) {
            true => 0.0,
            false => self.rates.price(PROBE_INPUT_TOKENS, 0, PROBE_OUTPUT_TOKENS),
        }
    }

    fn probe_spend(&self) -> f64 {
        *self.probes_usd.lock().expect("never poisoned")
    }
}

/// What the turn's tokens come to. `None` if it didn't report any.
fn priced(turn: &Turn, rates: &Rates) -> Option<f64> {
    let cached = turn.cached_tokens.unwrap_or(0);
    let output = turn.output_tokens.unwrap_or(0);
    turn.input_tokens
        .map(|input| rates.price(input, cached, output))
}

/// Codex has no system prompt flag, so the stable prefix opens stdin.
fn stdin_for(batch: &[CandidateView], ctx: &JudgeContext) -> Vec<u8> {
    let mut input = ctx.system.clone().into_bytes();
    input.extend_from_slice(b"\n## Pairs\n\n");
    input.extend_from_slice(&serde_json::to_vec(batch).expect("a batch serializes"));
    input
}

/// Walks the event stream. A line that isn't JSON is skipped: warnings can land
/// on stdout.
fn events(stdout: &str) -> Turn {
    let mut turn = Turn::default();
    let mut last_error = None;
    for event in stdout.lines().filter_map(|line| {
        let event: Option<Value> = serde_json::from_str(line).ok();
        event
    }) {
        let text = |value: &Value, key: &str| value.get(key)?.as_str().map(String::from);
        match event.get("type").and_then(Value::as_str) {
            Some("item.completed") => {
                let item = &event["item"];
                if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                    turn.answer = text(item, "text");
                }
            }
            Some("turn.completed") => {
                let tokens = |key| event["usage"].get(key).and_then(Value::as_i64);
                turn.input_tokens = tokens("input_tokens");
                turn.cached_tokens = tokens("cached_input_tokens");
                turn.output_tokens = tokens("output_tokens");
                turn.completed = true;
            }
            Some("turn.failed") => {
                let reason = text(&event["error"], "message");
                turn.failure = reason.or(Some(String::new()));
            }
            Some("error") => last_error = text(&event, "message"),
            _ => {}
        }
    }
    if turn.failure.as_deref() == Some("") {
        turn.failure = last_error;
    }
    turn
}

fn read_turn(finished: &Finished) -> Result<Turn, JudgeError> {
    let turn = events(&finished.stdout);
    if finished.success && turn.failure.is_none() {
        return Ok(turn);
    }
    let message = turn.failure.as_deref().unwrap_or(&finished.stderr);
    if means_logged_out(message) {
        return Err(JudgeError::NotLoggedIn(ID));
    }
    Err(JudgeError::Failed(ID, brief(message)))
}

fn answer_of(turn: Turn) -> Result<Value, JudgeError> {
    if !turn.completed {
        return Err(JudgeError::Malformed(ID, "no turn.completed".to_string()));
    }
    let answer = turn.answer.as_deref().unwrap_or_default();
    serde_json::from_str(answer)
        .map_err(|_| JudgeError::Malformed(ID, "the last message isn't JSON".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kbx_core::{EXAMPLE_ONTOLOGY, Ontology};

    fn finished(success: bool, stdout: &str) -> Finished {
        Finished {
            success,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn a_turn_gives_its_message_and_cached_tokens() {
        // As printed by codex 0.166.0.
        let stdout = r#"warning: something on stdout
{"type":"thread.started","thread_id":"t"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"ok\":true}"}}
{"type":"turn.completed","usage":{"input_tokens":20593,"cached_input_tokens":512,"output_tokens":66}}"#;
        let turn = read_turn(&finished(true, stdout)).unwrap();
        assert_eq!(turn.cached_tokens, Some(512));
        assert_eq!(answer_of(turn).unwrap(), serde_json::json!({"ok": true}));
    }

    #[test]
    fn a_turn_is_priced_with_cached_tokens_at_their_own_rate() {
        let stdout = r#"{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":400000,"output_tokens":100000}}"#;
        let rates = Rates {
            input: 1.0,
            cached_input: 0.5,
            output: 10.0,
        };
        // 600k fresh at $1, 400k cached at $0.50, 100k out at $10.
        let cost = priced(&events(stdout), &rates).unwrap();
        assert!((cost - 1.8).abs() < 1e-9, "{cost}");
        assert_eq!(priced(&Turn::default(), &rates), None);
    }

    #[test]
    fn a_401_is_a_lost_login_and_a_refused_model_is_not() {
        let unauthorized = r#"{"type":"error","message":"Reconnecting... 2/5 (unexpected status 401 Unauthorized)"}
{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Missing bearer"}}"#;
        let error = read_turn(&finished(false, unauthorized)).err().unwrap();
        assert!(matches!(error, JudgeError::NotLoggedIn(_)), "{error}");

        let refused = r#"{"type":"turn.failed","error":{"message":"The 'x' model is not supported when using Codex with a ChatGPT account."}}"#;
        let error = read_turn(&finished(false, refused)).err().unwrap();
        assert!(matches!(error, JudgeError::Failed(..)), "{error}");
        assert!(error.to_string().contains("not supported"));
    }

    #[test]
    fn a_retry_that_recovered_is_not_a_failure() {
        let stdout = r#"{"type":"error","message":"Reconnecting... 1/5 (stream closed)"}
{"type":"item.completed","item":{"type":"agent_message","text":"{}"}}
{"type":"turn.completed","usage":{"cached_input_tokens":0}}"#;
        assert!(read_turn(&finished(true, stdout)).is_ok());
    }

    #[test]
    fn prose_or_a_cut_off_stream_is_malformed() {
        let prose = r#"{"type":"item.completed","item":{"type":"agent_message","text":"Sure!"}}
{"type":"turn.completed","usage":{}}"#;
        let error = answer_of(events(prose)).err().unwrap();
        assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
        assert!(!error.to_string().contains("Sure!"));

        let cut = r#"{"type":"item.completed","item":{"type":"agent_message","text":"{}"}}"#;
        let error = answer_of(events(cut)).err().unwrap();
        assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
    }

    #[test]
    fn stdin_opens_with_the_system_prompt_byte_for_byte() {
        let ontology = Ontology::parse(EXAMPLE_ONTOLOGY).unwrap();
        let ctx = JudgeContext::new(ontology, EXAMPLE_ONTOLOGY);
        let input = stdin_for(&[], &ctx);
        assert!(input.starts_with(crate::SYSTEM_PROMPT.as_bytes()));
        assert!(input.ends_with(b"[]"));
    }
}
