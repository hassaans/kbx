//! The frontmatter writer: what changes, what must not, and what is refused.

use std::fs;

use kbx_core::{
    StagedEntry, WriteError, add_relation, parse_note, proposed_ids, set_reviewed,
    split_frontmatter, stage_entries, unstage_entries, write_atomic,
};
use tempfile::TempDir;

const HEAD: &str = "id: A\ntype: Concept\ntitle: A note\n";
const MIRROR: &str = "qmd:\n  metadata: {type: Concept, tags: []}\n";

/// Adds a link, with only the target's id and the slugged `B-…` resolving to it.
fn add(src: &str, key: &str, target: &str) -> Result<Option<String>, WriteError> {
    add_relation(src, key, target, |link| {
        link == target
            || link
                .strip_prefix(target)
                .is_some_and(|r| r.starts_with('-'))
    })
}

fn note(yaml: &str) -> String {
    format!("---\n{yaml}---\nBody.\n")
}

/// Everything outside the text between the fences.
fn outside(src: &str) -> (Vec<u8>, Vec<u8>) {
    let frontmatter = split_frontmatter(src).unwrap().unwrap();
    let end = frontmatter.yaml_start + frontmatter.yaml.len();
    (
        src.as_bytes()[..frontmatter.yaml_start].to_vec(),
        src.as_bytes()[end..].to_vec(),
    )
}

/// Adds `[[B]]` to `rel`, requires the bytes outside the fences to stay, and
/// returns the new frontmatter.
fn edited(src: &str) -> String {
    let new = add(src, "rel", "B").unwrap().expect("a change");
    assert_eq!(outside(src), outside(&new));
    parse_note("n.md", &new).unwrap();
    split_frontmatter(&new).unwrap().unwrap().yaml.to_string()
}

#[test]
fn a_new_key_goes_after_the_last_user_key_and_before_the_tool_namespaces() {
    let src = note(&format!(
        "{HEAD}# about kbx\nkbx:\n  reviewed: 2026-11-03\n{MIRROR}"
    ));
    assert_eq!(
        edited(&src),
        format!("{HEAD}rel: [\"[[B]]\"]\n# about kbx\nkbx:\n  reviewed: 2026-11-03\n{MIRROR}")
    );
}

#[test]
fn a_flow_list_grows_in_place_and_keeps_its_comment() {
    let src = note(&format!(
        "{HEAD}rel: [\"[[A1]]\", '[[A2]]']  # why\ntags: []\n{MIRROR}"
    ));
    assert_eq!(
        edited(&src),
        format!("{HEAD}rel: [\"[[A1]]\", '[[A2]]', \"[[B]]\"]  # why\ntags: []\n{MIRROR}")
    );
}

#[test]
fn a_multi_line_flow_list_grows_after_its_last_element() {
    let src = note(&format!(
        "{HEAD}rel: [\n  \"[[A1]]\", # first\n  \"[[A2]]\"\n]\n{MIRROR}"
    ));
    assert_eq!(
        edited(&src),
        format!("{HEAD}rel: [\n  \"[[A1]]\", # first\n  \"[[A2]]\", \"[[B]]\"\n]\n{MIRROR}")
    );
}

#[test]
fn a_block_list_keeps_its_indent_at_the_margin_or_not() {
    for indent in ["", "  ", "    "] {
        let list = format!("rel: # why\n{indent}- \"[[A1]]\" # first\n{indent}# aside\n");
        let src = note(&format!("{HEAD}{list}tags: []\n{MIRROR}"));
        let grown = format!(
            "rel: # why\n{indent}- \"[[A1]]\" # first\n{indent}- \"[[B]]\"\n{indent}# aside\n"
        );
        assert_eq!(edited(&src), format!("{HEAD}{grown}tags: []\n{MIRROR}"));
    }
}

#[test]
fn a_single_link_becomes_a_list() {
    let src = note(&format!("{HEAD}rel: \"[[A1]]\" # why\n{MIRROR}"));
    assert_eq!(
        edited(&src),
        format!("{HEAD}rel: [\"[[A1]]\", \"[[B]]\"] # why\n{MIRROR}")
    );
}

#[test]
fn an_empty_key_is_filled() {
    for (before, after) in [
        ("rel:\n", "rel: [\"[[B]]\"]\n"),
        ("rel: # todo\n", "rel: [\"[[B]]\"] # todo\n"),
        ("rel: []\n", "rel: [\"[[B]]\"]\n"),
    ] {
        let src = note(&format!("{HEAD}{before}{MIRROR}"));
        assert_eq!(edited(&src), format!("{HEAD}{after}{MIRROR}"));
    }
}

#[test]
fn key_order_and_comments_survive() {
    let yaml = "# top\ntitle: T # the title\n\n# the id\nid: A\ntype: Concept\nsummary: |\n  # not a comment\n  text\n\naliases: [x] # names\n# the end\n";
    let new = edited(&note(yaml));
    assert!(
        new.starts_with("# top\ntitle: T # the title\n\n# the id\nid: A\n"),
        "{new}"
    );
    assert!(
        new.contains("summary: |\n  # not a comment\n  text\n"),
        "{new}"
    );
    assert!(
        new.contains("aliases: [x] # names\nrel: [\"[[B]]\"]\n# the end\n"),
        "{new}"
    );
}

#[test]
fn crlf_stays_crlf_and_the_outside_stays_byte_for_byte() {
    let src = "---\r\nid: A\r\ntype: Concept\r\ntitle: T\r\ntags: [x]\r\ndep:\r\n  - \"[[A1]]\"\r\n---\r\nBody\r\nmixed\n";
    let new = add(src, "dep", "B").unwrap().unwrap();
    assert_eq!(outside(src), outside(&new));
    assert_eq!(
        new,
        "---\r\nid: A\r\ntype: Concept\r\ntitle: T\r\ntags: [x]\r\ndep:\r\n  - \"[[A1]]\"\r\n  - \"[[B]]\"\r\nqmd:\r\n  metadata: {type: Concept, tags: [x]}\r\n---\r\nBody\r\nmixed\n"
    );
    let added = add(src, "rel", "B").unwrap().unwrap();
    assert_eq!(outside(src), outside(&added));
    let bare_newlines = added.replace("\r\n", "").matches('\n').count();
    assert_eq!(bare_newlines, 1, "only the body's own: {added:?}");
}

#[test]
fn awkward_bodies_and_fences_are_left_alone() {
    for src in [
        format!("\u{feff}---\n{HEAD}---\nBody\n"),
        format!("---\n{HEAD}---"),
        format!("---  \n{HEAD}---\t\n\n\n"),
        format!("---\n{HEAD}---\n---\nrel: [\"[[Z]]\"]\n---\n"),
        format!("---\n{HEAD}---\n  trailing spaces  \n\n\u{0}\ttabs"),
    ] {
        edited(&src);
    }
}

#[test]
fn the_mirror_follows_type_and_tags_and_leaves_other_qmd_keys() {
    let src = note(
        "id: A\ntype: Concept\ntitle: T\ntags: [auth, \"needs, quoting\", 12]\nqmd:\n  collection: x # keep\n  metadata: {type: Old, tags: [gone], owner: me}\n  extra:\n    - 1\n",
    );
    let new = edited(&src);
    assert!(new.contains("  collection: x # keep\n"), "{new}");
    assert!(new.contains("  extra:\n    - 1\n"), "{new}");
    assert!(
        new.contains(
            "  metadata: {type: Concept, tags: [auth, \"needs, quoting\", \"12\"], owner: me}\n"
        ),
        "{new}"
    );
}

#[test]
fn the_mirror_is_added_under_an_existing_qmd_key() {
    let new = edited(&note(&format!("{HEAD}qmd:\n    collection: x\n")));
    assert!(
        new.contains("qmd:\n    metadata: {type: Concept, tags: []}\n    collection: x\n"),
        "{new}"
    );
}

#[test]
fn a_current_mirror_is_not_touched_even_with_comments_in_it() {
    let block = "qmd:\n  metadata: # kept\n    type: Concept # kept too\n    tags: []\n";
    let new = edited(&note(&format!("{HEAD}{block}")));
    assert!(new.ends_with(block), "{new}");
}

/// The limitation in `docs/frontmatter-writer.md`: a stale `qmd.metadata` is
/// written again, and comments in it go. Comments around it stay.
#[test]
fn a_stale_mirror_loses_the_comments_inside_it_and_only_those() {
    let block = "qmd:\n  # above\n  metadata: # lost\n    type: Old # lost too\n    tags: []\n  # below\n  collection: x\n";
    let new = edited(&note(&format!("{HEAD}{block}")));
    assert!(
        new.ends_with(
            "qmd:\n  # above\n  metadata: {type: Concept, tags: []}\n  # below\n  collection: x\n"
        ),
        "{new}"
    );
}

/// The same limitation for a `qmd:` written as one flow map.
#[test]
fn a_flow_qmd_is_written_again_and_loses_its_comment() {
    let new = edited(&note(&format!("{HEAD}qmd: {{collection: x}} # lost\n")));
    assert!(
        new.ends_with("qmd: {collection: x, metadata: {type: Concept, tags: []}}\n"),
        "{new}"
    );
}

#[test]
fn nothing_to_do_returns_none() {
    for link in ["[[B]]", "[[B-some-slug]]", "[[B#heading]]", "[[B|shown]]"] {
        let src = note(&format!("{HEAD}rel: [\"{link}\"]\n{MIRROR}"));
        assert_eq!(add(&src, "rel", "B").unwrap(), None, "{link}");
    }
}

#[test]
fn an_existing_link_still_gets_a_stale_mirror_fixed() {
    let src = note(&format!("{HEAD}rel: [\"[[B]]\"]\n"));
    assert_eq!(edited(&src), format!("{HEAD}rel: [\"[[B]]\"]\n{MIRROR}"));
}

#[test]
fn what_counts_as_the_target_is_up_to_the_resolver() {
    let src = note(&format!("{HEAD}rel: [\"[[B-child]]\"]\n{MIRROR}"));
    let new = add_relation(&src, "rel", "B", |link| link == "B").unwrap();
    assert!(new.unwrap().contains("rel: [\"[[B-child]]\", \"[[B]]\"]\n"));
    assert_eq!(add_relation(&src, "rel", "B", |_| true).unwrap(), None);
}

#[test]
fn a_prefix_of_an_id_is_not_that_id() {
    let src = note(&format!("{HEAD}rel: [\"[[BC]]\"]\n{MIRROR}"));
    assert!(edited(&src).contains("rel: [\"[[BC]]\", \"[[B]]\"]\n"));
}

#[test]
fn a_key_holding_something_else_is_refused() {
    for value in ["rel: plain text\n", "rel: [one, two]\n", "rel: 3\n"] {
        let src = note(&format!("{HEAD}{value}"));
        let error = add(&src, "rel", "B").unwrap_err();
        assert!(matches!(error, WriteError::NotLinks(_)), "{value}: {error}");
    }
}

#[test]
fn a_qmd_that_is_not_a_map_is_refused() {
    let error = add(&note(&format!("{HEAD}qmd: 5\n")), "rel", "B").unwrap_err();
    assert!(matches!(error, WriteError::QmdNotAMap), "{error}");
}

#[test]
fn yaml_the_line_edit_cannot_follow_is_refused_not_rewritten() {
    for yaml in [
        // Keys indented as a whole.
        "  id: A\n  type: Concept\n  title: T\n",
        // `~` is null, with no room after the colon.
        "id: A\ntype: Concept\ntitle: T\nrel: ~\n",
        // A list behind an anchor.
        "id: A\ntype: Concept\ntitle: T\nrel: &links [\"[[A1]]\"]\n",
    ] {
        let error = add(&note(yaml), "rel", "B").unwrap_err();
        assert!(
            matches!(error, WriteError::Unsupported(_)),
            "{yaml}: {error}"
        );
    }
}

/// A line of `title` looks like the `rel` key, and the edit lands inside the
/// string. It parses, so only comparing the meaning catches it.
#[test]
fn an_edit_that_would_change_another_value_is_refused() {
    let yaml = "id: A\ntype: Concept\ntitle: 'two\nrel: \"[[Z]]\" lines'\nrel: [\"[[A1]]\"]\n";
    let src = note(yaml);
    assert_eq!(
        parse_note("n.md", &src).unwrap().title,
        "two rel: \"[[Z]]\" lines"
    );
    let error = add(&src, "rel", "B").unwrap_err();
    assert!(matches!(error, WriteError::Unsupported(_)), "{error}");
}

#[test]
fn a_file_kbx_cannot_read_is_refused() {
    let error = add("no frontmatter\n", "rel", "B").unwrap_err();
    assert!(matches!(error, WriteError::Parse(_)), "{error}");
    let error = add("---\nid: [\n---\n", "rel", "B").unwrap_err();
    assert!(matches!(error, WriteError::Parse(_)), "{error}");
}

#[test]
fn an_odd_target_id_is_quoted_so_it_reads_back() {
    let src = note(HEAD);
    let new = add(&src, "rel", "a\"b\\c").unwrap().unwrap();
    let parsed = parse_note("n.md", &new).unwrap();
    assert_eq!(parsed.relation("rel").unwrap().links[0].target, "a\"b\\c");
}

#[test]
fn write_atomic_replaces_the_file_and_leaves_no_temp_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("n.md");
    fs::write(&path, "old").unwrap();
    write_atomic(&path, b"new").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn write_atomic_keeps_the_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("n.md");
    for mode in [0o640, 0o600, 0o664] {
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        write_atomic(&path, b"new").unwrap();
        let kept = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(kept, mode);
    }
}

#[cfg(unix)]
#[test]
fn write_atomic_follows_a_symlink() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("real.md");
    let link = dir.path().join("link.md");
    fs::write(&real, "old").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    write_atomic(&link, b"new").unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&real).unwrap(), b"new");
}

fn entry(id: &str) -> StagedEntry {
    StagedEntry {
        rel: "see_also".to_string(),
        to: "[[B]]".to_string(),
        by: vec!["adamic_adar".to_string(), "judge".to_string()],
        score: 0.81,
        id: id.to_string(),
    }
}

/// Stages entries, requires the bytes outside the fences to stay, and returns
/// the new frontmatter.
fn staged(src: &str, ids: &[&str]) -> String {
    let entries: Vec<_> = ids.iter().map(|id| entry(id)).collect();
    let new = stage_entries(src, &entries).unwrap().expect("a change");
    assert_eq!(outside(src), outside(&new));
    parse_note("n.md", &new).unwrap();
    split_frontmatter(&new).unwrap().unwrap().yaml.to_string()
}

#[test]
fn a_first_entry_makes_the_block_before_qmd_as_section_3_shows_it() {
    let src = note(&format!("{HEAD}rel: [\"[[C]]\"]  # why\n{MIRROR}"));
    assert_eq!(
        staged(&src, &["7f3a"]),
        format!(
            "{HEAD}rel: [\"[[C]]\"]  # why\nkbx:\n  proposed:\n    - {{rel: see_also, \
             to: \"[[B]]\", by: [adamic_adar, judge], score: 0.81, id: 7f3a}}\n{MIRROR}"
        )
    );
}

#[test]
fn entries_join_the_list_and_the_rest_of_the_block_stays() {
    let src = note(&format!(
        "{HEAD}kbx:\n  proposed:\n    - {{to: \"[[Z]]\", id: old, extra: 1}}\n  reviewed: 2026-11-03\n{MIRROR}"
    ));
    let yaml = staged(&src, &["new"]);
    let kbx = "kbx:\n  proposed:\n    - {to: \"[[Z]]\", id: old, extra: 1}\n    - {rel: see_also, \
         to: \"[[B]]\", by: [adamic_adar, judge], score: 0.81, id: new}\n  \
         reviewed: 2026-11-03\n";
    assert_eq!(yaml, format!("{HEAD}{kbx}{MIRROR}"));
}

#[test]
fn an_entry_already_there_is_not_added_twice() {
    let src = note(&format!("{HEAD}{MIRROR}"));
    let once = stage_entries(&src, &[entry("a")]).unwrap().unwrap();
    assert_eq!(stage_entries(&once, &[entry("a")]).unwrap(), None);
}

#[test]
fn an_id_that_reads_as_a_number_stays_a_string() {
    let src = note(&format!("{HEAD}{MIRROR}"));
    let new = stage_entries(&src, &[entry("1234")]).unwrap().unwrap();
    let parsed = parse_note("n.md", &new).unwrap();
    assert_eq!(proposed_ids(&parsed).unwrap(), ["1234".to_string()].into());
}

#[test]
fn staging_mirrors_like_every_other_write() {
    let src = note(HEAD);
    assert_eq!(
        staged(&src, &["a"]),
        format!(
            "{HEAD}kbx:\n  proposed:\n    - {{rel: see_also, to: \"[[B]]\", \
             by: [adamic_adar, judge], score: 0.81, id: a}}\n{MIRROR}"
        )
    );
}

#[test]
fn a_crlf_note_stays_crlf_when_staged_and_unstaged() {
    let src = note(&format!("{HEAD}{MIRROR}")).replace('\n', "\r\n");
    let new = stage_entries(&src, &[entry("a")]).unwrap().unwrap();
    assert_eq!(new.matches('\n').count(), new.matches("\r\n").count());
    assert_eq!(unstage_entries(&new, &["a"]).unwrap().unwrap(), src);
}

#[test]
fn removing_the_last_entry_removes_the_block_and_gives_the_old_bytes_back() {
    let src = note(&format!("{HEAD}# about kbx\n{MIRROR}"));
    let new = stage_entries(&src, &[entry("a")]).unwrap().unwrap();
    assert_eq!(unstage_entries(&new, &["a"]).unwrap().unwrap(), src);
}

#[test]
fn removing_an_entry_keeps_reviewed_and_the_other_entries() {
    let src = note(&format!("{HEAD}kbx:\n  reviewed: 2026-11-03\n{MIRROR}"));
    let new = stage_entries(&src, &[entry("a"), entry("b")])
        .unwrap()
        .unwrap();
    let new = unstage_entries(&new, &["a"]).unwrap().unwrap();
    let parsed = parse_note("n.md", &new).unwrap();
    assert_eq!(proposed_ids(&parsed).unwrap(), ["b".to_string()].into());
    assert!(new.contains("reviewed: 2026-11-03"));
}

#[test]
fn removing_an_id_that_is_not_there_changes_nothing() {
    let src = note(&format!("{HEAD}{MIRROR}"));
    assert_eq!(unstage_entries(&src, &["a"]).unwrap(), None);
}

#[test]
fn a_block_style_list_is_read_and_can_lose_an_entry() {
    let src = note(&format!(
        "{HEAD}kbx:\n  proposed:\n    - rel: see_also\n      to: \"[[B]]\"\n      id: a\n    \
         - rel: see_also\n      to: \"[[C]]\"\n      id: b\n{MIRROR}"
    ));
    let parsed = parse_note("n.md", &src).unwrap();
    let both = ["a".to_string(), "b".to_string()].into();
    assert_eq!(proposed_ids(&parsed).unwrap(), both);
    let new = unstage_entries(&src, &["a"]).unwrap().unwrap();
    let parsed = parse_note("n.md", &new).unwrap();
    assert_eq!(proposed_ids(&parsed).unwrap(), ["b".to_string()].into());
}

#[test]
fn a_list_that_is_not_proposals_is_unreadable() {
    let bad = note(&format!("{HEAD}kbx:\n  proposed:\n    - to: x\n{MIRROR}"));
    assert_eq!(proposed_ids(&parse_note("n.md", &bad).unwrap()), None);
    let bad = note(&format!("{HEAD}kbx:\n  proposed: nope\n{MIRROR}"));
    assert_eq!(proposed_ids(&parse_note("n.md", &bad).unwrap()), None);
    let empty = note(&format!("{HEAD}kbx:\n  proposed:\n{MIRROR}"));
    let none = parse_note("n.md", &empty).unwrap();
    assert_eq!(proposed_ids(&none), Some(Default::default()));
}

#[test]
fn a_kbx_key_that_is_not_a_map_is_refused() {
    let src = note(&format!("{HEAD}kbx: nope\n{MIRROR}"));
    let error = stage_entries(&src, &[entry("a")]).unwrap_err();
    assert!(matches!(error, WriteError::KbxNotAMap), "{error}");
}

#[test]
fn reviewed_is_set_in_place_and_only_changes_what_it_should() {
    let src = note(&format!("{HEAD}rel: [\"[[C]]\"]  # why\n{MIRROR}"));
    let new = set_reviewed(&src, "2026-11-04").unwrap().unwrap();
    assert_eq!(outside(&src), outside(&new));
    assert!(new.contains("rel: [\"[[C]]\"]  # why\nkbx:\n  reviewed: 2026-11-04\nqmd:"));
    assert_eq!(set_reviewed(&new, "2026-11-04").unwrap(), None);
}
