//! The frontmatter writer (§7).
//!
//! Edits are made on the text, because a `serde_yaml` round trip would drop
//! every comment and restyle every value. Each edit is then checked by parsing
//! the result: the mapping must equal the old one plus the change, with its
//! keys in the same order. YAML too unusual to edit that way is refused rather
//! than rewritten. See `docs/frontmatter-writer.md` for what that covers.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};
use tempfile::{Builder, NamedTempFile};

use crate::index::sha256;
use crate::note::{Note, ParseError, bom_len, parse_note, split_frontmatter};

const QMD: &str = "qmd";
const METADATA: &str = "metadata";
const KBX: &str = "kbx";
const PROPOSED: &str = "proposed";
const REVIEWED: &str = "reviewed";

/// Why a note's frontmatter could not be edited.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// The file is not a note that kbx can read.
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// The key already holds something other than wikilinks.
    #[error("frontmatter key `{0}` holds something other than wikilinks")]
    NotLinks(String),
    /// `qmd:` is not a map, so there is nowhere to mirror into.
    #[error("frontmatter key `qmd` is not a map")]
    QmdNotAMap,
    /// `kbx:` is not a map, so there is nowhere to stage into.
    #[error("frontmatter key `kbx` is not a map")]
    KbxNotAMap,
    /// The edit would have changed more than it should. Nothing is written.
    #[error("can't edit `{0}` in this frontmatter without changing anything else; edit it by hand")]
    Unsupported(String),
}

/// Adds `[[target]]` to the relation `key`, creating the key if needed, and
/// mirrors `type` and `tags` into `qmd.metadata`. Returns the whole new file,
/// or `None` if the link is already there and the mirror is current.
///
/// `is_target` says whether a link target as written names the same note.
/// That takes the vault, because `[[b-child]]` is either `b` with a slug or
/// another note. Only the text between the `---` fences changes. A note with
/// no fences gets them, around the relation alone, and its body stays as it is.
pub fn add_relation(
    src: &str,
    key: &str,
    target: &str,
    is_target: impl Fn(&str) -> bool,
) -> Result<Option<String>, WriteError> {
    let block = FrontmatterBlock::of(src)?;
    let note = parse_note("", src)?;
    let old = match serde_yaml::from_str(block.yaml).map_err(ParseError::Yaml)? {
        Value::Mapping(old) => old,
        Value::Null => Mapping::new(),
        _ => return Err(ParseError::NotAMapping.into()),
    };

    let mut expected = old.clone();
    let mut yaml = block.yaml.to_string();
    if !holds_link(&note, key, is_target) {
        let link = format!("[[{target}]]");
        yaml = append_link(&yaml, key, &link, &note, &old)?;
        expected.insert(key.into(), with_link(old.get(key), &link));
    }
    if let Some(metadata) = stale_metadata(&note, &old)? {
        yaml = write_metadata(&yaml, &metadata, &old)?;
        set_metadata(&mut expected, metadata);
    }
    if yaml == block.yaml {
        return Ok(None);
    }
    verify(&yaml, &old, &expected, key)?;
    Ok(Some(block.with_yaml(yaml)))
}

/// A note's text around the YAML between its fences. A note with no fences
/// has an empty block, with fences to match its first line ending.
struct FrontmatterBlock<'a> {
    head: Cow<'a, str>,
    yaml: &'a str,
    tail: Cow<'a, str>,
}

impl<'a> FrontmatterBlock<'a> {
    fn of(src: &'a str) -> Result<FrontmatterBlock<'a>, ParseError> {
        if let Some(frontmatter) = split_frontmatter(src)? {
            let end = frontmatter.yaml_start + frontmatter.yaml.len();
            return Ok(FrontmatterBlock {
                head: src[..frontmatter.yaml_start].into(),
                yaml: frontmatter.yaml,
                tail: src[end..].into(),
            });
        }
        let (bom, body) = src.split_at(bom_len(src));
        let eol = match body.find('\n') {
            Some(at) if body[..at].ends_with('\r') => "\r\n",
            _ => "\n",
        };
        Ok(FrontmatterBlock {
            head: format!("{bom}---{eol}").into(),
            yaml: "",
            tail: format!("---{eol}{body}").into(),
        })
    }

    /// The whole file around `yaml`. Lines added to an empty block had no
    /// neighbour to take a line ending from, so they take the fence's.
    fn with_yaml(&self, yaml: String) -> String {
        let yaml = match self.yaml.is_empty() && self.head.ends_with("\r\n") {
            true => yaml.replace('\n', "\r\n"),
            false => yaml,
        };
        format!("{}{yaml}{}", self.head, self.tail)
    }
}

/// A proposal under `kbx.proposed` (§3), which the compiler never reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedEntry {
    /// The relation proposed, or [`NONE`](crate::NONE) if nothing names one.
    pub rel: String,
    /// The note it would point at, as a wikilink.
    pub to: String,
    /// What proposed it: the generators, and `judge` if a judge agreed.
    pub by: Vec<String>,
    /// The signals combined, from 0 to 1.
    pub score: f64,
    /// The candidate's cid.
    pub id: String,
}

impl StagedEntry {
    fn value(&self) -> Value {
        let by = self.by.iter().map(|name| Value::from(name.as_str()));
        let mut entry = Mapping::new();
        entry.insert("rel".into(), self.rel.as_str().into());
        entry.insert("to".into(), self.to.as_str().into());
        entry.insert("by".into(), Value::Sequence(by.collect()));
        entry.insert("score".into(), self.score.into());
        entry.insert("id".into(), self.id.as_str().into());
        Value::Mapping(entry)
    }
}

/// Adds the entries not already under `kbx.proposed`. Returns the whole new
/// file, or `None` if there was nothing to add.
pub fn stage_entries(src: &str, entries: &[StagedEntry]) -> Result<Option<String>, WriteError> {
    edit_kbx(src, |kbx| {
        let mut proposed = proposed_list(kbx)?;
        let mut held: HashSet<String> = proposed.iter().filter_map(entry_id).collect();
        for entry in entries {
            if held.insert(entry.id.clone()) {
                proposed.push(entry.value());
            }
        }
        set_proposed(kbx, proposed);
        Ok(())
    })
}

/// Takes the entries with these ids out of `kbx.proposed`, and the key with
/// them when it empties, and `kbx:` too if nothing else is under it. Returns
/// the whole new file, or `None` if none of them was there.
pub fn unstage_entries(src: &str, ids: &[&str]) -> Result<Option<String>, WriteError> {
    edit_kbx(src, |kbx| {
        let mut proposed = proposed_list(kbx)?;
        proposed.retain(|item| !entry_id(item).is_some_and(|id| ids.contains(&id.as_str())));
        set_proposed(kbx, proposed);
        Ok(())
    })
}

/// Sets `kbx.reviewed`. Returns the whole new file, or `None` if it says so.
pub fn set_reviewed(src: &str, date: &str) -> Result<Option<String>, WriteError> {
    edit_kbx(src, |kbx| {
        kbx.insert(REVIEWED.into(), date.into());
        Ok(())
    })
}

/// The ids under `kbx.proposed`, or `None` if it holds something else. A list
/// someone mangled says nothing about which proposals they meant to remove.
pub fn proposed_ids(note: &Note) -> Option<HashSet<String>> {
    let kbx = match &note.kbx {
        None | Some(Value::Null) => return Some(HashSet::new()),
        Some(Value::Mapping(kbx)) => kbx,
        Some(_) => return None,
    };
    match kbx.get(PROPOSED) {
        None | Some(Value::Null) => Some(HashSet::new()),
        Some(Value::Sequence(items)) => items.iter().map(entry_id).collect(),
        Some(_) => None,
    }
}

/// `kbx.reviewed`, if the note has one.
pub(crate) fn reviewed(note: &Note) -> Option<&str> {
    note.kbx.as_ref()?.get(REVIEWED)?.as_str()
}

/// A hash of the file without `kbx:` and `qmd:`, the blocks kbx writes itself.
/// It stays put through staging, syncing and mirroring, so it says whether the
/// note's own content changed, which `content_hash` can't.
pub(crate) fn user_content(src: &str) -> String {
    let Ok(Some(frontmatter)) = split_frontmatter(src) else {
        return sha256(src.as_bytes());
    };
    let yaml = frontmatter.yaml;
    let mut kept = src[..frontmatter.yaml_start].to_string();
    let mut at = 0;
    for block in blocks(yaml)
        .iter()
        .filter(|b| matches!(b.key.as_str(), KBX | QMD))
    {
        kept.push_str(&yaml[at..block.start]);
        at = block.content_end(yaml);
    }
    kept.push_str(&yaml[at..]);
    kept.push_str(&src[frontmatter.yaml_start + yaml.len()..]);
    sha256(kept.as_bytes())
}

fn entry_id(item: &Value) -> Option<String> {
    item.get("id")?.as_str().map(str::to_string)
}

fn proposed_list(kbx: &Mapping) -> Result<Vec<Value>, WriteError> {
    match kbx.get(PROPOSED) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Sequence(items)) => Ok(items.clone()),
        Some(_) => Err(WriteError::Unsupported(format!("{KBX}.{PROPOSED}"))),
    }
}

fn set_proposed(kbx: &mut Mapping, proposed: Vec<Value>) {
    if proposed.is_empty() {
        kbx.remove(PROPOSED);
    } else if kbx.contains_key(PROPOSED) {
        kbx.insert(PROPOSED.into(), Value::Sequence(proposed));
    } else {
        // §3 has `proposed` first.
        let rest = std::mem::take(kbx);
        kbx.insert(PROPOSED.into(), Value::Sequence(proposed));
        kbx.extend(rest);
    }
}

/// Changes `kbx:` and mirrors `qmd.metadata`, as every write does (§3).
///
/// The block is tool-owned, and an editor may have written it back in any
/// style, so it's written out again from its value rather than edited by line.
/// A comment inside it goes. Everything else is as in [`add_relation`].
fn edit_kbx(
    src: &str,
    change: impl FnOnce(&mut Mapping) -> Result<(), WriteError>,
) -> Result<Option<String>, WriteError> {
    let frontmatter = split_frontmatter(src)?.ok_or(ParseError::NoFrontmatter)?;
    let note = parse_note("", src)?;
    let Value::Mapping(old) = serde_yaml::from_str(frontmatter.yaml).map_err(ParseError::Yaml)?
    else {
        return Err(ParseError::NotAMapping.into());
    };
    let before = match old.get(KBX) {
        None | Some(Value::Null) => Mapping::new(),
        Some(Value::Mapping(kbx)) => kbx.clone(),
        Some(_) => return Err(WriteError::KbxNotAMap),
    };
    let mut kbx = before.clone();
    change(&mut kbx)?;
    if kbx == before {
        return Ok(None);
    }

    let mut expected = old.clone();
    let mut yaml = write_kbx(frontmatter.yaml, &kbx)
        .ok_or_else(|| WriteError::Unsupported(KBX.to_string()))?;
    match kbx.is_empty() {
        true => expected.remove(KBX),
        false => expected.insert(KBX.into(), Value::Mapping(kbx)),
    };
    if let Some(metadata) = stale_metadata(&note, &old)? {
        yaml = write_metadata(&yaml, &metadata, &old)?;
        set_metadata(&mut expected, metadata);
    }
    verify(&yaml, &old, &expected, KBX)?;

    let (head, tail) = (
        &src[..frontmatter.yaml_start],
        &src[frontmatter.yaml_start + frontmatter.yaml.len()..],
    );
    Ok(Some(format!("{head}{yaml}{tail}")))
}

/// Replaces the `kbx:` block, or adds one where [`new_key`] puts relations
/// ahead of, so that §3's order holds: relations, `kbx:`, `qmd:`.
fn write_kbx(yaml: &str, kbx: &Mapping) -> Option<String> {
    let all = blocks(yaml);
    if let Some(block) = all.iter().find(|b| b.key == KBX) {
        let end = block.content_end(yaml);
        let text = kbx_text(kbx, eol_at(yaml, end));
        return Some(format!("{}{text}{}", &yaml[..block.start], &yaml[end..]));
    }
    let last = all.iter().rfind(|b| !matches!(b.key.as_str(), KBX | QMD))?;
    let at = last.content_end(yaml);
    Some(insert(yaml, at, &kbx_text(kbx, eol_at(yaml, at))))
}

/// The block as §3 shows it: one line a key, and one line a list item.
fn kbx_text(kbx: &Mapping, eol: &str) -> String {
    if kbx.is_empty() {
        return String::new();
    }
    let mut text = format!("{KBX}:{eol}");
    for (key, value) in kbx {
        let key = flow(key);
        match value {
            Value::Sequence(items) if !items.is_empty() => {
                text.push_str(&format!("  {key}:{eol}"));
                for item in items {
                    text.push_str(&format!("    - {}{eol}", flow(item)));
                }
            }
            value => text.push_str(&format!("  {key}: {}{eol}", flow(value))),
        }
    }
    text
}

/// Replaces a file with `bytes`, or leaves it as it was: see [`Staged`].
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    Staged::new(path, bytes)?.commit()
}

/// New content in a temp file next to the note, ready to rename over it. A
/// rename within a folder is atomic, so readers see the old file or the new
/// one. The temp name starts with a dot, so nothing indexes it.
#[derive(Debug)]
pub(crate) struct Staged {
    file: NamedTempFile,
    target: PathBuf,
}

impl Staged {
    /// Writes the temp file with the note's permissions. Follows a symlink, so
    /// that the link survives and the file it points at is what changes.
    pub(crate) fn new(path: &Path, bytes: &[u8]) -> io::Result<Staged> {
        let target = path.canonicalize()?;
        let permissions = fs::metadata(&target)?.permissions();
        let folder = target.parent().unwrap_or(Path::new("."));
        let mut file = Builder::new()
            .prefix(".kbx-")
            .suffix(".tmp")
            .tempfile_in(folder)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        fs::set_permissions(file.path(), permissions)?;
        Ok(Staged { file, target })
    }

    pub(crate) fn commit(self) -> io::Result<()> {
        self.file
            .persist(&self.target)
            .map(drop)
            .map_err(|error| error.error)
    }
}

/// Whether a link under the key names the note `is_target` picks out.
pub(crate) fn holds_link(note: &Note, key: &str, is_target: impl Fn(&str) -> bool) -> bool {
    note.relation(key)
        .is_some_and(|entry| entry.links.iter().any(|link| is_target(&link.target)))
}

/// The key's value once `link` is added, for checking the edit against.
fn with_link(old: Option<&Value>, link: &str) -> Value {
    let link = Value::from(link);
    match old {
        Some(Value::Sequence(items)) => {
            Value::Sequence(items.iter().cloned().chain([link]).collect())
        }
        Some(Value::String(text)) => Value::Sequence(vec![text.as_str().into(), link]),
        _ => Value::Sequence(vec![link]),
    }
}

/// A top-level key and the lines it owns, as byte offsets into the YAML. Blank
/// lines and comments before the next key count as this key's.
struct Block {
    key: String,
    start: usize,
    /// The first byte after the key's colon.
    value: usize,
    end: usize,
}

impl Block {
    /// The start of the line after the key's.
    fn next_line(&self, yaml: &str) -> usize {
        yaml[self.value..self.end]
            .find('\n')
            .map_or(self.end, |at| self.value + at + 1)
    }

    /// The start of the first line of trailing blanks and top-level comments.
    fn content_end(&self, yaml: &str) -> usize {
        let mut end = self.start;
        let mut offset = self.start;
        for line in yaml[self.start..self.end].split_inclusive('\n') {
            offset += line.len();
            if !line.trim().is_empty() && !line.starts_with('#') {
                end = offset;
            }
        }
        end
    }
}

fn blocks(yaml: &str) -> Vec<Block> {
    let mut found: Vec<Block> = Vec::new();
    let mut offset = 0;
    for line in yaml.split_inclusive('\n') {
        if let Some((key, colon)) = top_level_key(line) {
            if let Some(last) = found.last_mut() {
                last.end = offset;
            }
            found.push(Block {
                key,
                start: offset,
                value: offset + colon + 1,
                end: yaml.len(),
            });
        }
        offset += line.len();
    }
    found
}

/// The key a line starts, if it starts one, and the offset of its colon. An
/// indented line, a comment and a list item at the margin all belong to the
/// key above.
fn top_level_key(line: &str) -> Option<(String, usize)> {
    let first = line.chars().next()?;
    if first.is_whitespace() || matches!(first, '#' | '-' | ']' | '}') {
        return None;
    }
    if first == '"' || first == '\'' {
        let close = line[1..].find(first)? + 1;
        let rest = &line[close + 1..];
        let colon = close + 1 + rest.find(|c: char| c != ' ')?;
        return (line[colon..].starts_with(':')).then(|| (line[1..close].to_string(), colon));
    }
    let colon = line
        .char_indices()
        .filter(|(_, c)| *c == ':')
        .map(|(at, _)| at)
        .find(|at| {
            line[at + 1..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
        })?;
    Some((line[..colon].trim_end().to_string(), colon))
}

fn insert(yaml: &str, at: usize, text: &str) -> String {
    format!("{}{text}{}", &yaml[..at], &yaml[at..])
}

/// The line ending of the line before `at`, so new lines match their
/// neighbours in a CRLF file.
fn eol_at(yaml: &str, at: usize) -> &'static str {
    if yaml[..at].ends_with("\r\n") || (at == 0 && yaml.contains("\r\n")) {
        "\r\n"
    } else {
        "\n"
    }
}

fn append_link(
    yaml: &str,
    key: &str,
    link: &str,
    note: &Note,
    old: &Mapping,
) -> Result<String, WriteError> {
    let quoted = quote(link);
    let holds_links = note.relation(key).is_some();
    let unsupported = || WriteError::Unsupported(key.to_string());
    let value = old.get(key);
    let empty = match value {
        None | Some(Value::Null) => true,
        Some(Value::Sequence(items)) => items.is_empty(),
        Some(_) => false,
    };
    if !holds_links && !empty {
        return Err(WriteError::NotLinks(key.to_string()));
    }
    if value.is_none() {
        return Ok(new_key(yaml, key, &quoted));
    }

    let all = blocks(yaml);
    let block = all.iter().find(|b| b.key == key).ok_or_else(unsupported)?;
    let line_end = block.next_line(yaml);
    let rest = &yaml[block.value..line_end];
    let start = block.value + (rest.len() - rest.trim_start_matches([' ', '\t']).len());
    match yaml[start..].chars().next() {
        Some('[') => {
            let last = flow_end(yaml, start, block.end).ok_or_else(unsupported)?;
            let text = match yaml[..last].chars().next_back() {
                Some('[') => quoted,
                Some(',') => format!(" {quoted}"),
                _ => format!(", {quoted}"),
            };
            Ok(insert(yaml, last, &text))
        }
        Some('"' | '\'') => {
            let end = quoted_end(yaml, start).ok_or_else(unsupported)?;
            let wrapped = insert(yaml, end, &format!(", {quoted}]"));
            Ok(insert(&wrapped, start, "["))
        }
        Some('#' | '\r' | '\n') if matches!(value, Some(Value::Null)) => {
            Ok(insert(yaml, block.value, &format!(" [{quoted}]")))
        }
        Some('#' | '\r' | '\n') => {
            append_item(yaml, line_end, block.end, &quoted).ok_or_else(unsupported)
        }
        _ => Err(unsupported()),
    }
}

/// Adds `key: ["[[…]]"]` after the last key outside the tool namespaces, which
/// is where §3 shows relations, or first if there is no such key.
fn new_key(yaml: &str, key: &str, quoted: &str) -> String {
    let all = blocks(yaml);
    let last = all.iter().rfind(|b| !matches!(b.key.as_str(), KBX | QMD));
    let at = last.map_or(0, |last| last.content_end(yaml));
    let eol = eol_at(yaml, at);
    insert(yaml, at, &format!("{key}: [{quoted}]{eol}"))
}

/// Adds a `- item` line after the last item of a block list, with its indent.
fn append_item(yaml: &str, first: usize, end: usize, quoted: &str) -> Option<String> {
    let mut indent = None;
    let mut after_last = None;
    let mut offset = first;
    for line in yaml[first..end].split_inclusive('\n') {
        offset += line.len();
        let text = line.trim_start_matches([' ', '\t']);
        if text.trim().is_empty() || text.starts_with('#') {
            continue;
        }
        indent.get_or_insert(&line[..line.len() - text.len()]);
        after_last = Some(offset);
    }
    let at = after_last?;
    let eol = eol_at(yaml, at);
    Some(insert(yaml, at, &format!("{}- {quoted}{eol}", indent?)))
}

/// The offset just past the last element or comma of the flow sequence opening
/// at `open`, or just past the `[` if it's empty. Skips strings and comments.
fn flow_end(yaml: &str, open: usize, end: usize) -> Option<usize> {
    let mut depth = 0;
    let mut last = open + 1;
    let mut at = open;
    while at < end {
        let c = yaml[at..].chars().next()?;
        match c {
            '"' | '\'' => {
                at = quoted_end(yaml, at)?;
                last = at;
                continue;
            }
            '#' if yaml[..at].ends_with([' ', '\t', '\n']) => {
                at += yaml[at..].find('\n')?;
                continue;
            }
            '[' | '{' => depth += 1,
            ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(last);
                }
            }
            _ => {}
        }
        at += c.len_utf8();
        if !c.is_whitespace() {
            last = at;
        }
    }
    None
}

/// The offset just past the quoted scalar opening at `open`.
fn quoted_end(yaml: &str, open: usize) -> Option<usize> {
    let quote = yaml[open..].chars().next()?;
    let mut chars = yaml[open + 1..].char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c == '\\' && quote == '"' {
            chars.next();
        } else if c == quote {
            if quote == '\'' && chars.peek().is_some_and(|(_, next)| *next == '\'') {
                chars.next();
            } else {
                return Some(open + 1 + at + 1);
            }
        }
    }
    None
}

/// What `qmd.metadata` should hold, if that isn't what it holds. Keys other
/// than `type` and `tags` stay. A note with an inferred key gets no mirror: the
/// mirror would be kbx's own guess, written into a note that never stated it.
fn stale_metadata(note: &Note, old: &Mapping) -> Result<Option<Mapping>, WriteError> {
    if note.inferred.any() {
        return Ok(None);
    }
    let qmd = match old.get(QMD) {
        None | Some(Value::Null) => None,
        Some(Value::Mapping(qmd)) => Some(qmd),
        Some(_) => return Err(WriteError::QmdNotAMap),
    };
    let current = qmd.and_then(|qmd| qmd.get(METADATA));
    let mut metadata = match current {
        Some(Value::Mapping(metadata)) => metadata.clone(),
        _ => Mapping::new(),
    };
    let tags = note.tags.iter().map(|tag| Value::from(tag.as_str()));
    metadata.insert("type".into(), note.class.as_str().into());
    metadata.insert("tags".into(), Value::Sequence(tags.collect()));
    let current = current.and_then(Value::as_mapping);
    Ok((current != Some(&metadata)).then_some(metadata))
}

fn set_metadata(frontmatter: &mut Mapping, metadata: Mapping) {
    let mut qmd = match frontmatter.get(QMD) {
        Some(Value::Mapping(qmd)) => qmd.clone(),
        _ => Mapping::new(),
    };
    qmd.insert(METADATA.into(), Value::Mapping(metadata));
    frontmatter.insert(QMD.into(), Value::Mapping(qmd));
}

/// Writes `qmd.metadata`. Other lines under `qmd:` stay as they are, unless
/// `qmd:` is one flow map, which has to be written out again.
fn write_metadata(yaml: &str, metadata: &Mapping, old: &Mapping) -> Result<String, WriteError> {
    let unsupported = || WriteError::Unsupported(QMD.to_string());
    let line = format!("{METADATA}: {}", flow(&Value::Mapping(metadata.clone())));
    let all = blocks(yaml);
    let Some(block) = all.iter().find(|b| b.key == QMD) else {
        let eol = eol_at(yaml, yaml.len());
        return Ok(format!("{yaml}{QMD}:{eol}  {line}{eol}"));
    };
    let line_end = block.next_line(yaml);
    let rest = yaml[block.value..line_end].trim_start_matches([' ', '\t']);

    if rest.starts_with('{') {
        let mut whole = old.clone();
        set_metadata(&mut whole, metadata.clone());
        let qmd = whole.get(QMD).ok_or_else(unsupported)?;
        let end = block.content_end(yaml);
        let eol = eol_at(yaml, end);
        let text = format!("{QMD}: {}{eol}", flow(qmd));
        return Ok(format!("{}{text}{}", &yaml[..block.start], &yaml[end..]));
    }
    if !(rest.trim().is_empty() || rest.starts_with('#')) {
        return Err(unsupported());
    }

    let children = yaml[line_end..block.end].split_inclusive('\n');
    let content = |text: &str| !text.trim().is_empty() && !text.trim_start().starts_with('#');
    let width = |text: &str| text.len() - text.trim_start_matches(' ').len();
    let indent = children.clone().find(|text| content(text)).map_or(2, width);
    let pad = " ".repeat(indent);

    // The metadata child runs from its key to its last deeper line.
    let (mut span_start, mut span_end) = (None, None);
    let mut offset = line_end;
    for text in children {
        let next = offset + text.len();
        if content(text) {
            let key = text[width(text)..].starts_with("metadata:");
            if width(text) == indent && span_start.is_some() {
                break;
            }
            if width(text) == indent && key {
                span_start = Some(offset);
            }
            if span_start.is_some() {
                span_end = Some(next);
            }
        }
        offset = next;
    }
    match (span_start, span_end) {
        (Some(start), Some(end)) => {
            let eol = eol_at(yaml, end);
            let text = format!("{pad}{line}{eol}");
            Ok(format!("{}{text}{}", &yaml[..start], &yaml[end..]))
        }
        _ => {
            let eol = eol_at(yaml, line_end);
            Ok(insert(yaml, line_end, &format!("{pad}{line}{eol}")))
        }
    }
}

/// A value in YAML flow style, as §3 shows `qmd.metadata`.
fn flow(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => scalar(text),
        Value::Sequence(items) => {
            let items: Vec<_> = items.iter().map(flow).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Mapping(map) => {
            let pairs: Vec<_> = map
                .iter()
                .map(|(key, value)| format!("{}: {}", flow(key), flow(value)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
        // No tag survives this, which the check after the edit refuses.
        Value::Tagged(tagged) => flow(&tagged.value),
    }
}

/// A string, plain if that reads back as the same string in flow style.
fn scalar(text: &str) -> String {
    let mut chars = text.chars();
    let starts_well = chars.next().is_some_and(char::is_alphanumeric);
    let safe = text
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.' | '/'));
    let reads_back = matches!(serde_yaml::from_str(text), Ok(Value::String(read)) if read == text);
    if starts_well && safe && !text.ends_with(' ') && reads_back {
        text.to_string()
    } else {
        quote(text)
    }
}

/// A double-quoted YAML string.
fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c.is_control() => quoted.push_str(&format!("\\u{:04x}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// The edited YAML must mean exactly what was intended, with the old keys in
/// their old order.
fn verify(yaml: &str, old: &Mapping, expected: &Mapping, key: &str) -> Result<(), WriteError> {
    let unsupported = || WriteError::Unsupported(key.to_string());
    let Ok(Value::Mapping(new)) = serde_yaml::from_str(yaml) else {
        return Err(unsupported());
    };
    let kept = new.keys().filter(|k| old.contains_key(*k));
    let wanted = old.keys().filter(|k| new.contains_key(*k));
    if new != *expected || !kept.eq(wanted) {
        return Err(unsupported());
    }
    Ok(())
}
