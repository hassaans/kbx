//! Drives `kbx accept`, `reject`, `defer` and `unreject` after a real
//! `kbx suggest` over the fixture vault.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{Fixture, stderr, stdout};
use rusqlite::Connection;

/// The first candidate `kbx suggest` stores for the vault, best score first.
struct Pair {
    cid: String,
    src: String,
    dst: String,
}

fn suggested() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    suggest(&vault);
    vault
}

fn suggest(vault: &Fixture) {
    vault.ok(&[
        "suggest",
        "--judge",
        "none",
        "--generators",
        "common_neighbors",
    ]);
}

fn db(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

fn pairs(vault: &Fixture) -> Vec<Pair> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT cid, src, dst FROM candidates ORDER BY score DESC, cid")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok(Pair {
                cid: row.get(0)?,
                src: row.get(1)?,
                dst: row.get(2)?,
            })
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn path_of(vault: &Fixture, id: &str) -> PathBuf {
    let path: String = db(vault)
        .query_row("SELECT path FROM files WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .unwrap();
    vault.root().join(path)
}

/// The file with the frontmatter taken out.
fn around_frontmatter(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let text = std::str::from_utf8(bytes).unwrap();
    let frontmatter = kbx_core::split_frontmatter(text).unwrap().unwrap();
    let end = frontmatter.yaml_start + frontmatter.yaml.len();
    (
        bytes[..frontmatter.yaml_start].to_vec(),
        bytes[end..].to_vec(),
    )
}

#[test]
fn accept_writes_both_notes_byte_for_byte_outside_the_frontmatter_and_reindexes() {
    let vault = suggested();
    let pair = &pairs(&vault)[0];
    let (src, dst) = (path_of(&vault, &pair.src), path_of(&vault, &pair.dst));
    // A CRLF body on one side, with a line that looks like frontmatter.
    let crlf = fs::read_to_string(&dst).unwrap().replace('\n', "\r\n") + "---\r\nid: x\r\n---\r\n";
    fs::write(&dst, crlf).unwrap();
    vault.ok(&["index"]);
    suggest(&vault);
    let (before_src, before_dst) = (fs::read(&src).unwrap(), fs::read(&dst).unwrap());

    let out = vault.ok(&["accept", &pair.cid, "--rel", "see_also", "--reason", "good"]);
    assert!(out.contains(&format!("accepted {}", pair.cid)), "{out}");

    let (after_src, after_dst) = (fs::read(&src).unwrap(), fs::read(&dst).unwrap());
    assert_eq!(
        around_frontmatter(&before_src),
        around_frontmatter(&after_src)
    );
    assert_eq!(
        around_frontmatter(&before_dst),
        around_frontmatter(&after_dst)
    );
    let (src_text, dst_text) = (
        String::from_utf8(after_src).unwrap(),
        String::from_utf8(after_dst).unwrap(),
    );
    assert!(
        src_text.contains(&format!("[[{}]]", pair.dst)),
        "{src_text}"
    );
    assert!(
        dst_text.contains(&format!("[[{}]]", pair.src)),
        "{dst_text}"
    );
    assert_eq!(
        dst_text.replace("\r\n", "").matches('\n').count(),
        0,
        "{dst_text:?}"
    );

    // The index followed: the edge is there and the pair isn't proposed again.
    let neighbors = vault.ok(&["graph", "neighbors", &pair.src]);
    assert!(neighbors.contains(&pair.dst), "{neighbors}");
    suggest(&vault);
    assert!(pairs(&vault).iter().all(|p| p.cid != pair.cid));
    let reason: String = db(&vault)
        .query_row(
            "SELECT reason FROM reviews WHERE decision = 'accept'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reason, "good");
}

#[test]
fn one_accept_takes_several_candidates_sharing_a_note() {
    let vault = suggested();
    let all = pairs(&vault);
    let shared = all
        .iter()
        .find_map(|a| {
            let touches = |b: &&Pair| {
                b.cid != a.cid
                    && [&b.src, &b.dst]
                        .iter()
                        .any(|id| **id == a.src || **id == a.dst)
            };
            all.iter().find(touches).map(|b| (a, b))
        })
        .expect("two candidates sharing a note");
    vault.ok(&["accept", &shared.0.cid, &shared.1.cid, "--rel", "see_also"]);
}

#[test]
fn an_edit_after_suggest_refuses_acceptance_even_after_kbx_index() {
    let vault = suggested();
    let pair = &pairs(&vault)[0];
    let (src, dst) = (path_of(&vault, &pair.src), path_of(&vault, &pair.dst));
    let edited = fs::read_to_string(&src).unwrap() + "\nAn edit.\n";
    fs::write(&src, &edited).unwrap();
    let target = fs::read(&dst).unwrap();

    for reindex in [false, true] {
        if reindex {
            vault.ok(&["index"]);
        }
        let output = vault.kbx(&["accept", &pair.cid, "--rel", "see_also"]);
        let log = stderr(&output);
        assert!(!output.status.success());
        assert!(
            log.contains("changed since this candidate was generated"),
            "{log}"
        );
        assert!(log.contains("kbx suggest"), "{log}");
        assert_eq!(fs::read_to_string(&src).unwrap(), edited);
        assert_eq!(fs::read(&dst).unwrap(), target);
    }
}

#[test]
fn a_missing_target_fails_with_target_unavailable() {
    let vault = suggested();
    let pair = &pairs(&vault)[0];
    let (src, dst) = (path_of(&vault, &pair.src), path_of(&vault, &pair.dst));
    let source = fs::read(&src).unwrap();
    fs::remove_file(&dst).unwrap();

    let output = vault.kbx(&["accept", &pair.cid, "--rel", "see_also"]);
    let log = stderr(&output);
    assert!(!output.status.success());
    assert!(log.contains("target.unavailable"), "{log}");
    assert_eq!(fs::read(&src).unwrap(), source);
    let reviews: i64 = db(&vault)
        .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
        .unwrap();
    assert_eq!(reviews, 0);
}

#[test]
fn accept_without_a_relation_says_how_to_name_one() {
    let vault = suggested();
    let pair = &pairs(&vault)[0];
    let output = vault.kbx(&["accept", &pair.cid]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--rel"), "{}", stderr(&output));
}

#[test]
fn one_refusal_does_not_stop_the_rest_but_fails_the_command() {
    let vault = suggested();
    let pair = &pairs(&vault)[0];
    let output = vault.kbx(&["reject", "nope", &pair.cid]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("no candidate nope"));
    assert!(stdout(&output).contains(&format!("rejected {}", pair.cid)));
}

#[test]
fn three_rejections_suppress_a_pair_on_the_next_suggest_and_unreject_restores_it() {
    let vault = suggested();
    let cid = pairs(&vault)[0].cid.clone();
    let proposed = |vault: &Fixture| pairs(vault).iter().any(|p| p.cid == cid);

    for strike in 1..=2 {
        let out = vault.ok(&["reject", &cid, "--reason", "not related"]);
        assert!(out.contains(&format!("strike {strike} of 3")), "{out}");
        assert!(!proposed(&vault), "a rejection leaves the queue");
        suggest(&vault);
        assert!(proposed(&vault), "{strike} strikes don't suppress");
    }
    let out = vault.ok(&["reject", &cid]);
    assert!(out.contains("suppressed"), "{out}");
    suggest(&vault);
    assert!(!proposed(&vault));
    assert!(vault.ok(&["unreject", "--list"]).contains(&cid));

    let out = vault.ok(&["unreject", &cid]);
    assert!(out.contains("3 strikes cleared"), "{out}");
    assert!(!vault.ok(&["unreject", "--list"]).contains(&cid));
    suggest(&vault);
    assert!(proposed(&vault));
}

#[test]
fn defer_leaves_the_candidate_in_the_queue() {
    let vault = suggested();
    let cid = pairs(&vault)[0].cid.clone();
    vault.ok(&["defer", &cid, "--reason", "later"]);
    assert!(pairs(&vault).iter().any(|p| p.cid == cid));
    let decision: String = db(&vault)
        .query_row("SELECT decision FROM reviews", [], |row| row.get(0))
        .unwrap();
    assert_eq!(decision, "defer");
}

#[test]
fn unreject_needs_a_cid_or_list() {
    let vault = suggested();
    assert!(!vault.kbx(&["unreject"]).status.success());
    assert!(!vault.kbx(&["unreject", "nope"]).status.success());
}

#[test]
fn deciding_before_the_first_index_says_to_run_it() {
    let vault = Fixture::new();
    let output = vault.kbx(&["reject", "abc"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("kbx index"), "{}", stderr(&output));
}
