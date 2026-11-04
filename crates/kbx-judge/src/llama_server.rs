use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::http::{
    Message, base_url, get, input, messages, post, read_reply, schema, silent, warn_if_remote,
};
use crate::{Availability, CandidateView, Judge, JudgeContext, JudgeError, JudgeId, Verdict};

const PROGRAM: &str = "llama-server";
const ID: JudgeId = JudgeId::LlamaServer;
const DEFAULT_HOST: &str = "http://127.0.0.1:8080";
const DEFAULT_PORT: u16 = 8080;

/// The model when neither `KBX_LLAMA_SERVER_MODEL` nor `kbx.toml` names one.
/// The server runs whatever it was started with, whatever it's asked for, so
/// this is only what `verdicts.model` says.
pub(crate) const DEFAULT_MODEL: &str = "default";

/// A local llama.cpp server, on its OpenAI-style API.
#[derive(Debug, Clone)]
pub struct LlamaServer {
    model: String,
    host: String,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    response_format: ResponseFormat<'a>,
    temperature: u8,
    stream: bool,
    /// The per-request form of `--reasoning-budget 0` (§6.2).
    chat_template_kwargs: TemplateOptions,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    r#type: &'static str,
    json_schema: JsonSchema<'a>,
}

#[derive(Serialize)]
struct JsonSchema<'a> {
    name: &'static str,
    strict: bool,
    schema: &'a RawValue,
}

#[derive(Serialize)]
struct TemplateOptions {
    enable_thinking: bool,
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Reply,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Reply {
    content: String,
}

impl LlamaServer {
    /// A backend labelled `model`, at `host` or the usual address.
    pub fn new(model: String, host: Option<&str>) -> LlamaServer {
        LlamaServer {
            model,
            host: base_url(host.unwrap_or(DEFAULT_HOST), DEFAULT_PORT),
        }
    }
}

#[async_trait]
impl Judge for LlamaServer {
    fn id(&self) -> JudgeId {
        ID
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn available(&self) -> Availability {
        match get(&format!("{}/v1/models", self.host)).await {
            Some(_) => Availability::Ready,
            None => silent(PROGRAM, &self.host),
        }
    }

    async fn judge(
        &self,
        batch: &[CandidateView],
        ctx: &JudgeContext,
    ) -> Result<Vec<Verdict>, JudgeError> {
        warn_if_remote(ID, &self.host);
        let input = input(batch);
        let schema = schema(ID, ctx)?;
        let request = Request {
            model: &self.model,
            messages: messages(ctx, &input),
            response_format: ResponseFormat {
                r#type: "json_schema",
                json_schema: JsonSchema {
                    name: "verdicts",
                    strict: true,
                    schema: &schema,
                },
            },
            temperature: 0,
            stream: false,
            chat_template_kwargs: TemplateOptions {
                enable_thinking: false,
            },
        };
        let url = format!("{}/v1/chat/completions", self.host);
        let text = post(ID, &url, &request).await?;
        let response: Option<Response> = serde_json::from_str(&text).ok();
        let choice = response.and_then(|response| response.choices.into_iter().next());
        let Some(choice) = choice else {
            let reason = "not a chat completion".to_string();
            return Err(JudgeError::Malformed(ID, reason));
        };
        if choice.finish_reason.as_deref() == Some("length") {
            return Err(JudgeError::Malformed(
                ID,
                "the reply was cut off".to_string(),
            ));
        }
        read_reply(ID, &choice.message.content, batch, ctx)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::http::fake::{CLOSED, FakeServer, ctx, verdicts, view};

    fn completion(content: &str, finish: &str) -> String {
        json!({"object": "chat.completion", "choices": [
            {"index": 0, "finish_reason": finish,
             "message": {"role": "assistant", "content": content}}
        ]})
        .to_string()
    }

    fn at(server: &FakeServer) -> LlamaServer {
        LlamaServer::new(DEFAULT_MODEL.to_string(), Some(&server.url))
    }

    #[tokio::test]
    async fn it_is_ready_when_v1_models_answers() {
        let models = r#"{"object":"list","data":[{"id":"qwen3-8b.gguf"}]}"#;
        let server = FakeServer::start(|_| (200, models.to_string()));
        assert_eq!(at(&server).available().await, Availability::Ready);
        assert_eq!(server.seen()[0].url, "/v1/models");
    }

    #[tokio::test]
    async fn a_server_still_loading_is_not_ready() {
        let server = FakeServer::start(|_| (503, r#"{"error":"Loading model"}"#.to_string()));
        assert_ne!(at(&server).available().await, Availability::Ready);
    }

    #[tokio::test]
    async fn a_closed_port_is_never_ready() {
        let server = LlamaServer::new(DEFAULT_MODEL.to_string(), Some(CLOSED));
        assert_ne!(server.available().await, Availability::Ready);
    }

    #[tokio::test]
    async fn the_request_is_the_one_in_the_spec() {
        let server = FakeServer::start(|_| (200, completion(&verdicts(&["a"]), "stop")));
        let ctx = ctx();
        let found = at(&server).judge(&[view("a")], &ctx).await.unwrap();
        assert_eq!(found[0].cost_usd, Some(0.0));

        let seen = server.seen();
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].url, "/v1/chat/completions");
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["response_format"]["type"], "json_schema");
        let schema: Value = serde_json::from_str(&ctx.schema).unwrap();
        assert_eq!(body["response_format"]["json_schema"]["schema"], schema);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["content"], ctx.system.as_str());
        assert!(seen[0].body.find("\"rationale\":") < seen[0].body.find("\"confidence\":"));
    }

    #[tokio::test]
    async fn a_reply_that_breaks_the_contract_is_malformed_and_not_quoted() {
        let replies = [
            completion("Sure! SECRET", "stop"),
            completion(&verdicts(&["SECRET"]), "stop"),
            completion(&verdicts(&["a"]), "length"),
            r#"{"choices":[]}"#.to_string(),
            "SECRET".to_string(),
        ];
        for reply in replies {
            let server = FakeServer::start(move |_| (200, reply.clone()));
            let error = at(&server).judge(&[view("a")], &ctx()).await.unwrap_err();
            assert!(matches!(error, JudgeError::Malformed(..)), "{error}");
            assert!(!error.to_string().contains("SECRET"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_server_error_gives_its_status_and_not_its_message() {
        let server = FakeServer::start(|_| (400, r#"{"error":"PRIVATE-EXCERPT"}"#.to_string()));
        let error = at(&server).judge(&[view("a")], &ctx()).await.unwrap_err();
        assert!(error.to_string().contains("400"), "{error}");
        assert!(!error.to_string().contains("PRIVATE-EXCERPT"), "{error}");
    }
}
