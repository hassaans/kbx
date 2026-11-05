//! Drives the judge wiring in `kbx suggest` and `kbx doctor` against fake
//! `claude` and `codex` programs on a fake `PATH`. Their output is copied from
//! claude 2.1.307 and codex 0.166.0. No test here may reach a real CLI.
//!
//! The fakes are `sh`, so this file is Unix only: on Windows `kbx doctor`'s
//! `NotInstalled` rows are all that the other tests cover.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::{CLOSED_PORT, FakeModel, FakePath, Fixture, id, stderr, stdout, write_executable};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PROMPT: &str = include_str!("../../../prompts/judge.system.md");
const SCHEMA: &str = include_str!("../../../schema/judge.schema.json");

/// The pairs of one note, few enough for one batch.
const NARROW: [&str; 4] = ["--generators", "schema_gap", "--note", NOTE];
const NOTE: &str = "01KBX000000000000000000010";

const LOGGED_IN: &str = r#"{"loggedIn": true, "authMethod": "claude.ai"}"#;
const LOGGED_OUT: &str = r#"{"loggedIn": false, "authMethod": "none"}"#;
const PROBE_OK: &str =
    r#"{"type":"result","is_error":false,"structured_output":{"ok":true},"total_cost_usd":0.0008}"#;
/// What a logged-out `claude -p` prints, with exit code 0.
const NOT_LOGGED_IN: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":null,"result":"Not logged in · Please run /login","total_cost_usd":0}"#;

const CODEX_PROBE_OK: &str = r#"{"type":"thread.started","thread_id":"t"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"ok\":true}"}}
{"type":"turn.completed","usage":{"input_tokens":20593,"cached_input_tokens":0,"output_tokens":66}}"#;
const CODEX_401: &str = r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}}"#;

/// Puts a `claude` on the path that answers from files beside it. The fake
/// `PATH` has nothing else on it, hence `/bin/cat`.
fn fake_claude(path: &FakePath) {
    path.tool(
        "claude",
        r#"d="${0%/*}"
if [ "$1" = auth ]; then
  /bin/cat "$d/claude-auth.out"
  exit "$(/bin/cat "$d/claude-auth.code")"
fi
case "$2" in
  'reply with'*) step=probe ;;
  *) step=batch
     printf '%s\n' "$@" > "$d/claude.args"
     pwd > "$d/claude.cwd"
     echo "$CLAUDE_CODE_DISABLE_AUTO_MEMORY" > "$d/claude.memory"
     /bin/ls -A > "$d/claude.listing"
     /bin/cat > "$d/claude.stdin"
     /bin/cat "$d/claude.args" "$d/claude.stdin" >> "$d/claude.seen" ;;
esac
echo "$step" >> "$d/claude.log"
/bin/cat "$d/claude-$step.out"
exit "$(/bin/cat "$d/claude-$step.code")""#,
    );
}

/// Puts a `codex` on the path that answers from files beside it.
fn fake_codex(path: &FakePath) {
    path.tool(
        "codex",
        r#"d="${0%/*}"
if [ "$1" = login ]; then
  /bin/cat "$d/codex-login.out"
  exit "$(/bin/cat "$d/codex-login.code")"
fi
/bin/cat > "$d/codex.in"
if [ "$(/bin/cat "$d/codex.in")" = 'reply with ok' ]; then
  step=probe
else
  step=batch
  /bin/mv "$d/codex.in" "$d/codex.stdin"
  printf '%s\n' "$@" > "$d/codex.args"
fi
echo "$step" >> "$d/codex.log"
/bin/cat "$d/codex-$step.out"
exit "$(/bin/cat "$d/codex-$step.code")""#,
    );
}

/// Sets what a fake prints and exits with at `step`.
fn answer(path: &FakePath, step: &str, code: i32, text: &str) {
    fs::write(path.dir().join(format!("{step}.out")), text).unwrap();
    fs::write(path.dir().join(format!("{step}.code")), code.to_string()).unwrap();
}

fn logged_in_claude(path: &FakePath) {
    fake_claude(path);
    answer(path, "claude-auth", 0, LOGGED_IN);
    answer(path, "claude-probe", 0, PROBE_OK);
    answer(path, "claude-batch", 1, "");
}

fn logged_out_claude(path: &FakePath) {
    fake_claude(path);
    answer(path, "claude-auth", 1, LOGGED_OUT);
    answer(path, "claude-probe", 0, NOT_LOGGED_IN);
    answer(path, "claude-batch", 0, NOT_LOGGED_IN);
}

fn logged_in_codex(path: &FakePath) {
    fake_codex(path);
    answer(path, "codex-login", 0, "Logged in using ChatGPT\n");
    answer(path, "codex-probe", 0, CODEX_PROBE_OK);
    answer(path, "codex-batch", 1, "");
}

fn logged_out_codex(path: &FakePath) {
    fake_codex(path);
    answer(path, "codex-login", 1, "Not logged in\n");
    answer(path, "codex-probe", 1, CODEX_401);
    answer(path, "codex-batch", 1, CODEX_401);
}

/// The steps a fake has run, in order.
fn calls(path: &FakePath, program: &str) -> Vec<String> {
    let log = fs::read_to_string(path.dir().join(format!("{program}.log")));
    log.unwrap_or_default().lines().map(String::from).collect()
}

fn indexed() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault
}

fn suggest(vault: &Fixture, judge: &str) -> std::process::Output {
    let mut args = vec!["suggest", "--judge", judge];
    args.extend(NARROW);
    vault.kbx(&args)
}

fn database(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

fn cids(vault: &Fixture) -> Vec<String> {
    let db = database(vault);
    let mut statement = db
        .prepare("SELECT cid FROM candidates ORDER BY cid")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn verdict_count(vault: &Fixture) -> i64 {
    database(vault)
        .query_row("SELECT COUNT(*) FROM verdicts", [], |row| row.get(0))
        .unwrap()
}

/// The stored reason for each pair marked `error`.
fn error_rationales(vault: &Fixture) -> Vec<String> {
    let db = database(vault);
    let mut statement = db
        .prepare("SELECT rationale FROM verdicts WHERE verdict = 'error'")
        .unwrap();
    let rows = statement.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

/// The verdicts a judge would give for every narrow candidate. Learns the cids
/// from a run with no judge, then clears them, as a first run would see it.
fn answers(vault: &Fixture) -> Vec<Value> {
    let output = suggest(vault, "none");
    assert!(output.status.success(), "{}", stderr(&output));
    let cids = cids(vault);
    assert!((2..=20).contains(&cids.len()), "{} pairs", cids.len());
    let rows = cids.iter().map(|cid| {
        json!({"cid": cid, "rationale": "Both cover restarts.", "verdict": "reject",
               "rel": "none", "confidence": 0.75})
    });
    rows.collect()
}

fn claude_result(verdicts: &[Value], cost: f64, cached: i64) -> String {
    json!({
        "type": "result", "subtype": "success", "is_error": false,
        "structured_output": {"verdicts": verdicts},
        "total_cost_usd": cost,
        "usage": {"input_tokens": 10, "cache_read_input_tokens": cached, "output_tokens": 50},
    })
    .to_string()
}

fn codex_events(verdicts: &[Value], cached: i64) -> String {
    let message = json!({"verdicts": verdicts}).to_string();
    let item = json!({"type": "item.completed",
                      "item": {"id": "item_0", "type": "agent_message", "text": message}});
    let turn = json!({"type": "turn.completed",
                      "usage": {"input_tokens": 9000, "cached_input_tokens": cached,
                                "output_tokens": 300}});
    format!("{{\"type\":\"thread.started\"}}\n{item}\n{turn}\n")
}

/// A logged-in `claude` that rejects whatever pairs it's sent, at `cost` a call,
/// so that a run can have any number of batches.
fn answering_claude(path: &FakePath, cost: f64) {
    path.tool(
        "claude",
        &format!(
            r#"d="${{0%/*}}"
if [ "$1" = auth ]; then
  echo '{LOGGED_IN}'
  exit 0
fi
case "$2" in
  'reply with'*) echo '{PROBE_OK}'; exit 0 ;;
esac
echo batch >> "$d/claude.log"
items=$(/usr/bin/grep -o '"cid":"[0-9a-f]*"' \
  | /usr/bin/sed 's/^/{{/; s/$/,"rationale":"r","verdict":"reject","rel":"none","confidence":0.5}}/' \
  | /usr/bin/paste -s -d , -)
echo "{{\"is_error\":false,\"structured_output\":{{\"verdicts\":[$items]}},\"total_cost_usd\":{cost},\"usage\":{{}}}}""#
        ),
    );
}

/// How many batches a fake `claude` has been sent.
fn batches(path: &FakePath) -> usize {
    let steps = calls(path, "claude");
    steps.iter().filter(|step| *step == "batch").count()
}

fn status_of<'a>(output: &'a str, check: &str) -> Vec<&'a str> {
    let row = output
        .lines()
        .find(|line| line.split_whitespace().next() == Some(check));
    let row = row.unwrap_or_else(|| panic!("no `{check}` row in:\n{output}"));
    row.split_whitespace().skip(1).collect()
}

mod doctor {
    use super::*;

    #[test]
    fn a_logged_in_cli_is_ready_and_chosen() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        logged_in_codex(&vault.path);
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "claude-cli"), ["ok", "Ready"], "{text}");
        assert_eq!(status_of(&text, "codex-cli"), ["ok", "Ready"], "{text}");
        let chosen = status_of(&text, "judge");
        assert_eq!(chosen[..2], ["ok", "claude-cli"], "{text}");
    }

    #[test]
    fn the_order_in_kbx_toml_decides_which_is_chosen() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        logged_in_codex(&vault.path);
        let config = vault.root().join(".kbx/kbx.toml");
        let text = fs::read_to_string(&config).unwrap();
        let swapped = text.replace(
            r#"order = ["claude-cli", "codex-cli","#,
            r#"order = ["codex-cli", "claude-cli","#,
        );
        assert_ne!(text, swapped);
        fs::write(config, swapped).unwrap();
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "judge")[1], "codex-cli", "{text}");
    }

    #[test]
    fn a_logged_out_cli_is_reported_without_spending_a_probe() {
        let vault = indexed();
        logged_out_claude(&vault.path);
        logged_out_codex(&vault.path);
        let output = vault.kbx(&["doctor"]);
        let text = stdout(&output);
        assert_eq!(status_of(&text, "claude-cli"), ["warn", "NotLoggedIn"]);
        assert_eq!(status_of(&text, "codex-cli"), ["warn", "NotLoggedIn"]);
        assert_eq!(status_of(&text, "judge")[0], "warn", "{text}");
        assert!(calls(&vault.path, "claude").is_empty());
        assert!(calls(&vault.path, "codex").is_empty());
        assert!(output.status.success(), "a missing login breaks nothing");
    }

    #[test]
    fn a_login_that_looks_fine_but_fails_the_probe_is_not_logged_in() {
        let probes = [
            (0, NOT_LOGGED_IN),
            (1, ""),
            (1, r#"{"is_error":true,"result":"authentication_failed"}"#),
        ];
        for (code, text) in probes {
            let vault = indexed();
            logged_in_claude(&vault.path);
            answer(&vault.path, "claude-probe", code, text);
            let text = stdout(&vault.kbx(&["doctor"]));
            assert_eq!(status_of(&text, "claude-cli")[1], "NotLoggedIn", "{text}");
        }

        let vault = indexed();
        logged_in_codex(&vault.path);
        answer(&vault.path, "codex-probe", 1, CODEX_401);
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "codex-cli")[1], "NotLoggedIn", "{text}");
    }

    #[test]
    fn a_claude_without_auth_status_is_left_to_the_probe() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        answer(
            &vault.path,
            "claude-auth",
            1,
            "error: unknown command 'auth'\n",
        );
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "claude-cli")[1], "Ready", "{text}");
    }

    #[test]
    fn a_missing_cli_is_not_installed() {
        let vault = indexed();
        let output = vault.kbx(&["doctor"]);
        let text = stdout(&output);
        assert_eq!(status_of(&text, "claude-cli"), ["warn", "NotInstalled"]);
        assert_eq!(status_of(&text, "codex-cli"), ["warn", "NotInstalled"]);
        assert!(output.status.success());
    }

    #[test]
    fn it_works_outside_a_vault() {
        let path = FakePath::with_qmd();
        logged_in_claude(&path);
        let dir = tempfile::TempDir::new().unwrap();
        let text = stdout(&path.kbx(dir.path(), &["doctor"]));
        assert_eq!(status_of(&text, "claude-cli")[1], "Ready", "{text}");
    }
}

mod probe_cache {
    use super::*;

    fn cache_file(vault: &Fixture) -> std::path::PathBuf {
        vault.root().join(".kbx/judge-cache.json")
    }

    #[test]
    fn a_probe_runs_once_a_day() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        logged_in_codex(&vault.path);
        vault.kbx(&["doctor"]);
        vault.kbx(&["doctor"]);
        assert_eq!(calls(&vault.path, "claude"), ["probe"]);
        assert_eq!(calls(&vault.path, "codex"), ["probe"]);
    }

    #[test]
    fn an_old_probe_is_run_again() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        vault.kbx(&["doctor"]);
        let old = r#"{"claude-cli": {"availability": "ready", "checked_at": 1000}}"#;
        fs::write(cache_file(&vault), old).unwrap();
        vault.kbx(&["doctor"]);
        assert_eq!(calls(&vault.path, "claude"), ["probe", "probe"]);
    }

    #[test]
    fn the_cache_holds_no_more_than_availability_and_time() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        vault.kbx(&["doctor"]);
        let text = fs::read_to_string(cache_file(&vault)).unwrap();
        let entries: Value = serde_json::from_str(&text).unwrap();
        let entry = entries["claude-cli"].as_object().unwrap();
        assert_eq!(entry.len(), 2, "{text}");
        assert_eq!(entry["availability"], "ready");
        assert!(entry["checked_at"].is_i64());
    }

    #[test]
    fn logging_out_beats_a_cached_probe() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        vault.kbx(&["doctor"]);
        answer(&vault.path, "claude-auth", 1, LOGGED_OUT);
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "claude-cli")[1], "NotLoggedIn", "{text}");
    }

    #[test]
    fn logging_in_takes_effect_at_once() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        answer(&vault.path, "claude-probe", 0, NOT_LOGGED_IN);
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "claude-cli")[1], "NotLoggedIn", "{text}");

        answer(&vault.path, "claude-probe", 0, PROBE_OK);
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(status_of(&text, "claude-cli")[1], "Ready", "{text}");
    }

    #[test]
    fn a_login_lost_mid_run_is_forgotten() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let _ = answers(&vault);
        answer(&vault.path, "claude-batch", 0, NOT_LOGGED_IN);
        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(verdict_count(&vault), 0);
        assert!(stderr(&output).contains("not logged in"));

        suggest(&vault, "claude-cli");
        assert_eq!(
            calls(&vault.path, "claude"),
            ["probe", "batch", "probe", "batch"]
        );
    }
}

mod batches {
    use super::*;

    #[test]
    fn claude_verdicts_are_stored_with_their_cost_and_cached_tokens() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        let result = claude_result(&verdicts, 0.012, 4096);
        answer(&vault.path, "claude-batch", 0, &result);

        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        let text = stdout(&output);
        assert!(text.contains("judged by claude-cli (haiku)"), "{text}");
        // The batch, and the $0.0008 probe, which `verdicts` doesn't hold.
        assert!(text.contains("$0.0128, 4096 cached tokens"), "{text}");

        let db = database(&vault);
        let (count, cost, cached): (i64, f64, i64) = db
            .query_row(
                "SELECT COUNT(*), SUM(cost_usd), SUM(cached_tokens) FROM verdicts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count as usize, verdicts.len());
        assert!((cost - 0.012).abs() < 1e-9, "{cost}");
        assert_eq!(cached, 4096);

        let row: (String, String, String, String, f64, String) = db
            .query_row(
                "SELECT backend, model, verdict, rel, confidence, rationale FROM verdicts",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        let expected = ("claude-cli", "haiku", "reject", "none");
        assert_eq!((&*row.0, &*row.1, &*row.2, &*row.3), expected);
        assert_eq!(row.4, 0.75);
        assert_eq!(row.5, "Both cover restarts.");
    }

    #[test]
    fn claude_is_invoked_as_the_spec_says() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        suggest(&vault, "claude-cli");

        let read = |name: &str| fs::read_to_string(vault.path.dir().join(name)).unwrap();
        let args = read("claude.args");
        let args: Vec<_> = args.lines().collect();
        let after = |flag: &str| {
            let at = args.iter().position(|arg| *arg == flag);
            args[at.unwrap_or_else(|| panic!("no {flag} in {args:?}")) + 1]
        };
        assert_eq!(args[0], "-p");
        assert_eq!(after("--output-format"), "json");
        assert_eq!(after("--model"), "haiku");
        assert_eq!(after("--max-turns"), "1");
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert_eq!(after("--permission-prompts"), "none");
        assert_eq!(after("--setting-sources"), "user");
        assert!(args.contains(&"--no-session-persistence"));
        assert!(!args.contains(&"--bare"));
        let schema: Value = serde_json::from_str(after("--json-schema")).unwrap();
        assert_eq!(schema, serde_json::from_str::<Value>(SCHEMA).unwrap());
        assert!(after("--append-system-prompt-file").ends_with("system.md"));

        assert_eq!(read("claude.memory").trim(), "1");
        assert_eq!(read("claude.listing"), "", "the cwd must be empty");
        let cwd = read("claude.cwd");
        assert!(!Path::new(cwd.trim()).starts_with(vault.root().canonicalize().unwrap()));
        assert!(
            !Path::new(cwd.trim()).exists(),
            "the temp dir is cleaned up"
        );

        let batch: Value = serde_json::from_str(&read("claude.stdin")).unwrap();
        let item = &batch[0];
        assert_eq!(batch.as_array().unwrap().len(), verdicts.len());
        assert_eq!(item["src"]["type"], "Procedure");
        assert!(item["src"]["title"].is_string());
        let excerpt = item["src"]["excerpt"].as_str().unwrap();
        assert!(!excerpt.is_empty() && !excerpt.contains("---"), "{excerpt}");
        assert!(
            item["why_proposed"][0]
                .as_str()
                .unwrap()
                .starts_with("schema_gap:depends_on")
        );
    }

    #[test]
    fn codex_verdicts_are_stored_with_cached_input_tokens_and_an_estimated_cost() {
        let vault = indexed();
        logged_in_codex(&vault.path);
        let verdicts = answers(&vault);
        answer(
            &vault.path,
            "codex-batch",
            0,
            &codex_events(&verdicts, 2048),
        );

        let output = suggest(&vault, "codex-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("judged by codex-cli (gpt-6-luna)"));

        let (count, cost, cached, backend): (i64, Option<f64>, i64, String) = database(&vault)
            .query_row(
                "SELECT COUNT(*), SUM(cost_usd), SUM(cached_tokens), MIN(backend) FROM verdicts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(count as usize, verdicts.len());
        // 6,952 fresh and 2,048 cached input tokens and 300 output, at the defaults.
        let expected = (6952.0 * 0.10 + 2048.0 * 0.01 + 300.0 * 0.50) / 1_000_000.0;
        assert!((cost.unwrap() - expected).abs() < 1e-9, "{cost:?}");
        assert_eq!(cached, 2048);
        assert_eq!(backend, "codex-cli");
    }

    #[test]
    fn codex_is_invoked_as_the_spec_says_with_the_prompt_first_on_stdin() {
        let vault = indexed();
        logged_in_codex(&vault.path);
        let verdicts = answers(&vault);
        answer(&vault.path, "codex-batch", 0, &codex_events(&verdicts, 0));
        suggest(&vault, "codex-cli");

        let read = |name: &str| fs::read_to_string(vault.path.dir().join(name)).unwrap();
        let args = read("codex.args");
        let args: Vec<_> = args.lines().collect();
        for flag in [
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "-",
        ] {
            assert!(args.contains(&flag), "{flag} missing from {args:?}");
        }
        let after = |flag: &str| args[args.iter().position(|a| *a == flag).unwrap() + 1];
        assert_eq!(after("--sandbox"), "read-only");
        assert_eq!(after("--model"), "gpt-6-luna");
        assert!(after("--output-schema").ends_with("schema.json"));
        assert!(after("-C").ends_with("empty"));

        let stdin = read("codex.stdin");
        assert!(
            stdin.starts_with(PROMPT),
            "the prefix must be byte-identical"
        );
        assert!(stdin.contains(&verdicts[0]["cid"].as_str().unwrap().to_string()));
    }

    #[test]
    fn verdicts_carry_the_hash_of_the_prompt_the_schema_and_the_ontology() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        suggest(&vault, "claude-cli");

        let ontology = fs::read(vault.root().join(".kbx/ontology.yaml")).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(PROMPT);
        hasher.update(SCHEMA);
        hasher.update(&ontology);
        let expected: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        let stored: String = database(&vault)
            .query_row("SELECT DISTINCT prompt_hash FROM verdicts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored, expected);
    }

    #[test]
    fn the_model_comes_from_the_environment_then_kbx_toml() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        let config = vault.root().join(".kbx/kbx.toml");
        let text = fs::read_to_string(&config).unwrap();
        let text = text.replace(r#"model = "haiku""#, r#"model = "from-toml""#);
        fs::write(&config, text).unwrap();
        suggest(&vault, "claude-cli");

        let mut args = vec!["suggest", "--judge", "claude-cli"];
        args.extend(NARROW);
        let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
            .args(args)
            .current_dir(vault.root())
            .env("PATH", vault.path.dir())
            .env("KBX_CLAUDE_MODEL", "from-env")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));

        let db = database(&vault);
        let mut statement = db
            .prepare("SELECT DISTINCT model FROM verdicts ORDER BY model")
            .unwrap();
        let rows = statement.query_map([], |row| row.get::<_, String>(0));
        let models: Vec<_> = rows.unwrap().map(Result::unwrap).collect();
        assert_eq!(models, ["from-env", "from-toml"]);
    }

    #[test]
    fn a_malformed_claude_response_is_marked_error_and_quotes_nothing() {
        let bad = [
            "NOTE-TEXT is not json".to_string(),
            r#"{"is_error":false,"result":"NOTE-TEXT in prose"}"#.to_string(),
            json!({"is_error": false, "structured_output": {"verdicts": [{"cid": "NOTE-TEXT"}]}})
                .to_string(),
        ];
        for text in bad {
            let vault = indexed();
            logged_in_claude(&vault.path);
            let _ = answers(&vault);
            let before = cids(&vault);
            answer(&vault.path, "claude-batch", 0, &text);

            let output = suggest(&vault, "claude-cli");
            assert!(output.status.success(), "{}", stderr(&output));
            assert_eq!(error_rationales(&vault).len(), before.len());
            assert_eq!(verdict_count(&vault) as usize, before.len());
            assert_eq!(cids(&vault), before, "the candidates stay");
            let (out, err) = (stdout(&output), stderr(&output));
            assert!(err.contains("malformed response"), "{err}");
            assert!(out.contains(&format!("{} error", before.len())), "{out}");
            assert!(!error_rationales(&vault).concat().contains("NOTE-TEXT"));
            assert!(
                !err.contains("NOTE-TEXT") && !out.contains("NOTE-TEXT"),
                "{err}"
            );
        }
    }

    #[test]
    fn a_malformed_codex_response_is_marked_error_and_quotes_nothing() {
        let vault = indexed();
        logged_in_codex(&vault.path);
        let _ = answers(&vault);
        let prose = r#"{"type":"item.completed","item":{"type":"agent_message","text":"NOTE-TEXT"}}
{"type":"turn.completed","usage":{"cached_input_tokens":0}}"#;
        answer(&vault.path, "codex-batch", 0, prose);

        let output = suggest(&vault, "codex-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        let reasons = error_rationales(&vault);
        assert_eq!(reasons.len() as i64, verdict_count(&vault));
        assert!(!reasons.is_empty());
        assert!(!reasons.concat().contains("NOTE-TEXT"));
        let err = stderr(&output);
        assert!(
            err.contains("malformed response") && !err.contains("NOTE-TEXT"),
            "{err}"
        );
    }

    #[test]
    fn a_failed_batch_does_not_stop_the_run() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let _ = answers(&vault);
        answer(&vault.path, "claude-batch", 1, "");
        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("claude-cli failed"));
    }

    #[test]
    fn kbx_accept_writes_the_relation_a_judge_named() {
        let vault = indexed().writing();
        logged_in_claude(&vault.path);
        let mut verdicts = answers(&vault);
        verdicts[0]["verdict"] = json!("accept");
        verdicts[0]["rel"] = json!("depends_on");
        let cid = verdicts[0]["cid"].as_str().unwrap().to_string();
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));

        vault.ok(&["accept", &cid]);
        let note = vault
            .root()
            .join("operations/01KBX000000000000000000010-restart-service.md");
        let text = fs::read_to_string(note).unwrap();
        assert!(text.contains("depends_on"), "{text}");
    }

    #[test]
    fn excerpts_stay_out_of_the_logs_and_the_vault_state() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        let mut args = vec!["suggest", "--judge", "claude-cli"];
        args.extend(NARROW);
        let output = Command::new(env!("CARGO_BIN_EXE_kbx"))
            .args(args)
            .current_dir(vault.root())
            .env("PATH", vault.path.dir())
            .env("KBX_LOG", "trace")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));

        let stdin = fs::read_to_string(vault.path.dir().join("claude.stdin")).unwrap();
        let batch: Value = serde_json::from_str(&stdin).unwrap();
        let excerpt = batch[0]["src"]["excerpt"].as_str().unwrap();
        assert!(!stderr(&output).contains(excerpt));
        assert!(!stdout(&output).contains(excerpt));
        let cache = fs::read_to_string(vault.root().join(".kbx/judge-cache.json")).unwrap();
        assert!(!cache.contains(excerpt));
    }
}

mod selection {
    use super::*;

    #[test]
    fn auto_skips_a_logged_out_cli_for_the_next() {
        let vault = indexed();
        logged_out_claude(&vault.path);
        logged_in_codex(&vault.path);
        let verdicts = answers(&vault);
        answer(&vault.path, "codex-batch", 0, &codex_events(&verdicts, 0));

        let output = suggest(&vault, "auto");
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).contains("claude-cli skipped: NotLoggedIn"));
        assert!(stdout(&output).contains("judged by codex-cli"));
        assert_eq!(verdict_count(&vault) as usize, verdicts.len());
    }

    #[test]
    fn auto_stops_at_the_first_ready_backend() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        logged_in_codex(&vault.path);
        let verdicts = answers(&vault);
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        suggest(&vault, "auto");
        assert!(calls(&vault.path, "codex").is_empty());
    }

    #[test]
    fn with_every_cli_logged_out_auto_still_stores_the_candidates() {
        let vault = indexed();
        logged_out_claude(&vault.path);
        logged_out_codex(&vault.path);
        let output = suggest(&vault, "auto");
        assert!(output.status.success(), "{}", stderr(&output));
        let (out, err) = (stdout(&output), stderr(&output));
        assert!(err.contains("claude-cli skipped: NotLoggedIn"), "{err}");
        assert!(err.contains("codex-cli skipped: NotLoggedIn"), "{err}");
        assert!(out.contains("no judge available"), "{out}");
        assert!(!cids(&vault).is_empty());
        assert_eq!(verdict_count(&vault), 0);
    }

    #[test]
    fn with_no_cli_installed_auto_still_stores_the_candidates() {
        let vault = indexed();
        let output = suggest(&vault, "auto");
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(!cids(&vault).is_empty());
        assert!(stdout(&output).contains("no judge available"));
        assert!(!stderr(&output).contains("skipped"), "{}", stderr(&output));
    }

    #[test]
    fn a_backend_asked_for_by_name_fails_loudly_but_keeps_the_candidates() {
        let vault = indexed();
        logged_out_claude(&vault.path);
        let output = suggest(&vault, "claude-cli");
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("NotLoggedIn"),
            "{}",
            stderr(&output)
        );
        assert!(!cids(&vault).is_empty());
    }

    #[test]
    fn a_bare_suggest_judges_with_the_first_ready_backend() {
        let vault = indexed();
        answering_claude(&vault.path, 0.0);
        let mut args = vec!["suggest"];
        args.extend(NARROW);
        let out = vault.ok(&args);
        assert!(out.contains("judged by claude-cli"), "{out}");
        assert!(batches(&vault.path) > 0);
    }

    #[test]
    fn judge_none_calls_nobody() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        vault.ok(&["suggest", "--judge", "none"]);
        assert!(calls(&vault.path, "claude").is_empty());
    }

    #[test]
    fn a_dry_run_calls_nothing() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        vault.ok(&["suggest", "--judge", "auto", "--dry-run"]);
        assert!(calls(&vault.path, "claude").is_empty());
        let db = database(&vault);
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM candidates", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn a_folder_pinned_to_another_backend_never_reaches_this_one() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        answer(&vault.path, "claude-batch", 0, &claude_result(&[], 0.0, 0));
        let config = vault.root().join(".kbx/kbx.toml");
        let mut text = fs::read_to_string(&config).unwrap();
        // The note in `NARROW` is in `operations`.
        text.push_str("\"operations\" = \"ollama\"\n");
        fs::write(&config, text).unwrap();

        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        let err = stderr(&output);
        let note = format!("{} pairs kept from claude-cli", verdicts.len());
        assert!(err.contains(&note), "{err}");
        assert_eq!(calls(&vault.path, "claude"), ["probe"], "no batch may run");
        assert!(!vault.path.dir().join("claude.stdin").exists());
        assert_eq!(id(10), NOTE);
    }
}

/// The verdicts for the pairs in a chat request, as the text a model writes.
fn reply_to(body: &str) -> String {
    let body: Value = serde_json::from_str(body).unwrap();
    let batch = body["messages"][1]["content"].as_str().unwrap();
    let batch: Vec<Value> = serde_json::from_str(batch).unwrap();
    let rows = batch.iter().map(|pair| {
        json!({"cid": pair["cid"], "rationale": "Unrelated.", "verdict": "reject",
               "rel": "none", "confidence": 0.5})
    });
    json!({"verdicts": rows.collect::<Vec<_>>()}).to_string()
}

/// An Ollama with `models` pulled, that judges whatever it's sent.
fn fake_ollama(models: &'static [&'static str]) -> FakeModel {
    FakeModel::start(move |path, body| match path {
        "/api/tags" => {
            let rows: Vec<Value> = models.iter().map(|name| json!({"name": name})).collect();
            (200, json!({"models": rows}).to_string())
        }
        "/api/chat" => {
            let message = json!({"role": "assistant", "content": reply_to(body)});
            let chat = json!({"message": message, "done": true, "done_reason": "stop"});
            (200, chat.to_string())
        }
        _ => (404, String::new()),
    })
}

/// A llama-server that judges whatever it's sent.
fn fake_llama_server() -> FakeModel {
    FakeModel::start(|path, body| match path {
        "/v1/models" => (
            200,
            r#"{"object":"list","data":[{"id":"m.gguf"}]}"#.to_string(),
        ),
        "/v1/chat/completions" => {
            let message = json!({"role": "assistant", "content": reply_to(body)});
            let choice = json!({"index": 0, "finish_reason": "stop", "message": message});
            (200, json!({"choices": [choice]}).to_string())
        }
        _ => (404, String::new()),
    })
}

/// `kbx suggest` with `env`, on the pairs of `NOTE`.
fn suggest_with(vault: &Fixture, judge: &str, env: &[(&str, &str)]) -> std::process::Output {
    let mut args = vec!["suggest", "--judge", judge];
    args.extend(NARROW);
    vault.kbx_with(&args, env)
}

fn pin(vault: &Fixture, folder: &str, backend: &str) {
    let config = vault.root().join(".kbx/kbx.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!("\"{folder}\" = \"{backend}\"\n"));
    fs::write(&config, text).unwrap();
}

mod local {
    use super::*;

    const MODEL: (&str, &str) = ("KBX_OLLAMA_MODEL", "qwen3:8b");
    /// From the note in `NARROW`, which is in `operations`.
    const PINNED_TEXT: &str = "Restart";

    fn stored(vault: &Fixture) -> Vec<(String, String, f64)> {
        let db = database(vault);
        let mut statement = db
            .prepare("SELECT DISTINCT backend, model, cost_usd FROM verdicts")
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    }

    #[test]
    fn doctor_lists_all_four_backends_with_each_kind_of_answer() {
        let vault = indexed();
        logged_out_claude(&vault.path);
        let ollama = fake_ollama(&["qwen3:8b"]);
        let env = [("OLLAMA_HOST", ollama.url.as_str()), MODEL];
        let text = stdout(&vault.kbx_with(&["doctor"], &env));
        assert_eq!(
            status_of(&text, "claude-cli"),
            ["warn", "NotLoggedIn"],
            "{text}"
        );
        assert_eq!(
            status_of(&text, "codex-cli"),
            ["warn", "NotInstalled"],
            "{text}"
        );
        assert_eq!(
            status_of(&text, "ollama"),
            ["ok", "Ready", "(qwen3:8b)"],
            "{text}"
        );
        assert_eq!(
            status_of(&text, "llama-server"),
            ["warn", "NotInstalled"],
            "{text}"
        );
        assert_eq!(status_of(&text, "judge")[..2], ["ok", "ollama"], "{text}");
    }

    #[test]
    fn doctor_tells_a_stopped_server_from_a_missing_one() {
        let vault = indexed();
        vault.path.tool("ollama", "");
        vault.path.tool("llama-server", "");
        let text = stdout(&vault.kbx(&["doctor"]));
        assert_eq!(
            status_of(&text, "ollama")[..2],
            ["warn", "Unreachable"],
            "{text}"
        );
        assert_eq!(
            status_of(&text, "llama-server"),
            ["warn", "Unreachable"],
            "{text}"
        );
    }

    #[test]
    fn doctor_says_not_installed_of_a_model_not_pulled() {
        let vault = indexed();
        let ollama = fake_ollama(&["llama3:8b"]);
        let env = [("OLLAMA_HOST", ollama.url.as_str()), MODEL];
        let text = stdout(&vault.kbx_with(&["doctor"], &env));
        assert_eq!(
            status_of(&text, "ollama")[..2],
            ["warn", "NotInstalled"],
            "{text}"
        );
    }

    #[test]
    fn doctor_lists_a_backend_left_out_of_the_order_but_never_chooses_it() {
        let vault = indexed();
        let config = vault.root().join(".kbx/kbx.toml");
        let text = fs::read_to_string(&config).unwrap();
        let cut = r#"order = ["codex-cli"]"#;
        let text = text.replace(
            r#"order = ["claude-cli", "codex-cli", "ollama", "llama-server"]"#,
            cut,
        );
        assert!(text.contains(cut));
        fs::write(&config, text).unwrap();
        let llama = fake_llama_server();
        let env = [("KBX_LLAMA_SERVER_HOST", llama.url.as_str())];
        let text = stdout(&vault.kbx_with(&["doctor"], &env));
        assert_eq!(status_of(&text, "llama-server"), ["ok", "Ready"], "{text}");
        assert_eq!(
            status_of(&text, "claude-cli"),
            ["warn", "NotInstalled"],
            "{text}"
        );
        assert_eq!(status_of(&text, "judge")[..2], ["warn", "none"], "{text}");
    }

    #[test]
    fn ollama_verdicts_are_stored_at_no_cost() {
        let vault = indexed();
        let verdicts = answers(&vault);
        let ollama = fake_ollama(&["qwen3:8b"]);
        let env = [("OLLAMA_HOST", ollama.url.as_str()), MODEL];
        let output = suggest_with(&vault, "ollama", &env);
        assert!(output.status.success(), "{}", stderr(&output));
        let out = stdout(&output);
        assert!(out.contains("judged by ollama (qwen3:8b)"), "{out}");
        assert!(out.contains("$0.0000"), "{out}");
        assert_eq!(verdict_count(&vault) as usize, verdicts.len());
        assert_eq!(
            stored(&vault),
            [("ollama".to_string(), "qwen3:8b".to_string(), 0.0)]
        );
    }

    #[test]
    fn llama_server_verdicts_are_stored_at_no_cost() {
        let vault = indexed();
        let verdicts = answers(&vault);
        let llama = fake_llama_server();
        let env = [("KBX_LLAMA_SERVER_HOST", llama.url.as_str())];
        let output = suggest_with(&vault, "llama-server", &env);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(verdict_count(&vault) as usize, verdicts.len());
        assert_eq!(
            stored(&vault),
            [("llama-server".to_string(), "default".to_string(), 0.0)]
        );
    }

    #[test]
    fn the_address_and_model_can_come_from_kbx_toml() {
        let vault = indexed();
        answers(&vault);
        let ollama = fake_ollama(&["gemma3:4b"]);
        let config = vault.root().join(".kbx/kbx.toml");
        let text = fs::read_to_string(&config).unwrap();
        let line = "ollama = { model = \"qwen3:14b\", host = \"http://127.0.0.1:11434\" }";
        assert!(text.contains(line));
        let ours = format!(
            "ollama = {{ model = \"gemma3:4b\", host = \"{}\" }}",
            ollama.url
        );
        fs::write(&config, text.replace(line, &ours)).unwrap();

        let output = suggest_with(&vault, "ollama", &[("OLLAMA_HOST", "")]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("judged by ollama (gemma3:4b)"));
    }

    #[test]
    fn a_stopped_server_asked_for_by_name_fails_loudly_but_keeps_the_candidates() {
        let vault = indexed();
        let output = suggest(&vault, "ollama");
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("ollama can't judge"),
            "{}",
            stderr(&output)
        );
        assert!(!cids(&vault).is_empty());
    }

    #[test]
    fn an_https_host_says_why_it_cant_be_used() {
        let vault = indexed();
        let env = [("OLLAMA_HOST", "https://models.example"), MODEL];
        let output = suggest_with(&vault, "ollama", &env);
        assert!(!output.status.success());
        let err = stderr(&output);
        assert!(err.contains("no TLS"), "{err}");
        assert!(err.contains("ollama can't judge: Unreachable"), "{err}");
    }

    #[test]
    fn auto_falls_through_the_clis_to_ollama_then_llama_server() {
        let vault = indexed();
        logged_out_claude(&vault.path);
        logged_out_codex(&vault.path);
        answers(&vault);
        let ollama = fake_ollama(&["qwen3:8b"]);
        let llama = fake_llama_server();
        let env = [
            ("OLLAMA_HOST", ollama.url.as_str()),
            ("KBX_LLAMA_SERVER_HOST", llama.url.as_str()),
            MODEL,
        ];
        let out = stdout(&suggest_with(&vault, "auto", &env));
        assert!(out.contains("judged by ollama"), "{out}");
        assert!(llama.bodies("/v1/chat/completions").is_empty());

        let env = [
            ("OLLAMA_HOST", CLOSED_PORT),
            ("KBX_LLAMA_SERVER_HOST", llama.url.as_str()),
        ];
        let out = stdout(&suggest_with(&vault, "auto", &env));
        assert!(out.contains("judged by llama-server"), "{out}");
    }

    #[test]
    fn a_proxy_in_the_environment_is_not_used() {
        let vault = indexed();
        answers(&vault);
        let ollama = fake_ollama(&["qwen3:8b"]);
        let proxy = fake_llama_server();
        let env = [
            ("OLLAMA_HOST", ollama.url.as_str()),
            MODEL,
            ("HTTP_PROXY", proxy.url.as_str()),
            ("http_proxy", proxy.url.as_str()),
            ("ALL_PROXY", proxy.url.as_str()),
            ("all_proxy", proxy.url.as_str()),
        ];
        let output = suggest_with(&vault, "ollama", &env);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(ollama.bodies("/api/chat").len(), 1);
    }

    #[test]
    fn a_pinned_scope_goes_to_ollama_and_shows_claude_nothing_not_even_in_its_probe() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        answers(&vault);
        answer(&vault.path, "claude-batch", 0, &claude_result(&[], 0.0, 0));
        // The fake's probe branch records its arguments too, for this test.
        let probe_log = vault.path.dir().join("claude.everything");
        let script = fs::read_to_string(vault.path.program("claude")).unwrap();
        let line = format!(
            "printf '%s\\n' \"$@\" >> {path}\n/bin/cat >> {path}\n",
            path = probe_log.display()
        );
        let script = script.replacen("d=\"${0%/*}\"\n", &format!("d=\"${{0%/*}}\"\n{line}"), 1);
        write_executable(&vault.path.program("claude"), &script);
        pin(&vault, "operations", "ollama");

        let ollama = fake_ollama(&["qwen3:8b"]);
        let env = [("OLLAMA_HOST", ollama.url.as_str()), MODEL];
        let output = suggest_with(&vault, "auto", &env);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            stdout(&output).contains("judged by ollama"),
            "{}",
            stdout(&output)
        );

        let sent = ollama.bodies("/api/chat").concat();
        assert!(
            sent.contains(PINNED_TEXT),
            "ollama should get the pinned notes"
        );
        let everything = fs::read_to_string(&probe_log).unwrap();
        assert!(
            everything.contains("reply with"),
            "the probe should have run"
        );
        assert!(!everything.contains(PINNED_TEXT), "{everything}");
        assert!(!vault.path.dir().join("claude.stdin").exists());
        assert_eq!(calls(&vault.path, "claude"), ["probe"]);
    }

    #[test]
    fn a_pinned_scope_with_ollama_down_is_left_unjudged() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        answers(&vault);
        answer(&vault.path, "claude-batch", 0, &claude_result(&[], 0.0, 0));
        pin(&vault, "operations", "ollama");

        let output = suggest(&vault, "auto");
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("ollama skipped"),
            "{}",
            stderr(&output)
        );
        assert!(!vault.path.dir().join("claude.stdin").exists());
        assert_eq!(verdict_count(&vault), 0);
    }
}

mod review {
    use super::*;

    #[test]
    fn the_queue_shows_what_a_judge_stored_and_a_rejection_of_it_stays_rejected() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let mut verdicts = answers(&vault);
        let (accepted, unsure) = (verdicts[0]["cid"].clone(), verdicts[1]["cid"].clone());
        verdicts[0]["verdict"] = json!("accept");
        verdicts[0]["rel"] = json!("see_also");
        verdicts[1]["verdict"] = json!("unsure");
        let result = claude_result(&verdicts, 0.0, 0);
        answer(&vault.path, "claude-batch", 0, &result);
        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));

        let queue = |vault: &Fixture| -> Vec<Value> {
            serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap()
        };
        let rows = queue(&vault);
        assert_eq!(rows.len(), verdicts.len());
        assert_eq!(rows.last().unwrap()["cid"], unsure);
        assert_eq!(rows.last().unwrap()["verdict"], json!("unsure"));
        let row = rows.iter().find(|row| row["cid"] == accepted).unwrap();
        assert_eq!(row["rel"], json!("see_also"));
        assert_eq!(row["verdict"], json!("accept"));
        assert_eq!(row["rationale"], json!("Both cover restarts."));

        vault.ok(&["reject", accepted.as_str().unwrap()]);
        let output = suggest(&vault, "claude-cli");
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(cids(&vault).contains(&accepted.as_str().unwrap().to_string()));
        assert!(queue(&vault).iter().all(|row| row["cid"] != accepted));
    }
}

mod cost_controls {
    use super::*;

    /// Every generator that needs no qmd, which comes to more than one batch.
    const WIDE: [&str; 2] = ["--generators", "schema_gap,common_neighbors,alias_cooccur"];

    fn wide(vault: &Fixture, extra: &[&str]) -> String {
        let mut args = vec!["suggest", "--judge", "claude-cli"];
        args.extend(WIDE);
        args.extend(extra);
        vault.ok(&args)
    }

    fn total_cost(vault: &Fixture) -> f64 {
        database(vault)
            .query_row("SELECT SUM(cost_usd) FROM verdicts", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn an_unchanged_rerun_sends_no_batch() {
        let vault = indexed();
        answering_claude(&vault.path, 0.01);
        wide(&vault, &[]);
        let first = batches(&vault.path);
        assert!(first > 0);

        let out = wide(&vault, &[]);
        assert_eq!(batches(&vault.path), first);
        assert!(out.contains(" already judged"), "{out}");
    }

    #[test]
    fn verdicts_from_another_prompt_are_asked_again() {
        let vault = indexed();
        answering_claude(&vault.path, 0.01);
        wide(&vault, &[]);
        let first = batches(&vault.path);

        // What a build with another `prompts/judge.system.md` would have left.
        database(&vault)
            .execute("UPDATE verdicts SET prompt_hash = 'another prompt'", [])
            .unwrap();
        wide(&vault, &[]);
        assert_eq!(batches(&vault.path), first * 2);
    }

    #[test]
    fn editing_the_ontology_asks_again() {
        let vault = indexed();
        answering_claude(&vault.path, 0.01);
        wide(&vault, &[]);
        let first = batches(&vault.path);

        let path = vault.root().join(".kbx/ontology.yaml");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{text}\n# edited\n")).unwrap();
        vault.ok(&["index"]);
        wide(&vault, &[]);
        assert_eq!(batches(&vault.path), first * 2);
    }

    #[test]
    fn the_budget_halts_the_run_and_reports_the_spend() {
        let vault = indexed();
        answering_claude(&vault.path, 0.06);
        let out = wide(&vault, &["--budget-usd", "0.10", "--batch", "10"]);

        assert_eq!(batches(&vault.path), 1);
        assert!(total_cost(&vault) <= 0.10);
        assert!(
            out.contains("budget reached: spent $0.0608 of $0.1000"),
            "{out}"
        );
        assert_eq!(verdict_count(&vault), 10);
    }

    #[test]
    fn one_budget_covers_every_backend_in_the_run() {
        let vault = indexed();
        logged_in_codex(&vault.path);
        // Over the limit in one call, which leaves nothing for codex.
        answering_claude(&vault.path, 0.2);
        pin(&vault, "operations", "codex-cli");
        let mut args = vec!["suggest", "--judge", "auto", "--budget-usd", "0.10"];
        args.extend(WIDE);
        let out = vault.ok(&args);

        assert_eq!(batches(&vault.path), 1);
        assert!(out.contains("judged by codex-cli"), "{out}");
        let steps = calls(&vault.path, "codex");
        assert!(!steps.iter().any(|step| step == "batch"), "{steps:?}");
    }

    #[test]
    fn a_budget_too_small_for_the_first_batch_spends_nothing() {
        let vault = indexed();
        answering_claude(&vault.path, 0.06);
        let out = wide(&vault, &["--budget-usd", "0"]);
        assert!(calls(&vault.path, "claude").is_empty(), "not even a probe");
        assert!(out.contains("budget reached: spent $0.0000"), "{out}");
    }

    #[test]
    fn a_probe_is_part_of_the_spend_until_it_is_cached() {
        let vault = indexed();
        answering_claude(&vault.path, 0.0);
        // `PROBE_OK` reports $0.0008, and the batches here report nothing.
        let out = wide(&vault, &["--budget-usd", "1"]);
        assert!(out.contains("$0.0008"), "{out}");
        let out = wide(&vault, &["--budget-usd", "0"]);
        assert!(!out.contains("$0.0008"), "{out}");
    }

    #[test]
    fn a_budget_must_be_an_amount() {
        let vault = indexed();
        for bad in ["NaN", "-1", "inf", "lots"] {
            let output = vault.kbx(&["suggest", "--judge", "none", "--budget-usd", bad]);
            assert!(!output.status.success(), "{bad}");
        }
    }

    #[test]
    fn the_batch_size_comes_from_the_flag_then_the_config_and_stays_in_range() {
        let vault = indexed();
        answering_claude(&vault.path, 0.0);
        let config = vault.root().join(".kbx/kbx.toml");

        for size in ["9", "26"] {
            let output = vault.kbx(&["suggest", "--judge", "none", "--batch", size]);
            assert!(!output.status.success(), "{size}");
            assert!(stderr(&output).contains("10 to 25"), "{}", stderr(&output));
        }

        let text = fs::read_to_string(&config).unwrap();
        fs::write(&config, text.replace("batch = 20", "batch = 3")).unwrap();
        assert!(!vault.kbx(&["suggest", "--judge", "none"]).status.success());
        vault.ok(&["suggest", "--judge", "none", "--batch", "10"]);

        let text = fs::read_to_string(&config).unwrap();
        fs::write(&config, text.replace("batch = 3", "batch = 10")).unwrap();
        let out = vault.ok(&["suggest", "--judge", "none", "--dry-run"]);
        assert!(out.contains("batch 1: 10 candidates"), "{out}");
    }

    #[test]
    fn a_dry_run_leaves_out_what_a_real_run_would_skip() {
        let vault = indexed();
        answering_claude(&vault.path, 0.0);
        let mut args = vec!["suggest", "--judge", "claude-cli", "--dry-run"];
        args.extend(NARROW);
        let before = vault.ok(&args);
        assert!(before.contains("batch 1:"), "{before}");

        suggest(&vault, "claude-cli");
        let sent = batches(&vault.path);
        let after = vault.ok(&args);
        assert!(!after.contains("batch 1:"), "{after}");
        assert_eq!(batches(&vault.path), sent);
    }

    #[test]
    fn codex_spend_is_its_tokens_at_the_configured_rates() {
        let vault = indexed();
        logged_in_codex(&vault.path);
        let verdicts = answers(&vault);
        // 9,000 input, 4,000 of them cached, and 300 output.
        answer(
            &vault.path,
            "codex-batch",
            0,
            &codex_events(&verdicts, 4000),
        );
        let config = vault.root().join(".kbx/kbx.toml");
        let text = fs::read_to_string(&config).unwrap();
        let text = text.replace(
            "codex  = { model = \"gpt-6-luna\" }",
            "codex  = { model = \"gpt-6-luna\", rates = { input = 2.0, cached_input = 0.5, output = 10.0 } }",
        );
        fs::write(&config, text).unwrap();

        suggest(&vault, "codex-cli");
        let expected = (5000.0 * 2.0 + 4000.0 * 0.5 + 300.0 * 10.0) / 1_000_000.0;
        assert!((total_cost(&vault) - expected).abs() < 1e-9);
    }

    #[test]
    fn a_failed_batch_is_retried_then_marked_error_and_the_queue_never_shows_it() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let verdicts = answers(&vault);
        answer(&vault.path, "claude-batch", 1, "");

        let out = stdout(&suggest(&vault, "claude-cli"));
        assert!(out.contains(&format!("{} error", verdicts.len())), "{out}");
        let sent = calls(&vault.path, "claude");
        assert_eq!(sent.iter().filter(|step| *step == "batch").count(), 3);

        let rows: Value = serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap();
        for row in rows.as_array().unwrap() {
            assert_eq!(row["verdict"], Value::Null);
        }
    }

    #[test]
    fn no_confidence_accepts_anything() {
        let vault = indexed();
        logged_in_claude(&vault.path);
        let mut verdicts = answers(&vault);
        for verdict in &mut verdicts {
            verdict["verdict"] = json!("accept");
            verdict["rel"] = json!("see_also");
            verdict["confidence"] = json!(1.0);
        }
        answer(
            &vault.path,
            "claude-batch",
            0,
            &claude_result(&verdicts, 0.0, 0),
        );
        suggest(&vault, "claude-cli");

        let db = database(&vault);
        let reviews: i64 = db
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(reviews, 0);
        let rows: Value = serde_json::from_str(&vault.ok(&["review", "--json"])).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), verdicts.len());
    }
}

/// A word that's in the private note and nowhere else.
const SECRET: &str = "xylophonewatermelon";

/// The fixture vault plus a note in `folder` that `schema_gap` pairs with
/// [`NOTE`]: a procedure needs something to depend on, this is a procedure, and
/// it's in the same scope.
fn vault_with_private_note(folder: &str) -> Fixture {
    let vault = Fixture::new();
    let private = id(90);
    vault.write(
        &format!("operations/{folder}/private.md"),
        &format!("---\nid: {private}\ntype: Procedure\ntitle: {SECRET}\n---\n\n{SECRET}\n"),
    );
    logged_in_claude(&vault.path);
    vault
}

fn set_exclude(vault: &Fixture, exclude: &str) {
    let file = vault.root().join(".kbx/kbx.toml");
    let text = fs::read_to_string(&file).unwrap();
    let kept = text.lines().filter(|line| !line.starts_with("exclude ="));
    let kept = kept.collect::<Vec<_>>().join("\n");
    fs::write(&file, format!("exclude = {exclude}\n{kept}\n")).unwrap();
}

/// Everything the fake `claude` was handed, in every call.
fn seen_by_judge(path: &FakePath) -> String {
    fs::read_to_string(path.dir().join("claude.seen")).unwrap_or_default()
}

fn judge_with_claude(vault: &Fixture) -> std::process::Output {
    suggest(vault, "claude-cli")
}

/// Without this, the tests below would pass on a vault where no pair ever
/// reached the judge at all.
#[test]
fn with_nothing_excluded_the_private_note_does_reach_the_judge() {
    let vault = vault_with_private_note("private");
    set_exclude(&vault, "[]");
    vault.ok(&["index"]);

    judge_with_claude(&vault);

    let seen = seen_by_judge(&vault.path);
    assert!(seen.contains(SECRET), "{seen}");
}

#[test]
fn a_note_in_a_default_excluded_folder_never_reaches_the_judge() {
    let vault = vault_with_private_note("credentials");
    vault.ok(&["index"]);

    judge_with_claude(&vault);

    let seen = seen_by_judge(&vault.path);
    assert!(seen.contains(NOTE) && !seen.contains(SECRET), "{seen}");
}

#[test]
fn a_note_in_a_gitignored_folder_never_reaches_the_judge() {
    let vault = vault_with_private_note("private");
    vault.write(".gitignore", "private/\n");
    vault.ok(&["index"]);

    judge_with_claude(&vault);

    let seen = seen_by_judge(&vault.path);
    assert!(seen.contains(NOTE) && !seen.contains(SECRET), "{seen}");
}

/// A note indexed before its folder was excluded is still in the graph, and
/// nothing says `kbx index` ran since.
#[test]
fn a_note_excluded_after_indexing_never_reaches_the_judge() {
    let vault = vault_with_private_note("private");
    set_exclude(&vault, "[]");
    vault.ok(&["index"]);
    set_exclude(&vault, r#"["private"]"#);

    let output = judge_with_claude(&vault);

    assert!(output.status.success(), "{}", stderr(&output));
    let seen = seen_by_judge(&vault.path);
    assert!(seen.contains(NOTE) && !seen.contains(SECRET), "{seen}");
    let graph = Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap();
    let left: i64 = graph
        .query_row(
            "SELECT count(*) FROM files WHERE path LIKE '%/private/%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
}

/// `kbx bakeoff` reads the graph read-only, so nothing can be dropped from it:
/// the views themselves have to refuse.
#[test]
fn a_labelled_pair_with_an_excluded_note_never_reaches_the_judge() {
    let vault = vault_with_private_note("private");
    set_exclude(&vault, "[]");
    vault.ok(&["index"]);
    set_exclude(&vault, r#"["private"]"#);
    let line = json!({"src": NOTE, "dst": id(90), "rel": "none", "decision": "reject"});
    vault.write("labels.jsonl", &format!("{line}\n"));
    vault.write(
        "report.md",
        "<!-- bakeoff:begin -->\n<!-- bakeoff:end -->\n",
    );

    let output = vault.kbx(&[
        "bakeoff",
        "--labels",
        "labels.jsonl",
        "--models",
        "claude-cli",
        "--doc",
        "report.md",
    ]);

    assert!(
        stderr(&output).contains("1 labelled pairs have an excluded note"),
        "{output:?}"
    );
    assert!(!seen_by_judge(&vault.path).contains(SECRET), "{output:?}");
}
