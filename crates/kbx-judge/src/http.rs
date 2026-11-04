use std::time::Duration;

use kbx_core::find_on_path;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use tracing::warn;

use crate::process::BATCH_TIMEOUT;
use crate::verdict::{Spend, read_answer};
use crate::{Availability, CandidateView, JudgeContext, JudgeError, JudgeId, Verdict};

/// How long a server gets to answer whether it's there.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What a local model costs, which isn't the same as not saying (§6.3).
const FREE: Spend = Spend {
    cost_usd: Some(0.0),
    cached_tokens: None,
};

/// One turn of a chat, in the form both servers take.
#[derive(Serialize)]
pub(crate) struct Message<'a> {
    role: &'static str,
    content: &'a str,
}

/// The stable prefix, then the batch.
pub(crate) fn messages<'a>(ctx: &'a JudgeContext, input: &'a str) -> [Message<'a>; 2] {
    [
        Message {
            role: "system",
            content: &ctx.system,
        },
        Message {
            role: "user",
            content: input,
        },
    ]
}

/// The batch as the model reads it.
pub(crate) fn input(batch: &[CandidateView]) -> String {
    serde_json::to_string(batch).expect("a batch serializes")
}

/// [`JudgeContext::schema`] as it stands. Going through a `Value` would sort
/// its keys, and the model would fill in `confidence` before `rationale`.
pub(crate) fn schema(id: JudgeId, ctx: &JudgeContext) -> Result<Box<RawValue>, JudgeError> {
    RawValue::from_string(ctx.schema.clone())
        .map_err(|error| JudgeError::Failed(id, error.to_string()))
}

/// A note's text is on its way to a server the user named, so it mustn't take
/// a detour: no proxy from the environment or the system, and no redirect.
fn client() -> reqwest::Result<Client> {
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(PROBE_TIMEOUT)
        .build()
}

/// The body of a successful `GET`, or `None` if `url` didn't give one in time.
pub(crate) async fn get(url: &str) -> Option<String> {
    let request = client().ok()?.get(url).timeout(PROBE_TIMEOUT);
    let response = request.send().await.ok()?;
    let response = response.error_for_status().ok()?;
    response.text().await.ok()
}

/// Sends `body` to `url` and returns the response's. A failure is only ever
/// reported by its status: a server's message could repeat what it was sent.
pub(crate) async fn post(
    id: JudgeId,
    url: &str,
    body: &impl Serialize,
) -> Result<String, JudgeError> {
    let failed = |error: reqwest::Error| {
        if error.is_timeout() {
            JudgeError::TimedOut(id)
        } else {
            JudgeError::Failed(id, error.without_url().to_string())
        }
    };
    let request = client().map_err(failed)?.post(url).json(body);
    let response = request
        .timeout(BATCH_TIMEOUT)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        return Err(JudgeError::Failed(id, format!("HTTP {}", status.as_u16())));
    }
    response.text().await.map_err(failed)
}

/// Checks a model's reply, which is JSON as text, against the batch.
pub(crate) fn read_reply(
    id: JudgeId,
    reply: &str,
    batch: &[CandidateView],
    ctx: &JudgeContext,
) -> Result<Vec<Verdict>, JudgeError> {
    let answer: Value = serde_json::from_str(reply)
        .map_err(|_| JudgeError::Malformed(id, "the reply isn't JSON".to_string()))?;
    read_answer(answer, batch, ctx, FREE).map_err(|reason| JudgeError::Malformed(id, reason))
}

/// What to call a server that didn't answer at `host`: one that runs here and
/// has no program on `PATH` isn't installed.
pub(crate) fn silent(program: &str, host: &str) -> Availability {
    if is_local(host) && find_on_path(program).is_none() {
        Availability::NotInstalled
    } else {
        Availability::Unreachable
    }
}

/// Says so when notes are about to leave the machine: a folder pinned to a
/// local backend is only as local as the host it's configured with.
pub(crate) fn warn_if_remote(id: JudgeId, host: &str) {
    if !is_local(host) {
        warn!("{id} is at {host}, so notes are leaving this machine");
    }
}

fn is_local(host: &str) -> bool {
    let url = Url::parse(host).ok();
    url.and_then(|url| url.host_str().map(str::to_string))
        .is_some_and(|name| {
            let name = name.trim_matches(['[', ']']);
            name == "localhost"
                || name
                    .parse()
                    .is_ok_and(|ip: std::net::IpAddr| ip.is_loopback())
        })
}

/// `host` as a base URL, in any of the forms `OLLAMA_HOST` takes: `:11434`,
/// `127.0.0.1`, `0.0.0.0:11434`, `https://host/`. Without a scheme it's `http`
/// on `port`. A server listening on every address is reached on this machine.
pub(crate) fn base_url(host: &str, port: u16) -> String {
    let host = host.trim().trim_end_matches('/');
    let schemed = host.contains("://");
    let text = match host.strip_prefix(':') {
        Some(port) => format!("http://127.0.0.1:{port}"),
        None if schemed => host.to_string(),
        None => format!("http://{host}"),
    };
    let Ok(mut url) = Url::parse(&text) else {
        return text;
    };
    if url.host_str() == Some("0.0.0.0") {
        let _ = url.set_host(Some("127.0.0.1"));
    }
    if !schemed && url.port().is_none() {
        let _ = url.set_port(Some(port));
    }
    url.as_str().trim_end_matches('/').to_string()
}

#[cfg(test)]
pub(crate) mod fake {
    use std::sync::{Arc, Mutex};
    use std::thread;

    use kbx_core::{EXAMPLE_ONTOLOGY, Ontology};
    use serde_json::{Value, json};
    use tiny_http::{Header, Response, Server};

    use crate::{CandidateView, JudgeContext, NoteView};

    /// A request as it arrived.
    #[derive(Debug, Clone)]
    pub struct Seen {
        pub method: String,
        pub url: String,
        pub body: String,
    }

    /// An HTTP server on a free port that answers every request from `reply`,
    /// which is given the path and returns a status and a body.
    pub struct FakeServer {
        pub url: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl FakeServer {
        pub fn start(reply: impl Fn(&str) -> (u16, String) + Send + 'static) -> FakeServer {
            let server = Server::http("127.0.0.1:0").unwrap();
            let port = server.server_addr().to_ip().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            thread::spawn(move || {
                for mut request in server.incoming_requests() {
                    let mut body = String::new();
                    request.as_reader().read_to_string(&mut body).unwrap();
                    let url = request.url().to_string();
                    let (status, text) = reply(&url);
                    log.lock().unwrap().push(Seen {
                        method: request.method().to_string(),
                        url,
                        body,
                    });
                    let response = Response::from_string(text).with_status_code(status);
                    let _ = request.respond(response.with_header(
                        Header::from_bytes("Location", "http://127.0.0.1:9/elsewhere").unwrap(),
                    ));
                }
            });
            FakeServer {
                url: format!("http://127.0.0.1:{port}"),
                seen,
            }
        }

        pub fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    /// A port nothing listens on.
    pub const CLOSED: &str = "http://127.0.0.1:9";

    pub fn ctx() -> JudgeContext {
        JudgeContext::new(Ontology::parse(EXAMPLE_ONTOLOGY).unwrap(), EXAMPLE_ONTOLOGY)
    }

    pub fn view(cid: &str) -> CandidateView {
        let note = |title: &str| NoteView {
            id: format!("{title}-id"),
            class: "Concept".to_string(),
            title: title.to_string(),
            excerpt: "PRIVATE-EXCERPT".to_string(),
        };
        CandidateView {
            cid: cid.to_string(),
            src: note("a"),
            dst: note("b"),
            why_proposed: Vec::new(),
        }
    }

    /// A reply to `cids`, as the JSON text a model writes.
    pub fn verdicts(cids: &[&str]) -> String {
        let row = |cid: &&str| {
            json!({"cid": cid, "rationale": "Unrelated.", "verdict": "reject",
                   "rel": "none", "confidence": 0.5})
        };
        let rows: Vec<Value> = cids.iter().map(row).collect();
        json!({ "verdicts": rows }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form_of_ollama_host_becomes_a_base_url() {
        let url = |host| base_url(host, 11434);
        assert_eq!(url("http://127.0.0.1:11434"), "http://127.0.0.1:11434");
        assert_eq!(url("http://127.0.0.1:11434/"), "http://127.0.0.1:11434");
        assert_eq!(url("127.0.0.1"), "http://127.0.0.1:11434");
        assert_eq!(url("localhost:1234"), "http://localhost:1234");
        assert_eq!(url(":1234"), "http://127.0.0.1:1234");
        assert_eq!(url("0.0.0.0:11434"), "http://127.0.0.1:11434");
        assert_eq!(url("https://models.example"), "https://models.example");
    }

    #[test]
    fn only_this_machine_is_local() {
        assert!(is_local("http://127.0.0.1:11434"));
        assert!(is_local("http://localhost:11434"));
        assert!(is_local("http://[::1]:11434"));
        assert!(!is_local("http://10.0.0.5:11434"));
        assert!(!is_local("https://models.example"));
        assert!(!is_local("not a url"));
    }
}
