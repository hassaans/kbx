//! LLM judge backends for kbx.
//!
//! A [`Judge`] answers a batch of [`CandidateView`]s with [`Verdict`]s.
//! [`ClaudeCli`] and [`CodexCli`] shell out to the logged-in cloud CLIs, and
//! [`Ollama`] and [`LlamaServer`] talk to a local model over HTTP, all with the
//! system prompt and schema from [`JudgeContext`]. [`views`] builds the batch
//! items, [`first_ready`] picks a backend, [`route`] shares the pairs out
//! between it and the backends `[judge.overrides]` pins folders to, and
//! [`judge_all`] runs the batches and stores what comes back in the `verdicts`
//! table. This crate never prints.
//!
//! What's in a note goes to a backend, on stdin or in a request body, and
//! nowhere else: not to a file, not to a log, not into an error, and not
//! through a proxy.

mod batch;
mod cache;
mod claude;
mod codex;
mod http;
mod llama_server;
mod ollama;
mod process;
mod prompt;
mod route;
mod run;
mod verdict;

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use batch::{CandidateView, NoteView, Views, views, why_proposed};
pub use cache::{CACHE_FILE, ProbeCache};
pub use claude::ClaudeCli;
pub use codex::CodexCli;
pub use llama_server::LlamaServer;
pub use ollama::Ollama;
pub use prompt::{JudgeContext, SCHEMA, SYSTEM_PROMPT, prompt_hash};
pub use route::{Assignment, Routes, route};
pub use run::{Tally, backend, backends, first_ready, judge_all, pinned_backends};
pub use verdict::{Decision, Verdict};

/// A judge of link candidates.
#[async_trait]
pub trait Judge: Send + Sync {
    /// Which backend this is.
    fn id(&self) -> JudgeId;
    /// The model as configured, which is what `verdicts.model` holds. It is
    /// known before a call, so a verdict can be looked up without making one.
    fn model(&self) -> &str;
    /// Whether [`Judge::judge`] can be expected to work right now.
    async fn available(&self) -> Availability;
    /// Judges every pair in `batch`, or fails as a whole.
    async fn judge(
        &self,
        batch: &[CandidateView],
        ctx: &JudgeContext,
    ) -> Result<Vec<Verdict>, JudgeError>;
}

/// A judge backend's name, as used in `--judge`, `kbx.toml` and `verdicts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeId {
    /// The Claude Code CLI.
    ClaudeCli,
    /// The Codex CLI.
    CodexCli,
    /// A local Ollama server.
    Ollama,
    /// A local llama.cpp server.
    LlamaServer,
}

impl JudgeId {
    /// Every backend, in the §6.1 order.
    pub const ALL: [JudgeId; 4] = [
        JudgeId::ClaudeCli,
        JudgeId::CodexCli,
        JudgeId::Ollama,
        JudgeId::LlamaServer,
    ];

    /// The name.
    pub fn as_str(self) -> &'static str {
        match self {
            JudgeId::ClaudeCli => "claude-cli",
            JudgeId::CodexCli => "codex-cli",
            JudgeId::Ollama => "ollama",
            JudgeId::LlamaServer => "llama-server",
        }
    }

    /// The backend called `name`, if this build has it.
    pub fn parse(name: &str) -> Option<JudgeId> {
        JudgeId::ALL.into_iter().find(|id| id.as_str() == name)
    }
}

impl fmt::Display for JudgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// Whether a backend can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// It answered.
    Ready,
    /// It's installed, but has no working login.
    NotLoggedIn,
    /// It isn't on `PATH`, or the model it was asked for isn't pulled.
    NotInstalled,
    /// It didn't answer in time.
    Unreachable,
}

impl fmt::Display for Availability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Availability::Ready => "Ready",
            Availability::NotLoggedIn => "NotLoggedIn",
            Availability::NotInstalled => "NotInstalled",
            Availability::Unreachable => "Unreachable",
        })
    }
}

/// Why a batch has no verdicts.
#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    /// The login stopped working, so other batches would fail too.
    #[error("{0} is not logged in")]
    NotLoggedIn(JudgeId),
    /// The backend reported a failure, or couldn't be run.
    #[error("{0} failed: {1}")]
    Failed(JudgeId, String),
    /// The backend didn't finish in time.
    #[error("{0} timed out")]
    TimedOut(JudgeId),
    /// The backend answered, but not with what was asked for. The reason never
    /// quotes the response.
    #[error("{0} returned a malformed response: {1}")]
    Malformed(JudgeId, String),
    /// The database failed.
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
}
