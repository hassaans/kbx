//! Drives `kbx suggest --stage`, `kbx sync-stage` and the `kbx.reviewed` lock
//! over copies of the fixture vault.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use common::{Fixture, id, stderr, stdout};
use rusqlite::Connection;
use serde_json::Value;
use serde_yaml::Value as Yaml;

const SUGGEST: [&str; 5] = [
    "suggest",
    "--judge",
    "none",
    "--generators",
    "common_neighbors",
];

fn indexed() -> Fixture {
    let vault = Fixture::new();
    vault.ok(&["index"]);
    vault
}

fn staged() -> Fixture {
    let vault = indexed();
    stage(&vault);
    vault
}

fn stage(vault: &Fixture) {
    let mut args = SUGGEST.to_vec();
    args.push("--stage");
    vault.ok(&args);
}

fn db(vault: &Fixture) -> Connection {
    Connection::open(vault.root().join(".kbx/graph.sqlite")).unwrap()
}

fn queue(vault: &Fixture) -> Vec<Value> {
    let text = vault.ok(&["review", "--json"]);
    match serde_json::from_str(&text).unwrap() {
        Value::Array(items) => items,
        other => panic!("not a queue: {other}"),
    }
}

fn path_of(vault: &Fixture, id: &str) -> PathBuf {
    let path: String = db(vault)
        .query_row("SELECT path FROM files WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .unwrap();
    vault.root().join(path)
}

fn read(vault: &Fixture, id: &str) -> String {
    fs::read_to_string(path_of(vault, id)).unwrap()
}

/// The `kbx.proposed` entries of a note, as written.
fn proposed(text: &str) -> Vec<Yaml> {
    let note = kbx_core::parse_note("n.md", text).unwrap();
    let list = note.kbx.as_ref().and_then(|kbx| kbx.get("proposed"));
    list.and_then(Yaml::as_sequence)
        .cloned()
        .unwrap_or_default()
}

fn entry_id(entry: &Yaml) -> String {
    entry.get("id").and_then(Yaml::as_str).unwrap().to_string()
}

/// One staged candidate: the note holding the line, and where it points.
struct Line {
    cid: String,
    note: String,
    target: String,
}

fn lines(vault: &Fixture) -> Vec<Line> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT cid, note, target FROM staged ORDER BY cid")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok(Line {
                cid: row.get(0)?,
                note: row.get(1)?,
                target: row.get(2)?,
            })
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// Takes a line out of the frontmatter, as an editor would.
fn remove_line(vault: &Fixture, line: &Line) {
    let path = path_of(vault, &line.note);
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text
        .split_inclusive('\n')
        .filter(|l| !l.contains(&line.cid))
        .collect();
    assert_ne!(kept.concat(), text, "the line should be there");
    fs::write(path, kept.concat()).unwrap();
}

/// Moves a proposal into a relation key, as a person in an editor would.
fn move_line(vault: &Fixture, line: &Line, key: &str) {
    remove_line(vault, line);
    let path = path_of(vault, &line.note);
    let text = fs::read_to_string(&path).unwrap();
    let new = format!("---\n{key}: [\"[[{}]]\"]\n", line.target);
    fs::write(path, text.replacen("---\n", &new, 1)).unwrap();
}

fn around_frontmatter(text: &str) -> (String, String) {
    let frontmatter = kbx_core::split_frontmatter(text).unwrap().unwrap();
    let end = frontmatter.yaml_start + frontmatter.yaml.len();
    (text[..frontmatter.yaml_start].into(), text[end..].into())
}

fn reviews(vault: &Fixture) -> Vec<(String, String, i64)> {
    let db = db(vault);
    let mut statement = db
        .prepare("SELECT rel, decision, strikes FROM reviews ORDER BY decided_at, rel")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn candidate_touches(vault: &Fixture, note: &str) -> bool {
    let count: i64 = db(vault)
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE src = ?1 OR dst = ?1",
            [note],
            |row| row.get(0),
        )
        .unwrap();
    count > 0
}

/// Every row the compiler writes, as text, leaving out what is about the file
/// rather than the graph. `lines` says whether to keep line numbers, which any
/// edit above them moves.
fn compiled(vault: &Fixture, lines: bool) -> Vec<String> {
    let at = |columns: &str| {
        if lines {
            format!(", {columns}")
        } else {
            String::new()
        }
    };
    let queries = [
        "SELECT path, id, type, title, scope FROM files ORDER BY path".to_string(),
        "SELECT alias, id FROM aliases ORDER BY alias, id".to_string(),
        format!(
            "SELECT key, id, heading_path, block_id, text_hash, tokens{} FROM sections ORDER BY key",
            at("start_line, end_line")
        ),
        format!(
            "SELECT id, block_id, section_key{} FROM blocks ORDER BY id, block_id",
            at("start_line, end_line")
        ),
        format!(
            "SELECT src, key, target, fragment, origin, src_section, dst{} FROM links
             ORDER BY src, key, target, origin, src_section",
            at("line")
        ),
        format!(
            "SELECT src, dst, rel, inverse_of, weight, origin, src_section{} FROM edges
             ORDER BY src, dst, rel, origin",
            at("line")
        ),
    ];
    let db = db(vault);
    let mut rows = Vec::new();
    for query in queries {
        let mut statement = db.prepare(&query).unwrap();
        let width = statement.column_count();
        let mut found = statement.query([]).unwrap();
        while let Some(row) = found.next().unwrap() {
            let cells: Vec<String> = (0..width)
                .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                .collect();
            rows.push(cells.join("|"));
        }
    }
    rows
}

/// Swaps each line of the `kbx:` block for a comment, so that the note says
/// the same thing with its line numbers where staging put them.
fn blank_kbx(text: &str) -> String {
    let mut inside = false;
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("kbx:") {
            inside = true;
        } else if !line.starts_with(' ') {
            inside = false;
        }
        out.push_str(if inside { "# padding\n" } else { line });
    }
    out
}

fn notes(vault: &Fixture) -> Vec<PathBuf> {
    let db = db(vault);
    let mut statement = db.prepare("SELECT path FROM files").unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(|path| vault.root().join(path.unwrap())).collect()
}

#[test]
fn stage_adds_an_entry_per_candidate_and_review_shows_the_same_queue() {
    let plain = indexed();
    plain.ok(&SUGGEST);
    let vault = staged();

    let (before, after) = (queue(&plain), queue(&vault));
    assert!(!after.is_empty());
    assert_eq!(before, after, "staging must not change the queue");

    let mut found = BTreeSet::new();
    for line in lines(&vault) {
        let entries = proposed(&read(&vault, &line.note));
        let entry = entries
            .iter()
            .find(|entry| entry_id(entry) == line.cid)
            .expect("an entry in the note");
        assert_eq!(
            entry.get("to").and_then(Yaml::as_str),
            Some(format!("[[{}]]", line.target).as_str())
        );
        assert_eq!(entry.get("rel").and_then(Yaml::as_str), Some("none"));
        let by = entry.get("by").and_then(Yaml::as_sequence).unwrap();
        assert_eq!(by[0].as_str(), Some("common_neighbors"));
        assert!(entry.get("score").and_then(Yaml::as_f64).unwrap() > 0.0);
        found.insert(line.cid);
    }
    let shown: BTreeSet<String> = after
        .iter()
        .map(|item| item["cid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(found, shown, "every queue item is staged, and nothing else");
}

#[test]
fn staging_leaves_everything_but_the_tool_namespaces_alone() {
    let plain = indexed();
    plain.ok(&SUGGEST);
    let vault = staged();
    for line in lines(&vault) {
        let (old, new) = (read(&plain, &line.note), read(&vault, &line.note));
        assert_eq!(around_frontmatter(&old), around_frontmatter(&new));
        let (old, new) = (
            kbx_core::parse_note("n.md", &old).unwrap(),
            kbx_core::parse_note("n.md", &new).unwrap(),
        );
        assert_eq!(old.relations, new.relations);
        assert_eq!(old.properties, new.properties);
        assert_eq!(
            (&old.title, &old.aliases, &old.tags),
            (&new.title, &new.aliases, &new.tags)
        );
    }
}

#[test]
fn staging_keeps_an_entry_kbx_has_no_record_of() {
    let vault = indexed();
    let note = id(2);
    let unknown = "7f3a9c";
    let before = read(&vault, &note);
    assert!(before.contains(unknown), "the fixture's own entry");
    let without_lock = before.replace("  reviewed: 2026-11-03\n", "");
    fs::write(path_of(&vault, &note), without_lock).unwrap();
    vault.ok(&["index"]);
    stage(&vault);
    assert!(lines(&vault).iter().any(|line| line.note == note));
    let entries = proposed(&read(&vault, &note));
    assert!(entries.iter().any(|entry| entry_id(entry) == unknown));
}

#[test]
fn staging_again_adds_nothing() {
    let vault = staged();
    let before: Vec<String> = notes(&vault)
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    stage(&vault);
    let after: Vec<String> = notes(&vault)
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn a_dry_run_stages_nothing() {
    let vault = indexed();
    let before: Vec<String> = notes(&vault)
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    let mut args = SUGGEST.to_vec();
    args.extend(["--stage", "--dry-run"]);
    let printed = vault.ok(&args);
    assert!(printed.contains("would stage"), "{printed}");
    let after: Vec<String> = notes(&vault)
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn the_index_is_current_after_staging() {
    let vault = staged();
    let printed = vault.ok(&["index"]);
    assert!(printed.starts_with("indexed 0,"), "{printed}");
}

#[test]
fn a_note_edited_since_the_index_is_not_staged_into() {
    let vault = indexed();
    vault.ok(&SUGGEST);
    let cid = queue(&vault)[0]["src"].as_str().unwrap().to_string();
    let path = path_of(&vault, &cid);
    let edited = format!("{}\nEdited.\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, &edited).unwrap();

    let mut args = SUGGEST.to_vec();
    args.push("--stage");
    let output = vault.kbx(&args);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("changed since"), "{output:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn a_staged_candidate_can_be_accepted_and_its_line_goes() {
    let vault = staged();
    let line = &lines(&vault)[0];
    vault.ok(&["accept", &line.cid, "--rel", "see_also"]);

    let text = read(&vault, &line.note);
    assert!(!text.contains(&line.cid));
    assert!(text.contains(&format!("[[{}]]", line.target)));
    assert!(!lines(&vault).iter().any(|l| l.cid == line.cid));
    assert!(vault.ok(&["sync-stage"]).contains("0 rejected"));
}

#[test]
fn accepting_a_line_keeps_the_others_on_that_note_acceptable() {
    let vault = staged();
    let all = lines(&vault);
    let (first, second) = all
        .iter()
        .enumerate()
        .find_map(|(at, a)| {
            let same = |b: &&Line| b.note == a.note;
            all[at + 1..].iter().find(same).map(|b| (a, b))
        })
        .expect("a note with two lines");
    vault.ok(&["accept", &first.cid, "--rel", "see_also"]);
    vault.ok(&["accept", &second.cid, "--rel", "see_also"]);
}

#[test]
fn rejecting_a_staged_candidate_takes_its_line_out_without_a_second_strike() {
    let vault = staged();
    let line = &lines(&vault)[0];
    vault.ok(&["reject", &line.cid]);
    assert!(!read(&vault, &line.note).contains(&line.cid));

    vault.ok(&["sync-stage"]);
    assert_eq!(reviews(&vault), [("none".into(), "reject".into(), 1)]);
}

#[test]
fn removing_a_line_is_a_rejection_with_a_strike() {
    let vault = staged();
    let line = &lines(&vault)[0];
    remove_line(&vault, line);

    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("1 rejected"), "{printed}");
    assert!(printed.contains("strike 1 of 3"), "{printed}");
    assert_eq!(reviews(&vault), [("none".into(), "reject".into(), 1)]);
    let items = queue(&vault);
    assert!(!items.iter().any(|item| item["cid"] == line.cid.as_str()));
}

#[test]
fn syncing_again_records_nothing_more() {
    let vault = staged();
    let line = &lines(&vault)[0];
    remove_line(&vault, line);
    vault.ok(&["sync-stage"]);
    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("0 rejected"), "{printed}");
    assert_eq!(reviews(&vault).len(), 1);
    assert_eq!(reviews(&vault)[0].2, 1);
}

#[test]
fn staging_does_not_put_back_a_line_removed_before_the_sync() {
    let vault = staged();
    let line = &lines(&vault)[0];
    remove_line(&vault, line);
    vault.ok(&["index"]);
    stage(&vault);
    assert!(!read(&vault, &line.note).contains(&line.cid));
    assert!(vault.ok(&["sync-stage"]).contains("1 rejected"));
}

#[test]
fn three_removals_suppress_the_pair() {
    let vault = staged();
    let cid = lines(&vault)[0].cid.clone();
    for strike in 1..=3 {
        let all = lines(&vault);
        let line = all.iter().find(|line| line.cid == cid).unwrap();
        remove_line(&vault, line);
        let printed = vault.ok(&["sync-stage"]);
        assert!(
            printed.contains(&format!("strike {strike} of 3")),
            "{printed}"
        );
        stage(&vault);
    }
    assert!(!lines(&vault).iter().any(|line| line.cid == cid));
    assert!(vault.ok(&["unreject", "--list"]).contains(&cid));
}

#[test]
fn removing_a_line_leaves_the_others_on_that_note_acceptable() {
    let vault = staged();
    let all = lines(&vault);
    let (first, second) = all
        .iter()
        .enumerate()
        .find_map(|(at, a)| {
            let same = |b: &&Line| b.note == a.note;
            all[at + 1..].iter().find(same).map(|b| (a, b))
        })
        .expect("a note with two lines");
    remove_line(&vault, first);
    vault.ok(&["sync-stage"]);
    vault.ok(&["accept", &second.cid, "--rel", "see_also"]);
}

#[test]
fn a_real_edit_alongside_a_removed_line_still_trips_the_hash_guard() {
    let vault = staged();
    let all = lines(&vault);
    let (first, second) = all
        .iter()
        .enumerate()
        .find_map(|(at, a)| {
            let same = |b: &&Line| b.note == a.note;
            all[at + 1..].iter().find(same).map(|b| (a, b))
        })
        .expect("a note with two lines");
    remove_line(&vault, first);
    let path = path_of(&vault, &first.note);
    let edited = format!("{}\nEdited.\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, edited).unwrap();
    vault.ok(&["sync-stage"]);
    let output = vault.kbx(&["accept", &second.cid, "--rel", "see_also"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("changed since"), "{output:?}");
}

#[test]
fn a_line_moved_into_a_relation_key_is_an_acceptance() {
    let vault = staged();
    let line = &lines(&vault)[0];
    let target_before = read(&vault, &line.target);
    move_line(&vault, line, "see_also");
    let holder_before = read(&vault, &line.note);

    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("1 accepted"), "{printed}");
    assert_eq!(reviews(&vault), [("see_also".into(), "accept".into(), 0)]);

    let holder = read(&vault, &line.note);
    assert_eq!(
        around_frontmatter(&holder_before),
        around_frontmatter(&holder)
    );
    let (old, new) = (
        kbx_core::parse_note("n.md", &holder_before).unwrap(),
        kbx_core::parse_note("n.md", &holder).unwrap(),
    );
    assert_eq!(old.relations, new.relations, "the person's keys stay");
    assert_eq!(old.properties, new.properties);

    let target = read(&vault, &line.target);
    assert_eq!(
        around_frontmatter(&target_before),
        around_frontmatter(&target)
    );
    let parsed = kbx_core::parse_note("n.md", &target).unwrap();
    let inverse = parsed.relation("see_also").expect("the inverse");
    assert!(inverse.links.iter().any(|link| link.target == line.note));
    assert!(!candidate_touches_pair(&vault, line));
    assert!(!lines(&vault).iter().any(|l| l.cid == line.cid));
    // The index followed what was written.
    assert!(vault.ok(&["index"]).starts_with("indexed 0,"));
}

fn candidate_touches_pair(vault: &Fixture, line: &Line) -> bool {
    let count: i64 = db(vault)
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE cid = ?",
            [&line.cid],
            |row| row.get(0),
        )
        .unwrap();
    count > 0
}

#[test]
fn a_moved_line_survives_a_suggest_before_the_sync() {
    let vault = staged();
    let line = &lines(&vault)[0];
    move_line(&vault, line, "see_also");
    vault.ok(&["index"]);
    vault.ok(&SUGGEST);
    assert!(
        !candidate_touches_pair(&vault, line),
        "the pair is linked now"
    );

    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("1 accepted"), "{printed}");
    assert!(read(&vault, &line.target).contains(&line.note));
}

#[test]
fn a_line_moved_under_an_inverse_key_reads_the_way_it_was_written() {
    let vault = staged();
    let all = lines(&vault);
    let line = all
        .iter()
        .find(|l| class(&vault, &l.target) == "Procedure" && class(&vault, &l.note) != "Decision")
        .expect("a line pointing at a procedure");
    move_line(&vault, line, "required_by");
    assert!(vault.ok(&["sync-stage"]).contains("1 accepted"));

    let target = kbx_core::parse_note("n.md", &read(&vault, &line.target)).unwrap();
    let forward = target.relation("depends_on").expect("the relation");
    assert!(forward.links.iter().any(|link| link.target == line.note));
    let holder = kbx_core::parse_note("n.md", &read(&vault, &line.note)).unwrap();
    assert!(
        holder
            .relation("depends_on")
            .is_none_or(|entry| { !entry.links.iter().any(|link| link.target == line.target) })
    );
}

fn class(vault: &Fixture, id: &str) -> String {
    db(vault)
        .query_row("SELECT type FROM files WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn a_key_the_schema_reads_the_other_way_is_refused_and_not_turned_around() {
    let vault = staged();
    let all = lines(&vault);
    let line = all
        .iter()
        .find(|l| class(&vault, &l.note) != "Procedure" && class(&vault, &l.target) == "Procedure")
        .expect("a line from a non-procedure to a procedure");
    let target_before = read(&vault, &line.target);
    move_line(&vault, line, "depends_on");

    let output = vault.kbx(&["sync-stage"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("other way round"), "{output:?}");
    assert!(reviews(&vault).is_empty());
    assert_eq!(read(&vault, &line.target), target_before);
    assert!(
        lines(&vault).iter().any(|l| l.cid == line.cid),
        "kept to retry"
    );
}

#[test]
fn a_link_written_on_the_target_is_an_acceptance_too() {
    let vault = staged();
    let line = &lines(&vault)[0];
    let swapped = Line {
        cid: line.cid.clone(),
        note: line.target.clone(),
        target: line.note.clone(),
    };
    let path = path_of(&vault, &swapped.note);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("contradicts:"),
        "a key the note doesn't have"
    );
    let new = format!("---\ncontradicts: [\"[[{}]]\"]\n", swapped.target);
    fs::write(path, text.replacen("---\n", &new, 1)).unwrap();

    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("1 accepted"), "{printed}");
    assert_eq!(
        reviews(&vault),
        [("contradicts".into(), "accept".into(), 0)]
    );
    assert!(
        !read(&vault, &line.note).contains(&line.cid),
        "the line goes"
    );
}

#[test]
fn a_target_edited_after_staging_does_not_strand_a_moved_line() {
    let vault = staged();
    let line = &lines(&vault)[0];
    let path = path_of(&vault, &line.target);
    let edited = format!("{}\nEdited.\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, edited).unwrap();
    move_line(&vault, line, "see_also");

    assert!(vault.ok(&["sync-stage"]).contains("1 accepted"));
    assert!(read(&vault, &line.target).ends_with("Edited.\n"));
}

#[test]
fn a_line_moved_under_an_unknown_key_is_refused_and_is_no_rejection() {
    let vault = staged();
    let line = &lines(&vault)[0];
    move_line(&vault, line, "sea_also");

    let output = vault.kbx(&["sync-stage"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("sea_also"), "{output:?}");
    assert!(reviews(&vault).is_empty());
    assert!(lines(&vault).iter().any(|l| l.cid == line.cid));
}

#[test]
fn a_note_that_went_away_is_no_rejection() {
    let vault = staged();
    let line = &lines(&vault)[0];
    fs::remove_file(path_of(&vault, &line.note)).unwrap();
    vault.ok(&["sync-stage"]);
    assert!(reviews(&vault).is_empty());
}

#[test]
fn a_note_that_does_not_parse_is_no_rejection_and_is_synced_once_it_does() {
    let vault = staged();
    let line = &lines(&vault)[0];
    let path = path_of(&vault, &line.note);
    let good = fs::read_to_string(&path).unwrap();
    fs::write(&path, good.replacen("title:", "title: [", 1)).unwrap();

    vault.ok(&["sync-stage"]);
    assert!(reviews(&vault).is_empty());

    // Mended, by now out of the index, and with the line gone.
    let kept: Vec<&str> = good
        .split_inclusive('\n')
        .filter(|l| !l.contains(&line.cid))
        .collect();
    fs::write(&path, kept.concat()).unwrap();
    assert!(vault.ok(&["sync-stage"]).contains("1 rejected"));
}

#[test]
fn a_mangled_proposal_list_is_no_rejection() {
    let vault = staged();
    let line = &lines(&vault)[0];
    let path = path_of(&vault, &line.note);
    let text = fs::read_to_string(&path).unwrap();
    let mangled = text.replace(&format!("id: {}", line.cid), "ident: oops");
    assert_ne!(mangled, text);
    fs::write(&path, mangled).unwrap();
    vault.ok(&["sync-stage"]);
    assert!(reviews(&vault).is_empty());
}

#[test]
fn a_block_rewritten_by_an_editor_still_syncs() {
    let vault = staged();
    let all = lines(&vault);
    let line = &all[0];
    let path = path_of(&vault, &line.note);
    let text = fs::read_to_string(&path).unwrap();
    // How Obsidian writes a nested list back: block style, every key a line.
    let mut out = String::new();
    for row in text.split_inclusive('\n') {
        let entry = row.trim_start().strip_prefix("- {").map(|e| e.trim_end());
        let Some(entry) = entry.and_then(|e| e.strip_suffix('}')) else {
            out.push_str(row);
            continue;
        };
        let cid = entry.rsplit("id: ").next().unwrap();
        let score = entry
            .split("score: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();
        let to = entry
            .split("to: ")
            .nth(1)
            .unwrap()
            .split(", by")
            .next()
            .unwrap();
        out.push_str(&format!(
            "    - rel: none\n      to: {to}\n      by:\n        - common_neighbors\n      \
             score: {score}\n      id: {cid}\n"
        ));
    }
    fs::write(&path, out).unwrap();
    let printed = vault.ok(&["sync-stage"]);
    assert!(printed.contains("0 rejected"), "{printed}");
    assert!(reviews(&vault).is_empty());
}

#[test]
fn the_graph_is_the_same_with_and_without_staged_entries() {
    let vault = indexed();
    let before = compiled(&vault, false);
    stage(&vault);
    assert!(!lines(&vault).is_empty());
    vault.ok(&["index", "--full"]);
    assert_eq!(before, compiled(&vault, false));

    // Line numbers move with the lines above them, and nothing else does: with
    // the block blanked and every line kept, each row is the same to the byte.
    let staged = compiled(&vault, true);
    for path in notes(&vault) {
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, blank_kbx(&text)).unwrap();
    }
    vault.ok(&["index", "--full"]);
    assert_eq!(staged, compiled(&vault, true));
}

#[test]
fn no_staged_target_reaches_the_graph() {
    let vault = staged();
    vault.ok(&["index", "--full"]);
    let db = db(&vault);
    for line in lines(&vault) {
        let linked: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE src = ?1 AND dst = ?2",
                [&line.note, &line.target],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(linked, 0);
    }
}

#[test]
fn lint_says_the_same_about_a_staged_vault() {
    let plain = indexed();
    let vault = staged();
    let report = |fixture: &Fixture| {
        let output = fixture.kbx(&["lint", "--format", "json"]);
        let json: Value = serde_json::from_str(&stdout(&output)).unwrap();
        (json["errors"].clone(), json["warnings"].clone())
    };
    assert_eq!(report(&plain), report(&vault));
}

/// A vault in which note 2, the fixture's reviewed one, is otherwise suggested.
fn reviewed_note() -> String {
    id(2)
}

#[test]
fn a_reviewed_note_receives_no_new_proposals() {
    let vault = indexed();
    let note = reviewed_note();
    assert!(read(&vault, &note).contains("reviewed: 2026-11-03"));
    stage(&vault);

    assert!(!candidate_touches(&vault, &note));
    assert!(
        !lines(&vault)
            .iter()
            .any(|l| l.note == note || l.target == note)
    );
    assert!(!read(&vault, &note).contains("common_neighbors"));
}

#[test]
fn the_same_note_without_the_lock_is_proposed() {
    let vault = indexed();
    let note = reviewed_note();
    let text = read(&vault, &note).replace("  reviewed: 2026-11-03\n", "");
    fs::write(path_of(&vault, &note), text).unwrap();
    vault.ok(&["index"]);
    stage(&vault);
    assert!(candidate_touches(&vault, &note));
}

#[test]
fn editing_a_reviewed_note_lifts_the_lock() {
    let vault = indexed();
    let note = reviewed_note();
    let path = path_of(&vault, &note);
    let edited = format!("{}\nEdited.\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, edited).unwrap();
    vault.ok(&["index"]);
    stage(&vault);
    assert!(candidate_touches(&vault, &note));
}

#[test]
fn changes_to_the_tool_namespaces_do_not_lift_the_lock() {
    let vault = indexed();
    let note = reviewed_note();
    let path = path_of(&vault, &note);
    let text = fs::read_to_string(&path).unwrap();
    let changed = text
        .replace("id: 7f3a9c", "id: 7f3a9d")
        .replace("tags: [auth, security]}", "tags: [auth]}");
    assert_ne!(changed, text);
    fs::write(&path, changed).unwrap();
    vault.ok(&["index"]);
    stage(&vault);
    assert!(!candidate_touches(&vault, &note));
}

#[test]
fn a_rebuild_keeps_the_lock_and_what_lifted_it() {
    let vault = indexed();
    let note = reviewed_note();
    vault.ok(&["index", "--full"]);
    stage(&vault);
    assert!(!candidate_touches(&vault, &note));

    let path = path_of(&vault, &note);
    let edited = format!("{}\nEdited.\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, edited).unwrap();
    vault.ok(&["index"]);
    vault.ok(&["index", "--full"]);
    stage(&vault);
    assert!(candidate_touches(&vault, &note));
}

#[test]
fn a_dry_run_honours_the_lock() {
    let vault = indexed();
    let mut args = SUGGEST.to_vec();
    args.push("--dry-run");
    let printed = vault.ok(&args);
    assert!(printed.contains("at reviewed notes"), "{printed}");
}

#[test]
fn mark_reviewed_locks_the_notes_a_sync_finished() {
    let vault = staged();
    let all = lines(&vault);
    let line = all
        .iter()
        .find(|a| all.iter().filter(|b| b.note == a.note).count() == 1)
        .expect("a note with one line");
    remove_line(&vault, line);
    vault.ok(&["sync-stage", "--mark-reviewed"]);

    let text = read(&vault, &line.note);
    let parsed = kbx_core::parse_note("n.md", &text).unwrap();
    let kbx = parsed.kbx.as_ref().expect("a kbx block");
    let date = kbx.get("reviewed").and_then(Yaml::as_str).unwrap();
    assert_eq!(date.len(), 10, "{date}");
    assert!(date.starts_with("20"), "{date}");

    stage(&vault);
    assert!(!lines(&vault).iter().any(|l| l.note == line.note));
    let others = lines(&vault);
    assert!(others.iter().any(|l| l.note != line.note));
}

#[test]
fn without_the_flag_a_sync_locks_nothing() {
    let vault = staged();
    let line = &lines(&vault)[0];
    remove_line(&vault, line);
    vault.ok(&["sync-stage"]);
    assert!(!read(&vault, &line.note).contains("reviewed:"));
}
