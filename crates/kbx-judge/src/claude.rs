use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kbx_core::{Rates, find_on_path};
use serde::Deserialize;
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

const PROGRAM: &str = "claude";
const ID: JudgeId = JudgeId::ClaudeCli;

/// The model when neither `KBX_CLAUDE_MODEL` nor `kbx.toml` names one.
pub(crate) const DEFAULT_MODEL: &str = "haiku";

/// Claude Haiku's prices. Only a budget's first batch goes by them: after that
/// it has `total_cost_usd`, which is also all that's ever summed.
pub(crate) const DEFAULT_RATES: Rates = Rates {
    input: 0.10,
    cached_input: 0.01,
    output: 0.50,
};

/// About what a probe reads: Claude Code's own prompt, at the least.
const PROBE_INPUT_TOKENS: i64 = 8000;
const PROBE_OUTPUT_TOKENS: i64 = 50;

const PROBE_PROMPT: &str = r#"reply with {"ok":true}"#;
const PROBE_SCHEMA: &str = r#"{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}"#;

/// The Claude Code CLI, `claude -p`.
#[derive(Debug, Clone)]
pub struct ClaudeCli {
    model: String,
    rates: Rates,
    cache: ProbeCache,
    probes_usd: Arc<Mutex<f64>>,
}

/// The fields of `--output-format json` that matter here.
#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    is_error: bool,
    /// The reply, or on an error what went wrong.
    result: Option<String>,
    api_error_status: Option<i64>,
    structured_output: Option<Value>,
    total_cost_usd: Option<f64>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    cache_read_input_tokens: Option<i64>,
}

impl ClaudeCli {
    /// A backend that asks for `model`, quoted at `rates`, with its probes
    /// kept in `cache`.
    pub fn new(model: String, rates: Rates, cache: ProbeCache) -> ClaudeCli {
        ClaudeCli {
            model,
            rates,
            cache,
            probes_usd: Arc::default(),
        }
    }

    /// The §6.2 invocation, with no prompt. `--bare` must never be here: it
    /// ignores the OAuth login. `--tools ""` and `--strict-mcp-config` leave a
    /// note's text nothing to act through. `--no-session-persistence` keeps
    /// the batch out of the transcripts under `~/.claude`.
    fn args(&self, schema: &str) -> Vec<String> {
        [
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            schema,
            "--model",
            &self.model,
            "--max-turns",
            "1",
            "--tools",
            "",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--setting-sources",
            "user",
        ]
        .map(String::from)
        .to_vec()
    }

    /// Only ever rules a login out. An unexpected answer, as from a version
    /// without the subcommand, leaves it to the probe.
    async fn logged_out(&self, program: &Path, workspace: &Workspace) -> bool {
        let cwd = workspace.cwd();
        let status = run(program, ["auth", "status"], &cwd, b"", PROBE_TIMEOUT);
        let Ok(finished) = status.await else {
            return false;
        };
        let status: Option<Value> = serde_json::from_str(&finished.stdout).ok();
        status.is_some_and(|s| s.get("loggedIn") == Some(&Value::Bool(false)))
    }

    /// Adds a probe's cost to what's been spent, and reads its result.
    fn charge_probe(&self, finished: &Finished) -> Result<Response, JudgeError> {
        let cost = reported_cost(finished).unwrap_or(0.0);
        *self.probes_usd.lock().expect("never poisoned") += cost;
        read_response(finished)
    }

    async fn probe(&self, program: &Path, workspace: &Workspace) -> Availability {
        let mut args = self.args(PROBE_SCHEMA);
        args.insert(1, PROBE_PROMPT.to_string());
        let cwd = workspace.cwd();
        match run(program, args, &cwd, b"", PROBE_TIMEOUT).await {
            Ok(finished) => match self.charge_probe(&finished) {
                Ok(_) => Availability::Ready,
                Err(error) => {
                    warn!("the claude-cli probe failed: {error}");
                    Availability::NotLoggedIn
                }
            },
            Err(RunError::TimedOut) => Availability::Unreachable,
            Err(RunError::Spawn(_)) => Availability::NotInstalled,
        }
    }
}

#[async_trait]
impl Judge for ClaudeCli {
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
        let program = program()?;
        let workspace = Workspace::new(&ctx.system, &ctx.schema)
            .map_err(|error| JudgeError::Failed(ID, error.to_string()))?;
        let mut args = self.args(&ctx.schema);
        args.push("--append-system-prompt-file".to_string());
        args.push(workspace.system_file().display().to_string());
        let input = serde_json::to_vec(batch).expect("a batch serializes");

        let finished = run(&program, args, &workspace.cwd(), &input, BATCH_TIMEOUT)
            .await
            .map_err(|error| match error {
                RunError::TimedOut => JudgeError::TimedOut(ID),
                RunError::Spawn(error) => JudgeError::Failed(ID, error.to_string()),
            })?;
        let response = read_response(&finished).map_err(|error| {
            if matches!(error, JudgeError::NotLoggedIn(_)) {
                self.cache.forget(ID);
            }
            error.billed(reported_cost(&finished))
        })?;
        let spend = Spend {
            cost_usd: response.total_cost_usd,
            cached_tokens: response.usage.and_then(|u| u.cache_read_input_tokens),
        };
        let answer = response.structured_output.unwrap_or(Value::Null);
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

/// What a failed call says it cost, if it got as far as saying.
fn reported_cost(finished: &Finished) -> Option<f64> {
    let response: Response = serde_json::from_str(&finished.stdout).ok()?;
    response.total_cost_usd
}

fn program() -> Result<PathBuf, JudgeError> {
    find_on_path(PROGRAM).ok_or_else(|| JudgeError::Failed(ID, format!("{PROGRAM} is not on PATH")))
}

/// Reads the result, checking `is_error` as well as the exit code: a logged-out
/// `claude -p` exits 0.
fn read_response(finished: &Finished) -> Result<Response, JudgeError> {
    let response: Option<Response> = serde_json::from_str(&finished.stdout).ok();
    let failed = !finished.success || response.as_ref().is_none_or(|r| r.is_error);
    if !failed {
        let response = response.expect("checked above");
        if response.structured_output.is_none() {
            let reason = "no structured_output".to_string();
            return Err(JudgeError::Malformed(ID, reason));
        }
        return Ok(response);
    }
    if finished.success && response.is_none() {
        let reason = "stdout isn't a JSON result".to_string();
        return Err(JudgeError::Malformed(ID, reason));
    }
    let reported = response.as_ref().and_then(|r| r.result.as_deref());
    let message = reported.unwrap_or(&finished.stderr);
    let status = response.as_ref().and_then(|r| r.api_error_status);
    if status == Some(401) || means_logged_out(message) {
        return Err(JudgeError::NotLoggedIn(ID));
    }
    Err(JudgeError::Failed(ID, brief(message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(success: bool, stdout: &str, stderr: &str) -> Finished {
        Finished {
            success,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn the_invocation_never_goes_bare() {
        let backend = ClaudeCli::new("haiku".to_string(), DEFAULT_RATES, ProbeCache::new(None));
        let args = backend.args("{}");
        assert!(!args.iter().any(|arg| arg == "--bare"));
        let tools = args.iter().position(|arg| arg == "--tools").unwrap();
        assert_eq!(args[tools + 1], "");
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
    }

    #[test]
    fn a_logged_out_cli_that_exits_zero_is_still_logged_out() {
        // What `claude -p` 2.1.307 prints with no login.
        let stdout = r#"{"type":"result","subtype":"success","is_error":true,
            "api_error_status":null,"result":"Not logged in · Please run /login"}"#;
        let error = read_response(&finished(true, stdout, "")).err().unwrap();
        assert!(matches!(error, JudgeError::NotLoggedIn(_)), "{error}");
    }

    #[test]
    fn a_rejected_token_is_a_lost_login() {
        let stdout = r#"{"is_error":true,"api_error_status":401,"result":"Failed"}"#;
        let error = read_response(&finished(false, stdout, "")).err().unwrap();
        assert!(matches!(error, JudgeError::NotLoggedIn(_)), "{error}");
        let error = read_response(&finished(false, "", "authentication_failed"))
            .err()
            .unwrap();
        assert!(matches!(error, JudgeError::NotLoggedIn(_)), "{error}");
    }

    #[test]
    fn other_failures_keep_their_message() {
        let stdout = r#"{"is_error":true,"api_error_status":429,"result":"Rate limited"}"#;
        let error = read_response(&finished(true, stdout, "")).err().unwrap();
        assert!(matches!(error, JudgeError::Failed(..)), "{error}");
        assert!(error.to_string().contains("Rate limited"));
    }

    #[test]
    fn a_failed_call_keeps_what_it_cost() {
        let stdout = r#"{"is_error":true,"result":"Overloaded","total_cost_usd":0.02}"#;
        assert_eq!(reported_cost(&finished(true, stdout, "")), Some(0.02));
        assert_eq!(reported_cost(&finished(false, "", "boom")), None);
    }

    #[test]
    fn a_result_without_structured_output_is_malformed() {
        let stdout = r#"{"is_error":false,"result":"Sure! Here are your verdicts"}"#;
        let error = read_response(&finished(true, stdout, "")).err().unwrap();
        assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
        assert!(!error.to_string().contains("Sure!"));

        let error = read_response(&finished(true, "Sure!", "")).err().unwrap();
        assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
    }
}
