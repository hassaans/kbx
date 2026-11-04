use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::http::{
    Message, base_url, get, input, messages, post, read_reply, schema, silent, warn_if_remote,
};
use crate::{Availability, CandidateView, Judge, JudgeContext, JudgeError, JudgeId, Verdict};

const PROGRAM: &str = "ollama";
const ID: JudgeId = JudgeId::Ollama;
const DEFAULT_HOST: &str = "http://127.0.0.1:11434";
const DEFAULT_PORT: u16 = 11434;

/// The model when neither `KBX_OLLAMA_MODEL` nor `kbx.toml` names one.
pub(crate) const DEFAULT_MODEL: &str = "qwen3:14b";

/// What [`DEFAULT_MODEL`] gives way to on a machine short of memory.
const SMALL_MODEL: &str = "qwen3:8b";

/// The free memory [`DEFAULT_MODEL`] needs, in bytes (§12).
const DEFAULT_MODEL_NEEDS: u64 = 12 << 30;

/// The context a batch normally fits in (§6.2). Ollama's own default is half
/// this, and it drops what doesn't fit from the front, which is the system
/// prompt.
const MIN_CONTEXT: u32 = 8192;

/// A batch of 20 full excerpts needs about 28k tokens, and qwen3 stops at 32k.
const MAX_CONTEXT: u32 = 32768;

/// Room for one verdict in the reply.
const TOKENS_PER_VERDICT: usize = 100;

/// Fewer bytes than the four to a token used elsewhere, because running short
/// costs the system prompt and notes aren't all English.
const BYTES_PER_TOKEN: usize = 3;

/// A local Ollama server.
#[derive(Debug, Clone)]
pub struct Ollama {
    model: String,
    host: String,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    format: &'a RawValue,
    think: bool,
    stream: bool,
    keep_alive: &'static str,
    options: Options,
}

#[derive(Serialize)]
struct Options {
    temperature: u8,
    num_ctx: u32,
}

#[derive(Deserialize)]
struct Response {
    message: Reply,
    done_reason: Option<String>,
}

#[derive(Deserialize)]
struct Reply {
    content: String,
}

#[derive(Deserialize)]
struct Tags {
    models: Vec<Tag>,
}

#[derive(Deserialize)]
struct Tag {
    name: String,
}

impl Ollama {
    /// A backend that asks for `model` at `host`, or at the usual address.
    pub fn new(model: String, host: Option<&str>) -> Ollama {
        Ollama {
            model,
            host: base_url(host.unwrap_or(DEFAULT_HOST), DEFAULT_PORT),
        }
    }

    fn has_model(&self, tags: &Tags) -> bool {
        let latest = format!("{}:latest", self.model);
        let same = |tag: &Tag| tag.name == self.model || tag.name == latest;
        tags.models.iter().any(same)
    }
}

/// The model to ask for, given the one configured and the bytes free. Only the
/// default gives way: a model someone chose is theirs to run out of memory
/// with. If nothing says what's free, nothing changes.
pub(crate) fn fit(model: String, free: Option<u64>) -> String {
    let short = free.is_some_and(|free| free < DEFAULT_MODEL_NEEDS);
    if model == DEFAULT_MODEL && short {
        SMALL_MODEL.to_string()
    } else {
        model
    }
}

/// A context big enough for the request and its reply. A power of two, because
/// Ollama reloads the model whenever this changes.
fn context_size(system: &str, input: &str, pairs: usize) -> u32 {
    let tokens = (system.len() + input.len()) / BYTES_PER_TOKEN + pairs * TOKENS_PER_VERDICT;
    let tokens = u32::try_from(tokens).unwrap_or(MAX_CONTEXT);
    tokens.next_power_of_two().clamp(MIN_CONTEXT, MAX_CONTEXT)
}

#[async_trait]
impl Judge for Ollama {
    fn id(&self) -> JudgeId {
        ID
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn available(&self) -> Availability {
        let Some(text) = get(&format!("{}/api/tags", self.host)).await else {
            return silent(PROGRAM, &self.host);
        };
        match serde_json::from_str(&text) {
            Ok(tags) if self.has_model(&tags) => Availability::Ready,
            Ok(_) => Availability::NotInstalled,
            Err(_) => Availability::Unreachable,
        }
    }

    async fn judge(
        &self,
        batch: &[CandidateView],
        ctx: &JudgeContext,
    ) -> Result<Vec<Verdict>, JudgeError> {
        warn_if_remote(ID, &self.host);
        let input = input(batch);
        let format = schema(ID, ctx)?;
        let request = Request {
            model: &self.model,
            messages: messages(ctx, &input),
            format: &format,
            think: false,
            stream: false,
            keep_alive: "10m",
            options: Options {
                temperature: 0,
                num_ctx: context_size(&ctx.system, &input, batch.len()),
            },
        };
        let text = post(ID, &format!("{}/api/chat", self.host), &request).await?;
        let response: Response = serde_json::from_str(&text)
            .map_err(|_| JudgeError::Malformed(ID, "not a chat response".to_string()))?;
        if response.done_reason.as_deref() == Some("length") {
            return Err(JudgeError::Malformed(
                ID,
                "the reply was cut off".to_string(),
            ));
        }
        read_reply(ID, &response.message.content, batch, ctx)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::http::fake::{CLOSED, FakeServer, ctx, verdicts, view};

    const GIB: u64 = 1 << 30;

    fn chat(content: &str) -> String {
        json!({"model": "qwen3:8b", "message": {"role": "assistant", "content": content},
               "done": true, "done_reason": "stop"})
        .to_string()
    }

    fn at(server: &FakeServer, model: &str) -> Ollama {
        Ollama::new(model.to_string(), Some(&server.url))
    }

    fn tags(names: &[&str]) -> String {
        let rows: Vec<Value> = names.iter().map(|name| json!({"name": name})).collect();
        json!({ "models": rows }).to_string()
    }

    #[test]
    fn the_default_model_gives_way_under_12_gb_free() {
        let fit = |model: &str, free| fit(model.to_string(), free);
        assert_eq!(fit("qwen3:14b", Some(11 * GIB)), "qwen3:8b");
        assert_eq!(fit("qwen3:14b", Some(12 * GIB - 1)), "qwen3:8b");
        assert_eq!(fit("qwen3:14b", Some(12 * GIB)), "qwen3:14b");
        assert_eq!(fit("qwen3:14b", Some(64 * GIB)), "qwen3:14b");
    }

    #[test]
    fn a_chosen_model_stays_however_little_is_free() {
        assert_eq!(fit("gemma3:27b".to_string(), Some(GIB)), "gemma3:27b");
    }

    #[test]
    fn unknown_memory_changes_nothing() {
        assert_eq!(fit("qwen3:14b".to_string(), None), "qwen3:14b");
    }

    #[test]
    fn the_context_grows_with_the_batch_and_stops_at_the_models_limit() {
        assert_eq!(context_size("system", "[]", 1), MIN_CONTEXT);
        let batch = "x".repeat(60_000);
        assert_eq!(context_size("system", &batch, 20), MAX_CONTEXT);
        let huge = "x".repeat(1_000_000);
        assert_eq!(context_size("system", &huge, 20), MAX_CONTEXT);
    }

    #[tokio::test]
    async fn it_is_ready_when_the_server_lists_the_model() {
        let server = FakeServer::start(|_| (200, tags(&["qwen3:8b", "qwen3:latest"])));
        assert_eq!(
            at(&server, "qwen3:8b").available().await,
            Availability::Ready
        );
        assert_eq!(at(&server, "qwen3").available().await, Availability::Ready);
        let seen = server.seen();
        assert_eq!(
            (seen[0].method.as_str(), seen[0].url.as_str()),
            ("GET", "/api/tags")
        );
    }

    #[tokio::test]
    async fn a_model_not_pulled_is_not_installed() {
        let server = FakeServer::start(|_| (200, tags(&["qwen3:8b"])));
        let availability = at(&server, "qwen3:14b").available().await;
        assert_eq!(availability, Availability::NotInstalled);
    }

    #[tokio::test]
    async fn a_server_that_makes_no_sense_is_unreachable() {
        let server = FakeServer::start(|_| (200, "<html>".to_string()));
        let availability = at(&server, "qwen3:8b").available().await;
        assert_eq!(availability, Availability::Unreachable);
    }

    #[tokio::test]
    async fn a_closed_port_is_never_ready() {
        let ollama = Ollama::new("qwen3:8b".to_string(), Some(CLOSED));
        assert_ne!(ollama.available().await, Availability::Ready);
    }

    #[tokio::test]
    async fn the_request_is_the_one_in_the_spec() {
        let server = FakeServer::start(|_| (200, chat(&verdicts(&["a"]))));
        let ctx = ctx();
        let found = at(&server, "qwen3:8b")
            .judge(&[view("a")], &ctx)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cost_usd, Some(0.0));

        let seen = server.seen();
        assert_eq!(
            (seen[0].method.as_str(), seen[0].url.as_str()),
            ("POST", "/api/chat")
        );
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["model"], "qwen3:8b");
        assert_eq!(body["think"], false);
        assert_eq!(body["stream"], false);
        assert_eq!(body["keep_alive"], "10m");
        assert_eq!(body["options"]["temperature"], 0);
        assert_eq!(body["options"]["num_ctx"], MIN_CONTEXT);
        let schema: Value = serde_json::from_str(&ctx.schema).unwrap();
        assert_eq!(body["format"], schema);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], ctx.system.as_str());
        assert_eq!(body["messages"][1]["role"], "user");
        let batch: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(batch[0]["cid"], "a");
    }

    #[tokio::test]
    async fn the_schema_reaches_the_model_with_the_rationale_first() {
        let server = FakeServer::start(|_| (200, chat(&verdicts(&["a"]))));
        at(&server, "qwen3:8b")
            .judge(&[view("a")], &ctx())
            .await
            .unwrap();
        let body = &server.seen()[0].body;
        let format = body.find("\"format\":").unwrap();
        let (rationale, confidence) = (
            format + body[format..].find("\"rationale\":").unwrap(),
            format + body[format..].find("\"confidence\":").unwrap(),
        );
        assert!(rationale < confidence, "{body}");
    }

    #[tokio::test]
    async fn a_reply_that_breaks_the_contract_is_malformed_and_not_quoted() {
        let replies = [
            chat("Sure! SECRET"),
            chat(r#"{"verdicts":[{"cid":"SECRET"}]}"#),
            chat(&verdicts(&["SECRET"])),
            "SECRET".to_string(),
        ];
        for reply in replies {
            let server = FakeServer::start(move |_| (200, reply.clone()));
            let error = at(&server, "m")
                .judge(&[view("a")], &ctx())
                .await
                .unwrap_err();
            assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
            assert!(!error.to_string().contains("SECRET"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_reply_cut_off_at_the_context_limit_is_malformed() {
        let reply = json!({"message": {"content": verdicts(&["a"])}, "done_reason": "length"});
        let server = FakeServer::start(move |_| (200, reply.to_string()));
        let error = at(&server, "m")
            .judge(&[view("a")], &ctx())
            .await
            .unwrap_err();
        assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
    }

    #[tokio::test]
    async fn a_server_error_gives_its_status_and_not_its_message() {
        let server = FakeServer::start(|_| (500, r#"{"error":"PRIVATE-EXCERPT"}"#.to_string()));
        let error = at(&server, "m")
            .judge(&[view("a")], &ctx())
            .await
            .unwrap_err();
        assert!(matches!(error, JudgeError::Failed(..)), "{error}");
        assert!(error.to_string().contains("500"), "{error}");
        assert!(!error.to_string().contains("PRIVATE-EXCERPT"), "{error}");
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let server = FakeServer::start(|_| (307, String::new()));
        let error = at(&server, "m")
            .judge(&[view("a")], &ctx())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("307"), "{error}");
        assert_eq!(server.seen().len(), 1);
    }
}
