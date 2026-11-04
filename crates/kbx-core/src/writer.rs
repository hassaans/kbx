//! The frontmatter writer (§7).
//!
//! Edits are made on the text, because a `serde_yaml` round trip would drop
//! every comment and restyle every value. Each edit is then checked by parsing
//! the result: the mapping must equal the old one plus the change, with its
//! keys in the same order. YAML too unusual to edit that way is refused rather
//! than rewritten. See `docs/frontmatter-writer.md` for what that covers.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};
use tempfile::{Builder, NamedTempFile};

use crate::note::{Note, ParseError, parse_note, split_frontmatter};

const QMD: &str = "qmd";
const METADATA: &str = "metadata";

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
/// another note. Only the text between the `---` fences changes.
pub fn add_relation(
    src: &str,
    key: &str,
    target: &str,
    is_target: impl Fn(&str) -> bool,
) -> Result<Option<String>, WriteError> {
    let frontmatter = split_frontmatter(src)?.ok_or(ParseError::NoFrontmatter)?;
    let note = parse_note("", src)?;
    let Value::Mapping(old) = serde_yaml::from_str(frontmatter.yaml).map_err(ParseError::Yaml)?
    else {
        return Err(ParseError::NotAMapping.into());
    };

    let mut expected = old.clone();
    let mut yaml = frontmatter.yaml.to_string();
    if !holds_link(&note, key, is_target) {
        let link = format!("[[{target}]]");
        yaml = append_link(&yaml, key, &link, &note, &old)?;
        expected.insert(key.into(), with_link(old.get(key), &link));
    }
    if let Some(metadata) = stale_metadata(&note, &old)? {
        yaml = write_metadata(&yaml, &metadata, &old)?;
        set_metadata(&mut expected, metadata);
    }
    if yaml == frontmatter.yaml {
        return Ok(None);
    }
    verify(&yaml, &old, &expected, key)?;

    let (head, tail) = (
        &src[..frontmatter.yaml_start],
        &src[frontmatter.yaml_start + frontmatter.yaml.len()..],
    );
    Ok(Some(format!("{head}{yaml}{tail}")))
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
        return new_key(yaml, key, &quoted).ok_or_else(unsupported);
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
/// is where §3 shows relations.
fn new_key(yaml: &str, key: &str, quoted: &str) -> Option<String> {
    let all = blocks(yaml);
    let last = all
        .iter()
        .rfind(|b| !matches!(b.key.as_str(), "kbx" | QMD))?;
    let at = last.content_end(yaml);
    let eol = eol_at(yaml, at);
    Some(insert(yaml, at, &format!("{key}: [{quoted}]{eol}")))
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
/// than `type` and `tags` stay.
fn stale_metadata(note: &Note, old: &Mapping) -> Result<Option<Mapping>, WriteError> {
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
    let kept: Vec<_> = new.keys().filter(|k| old.contains_key(*k)).collect();
    if new != *expected || !kept.into_iter().eq(old.keys()) {
        return Err(unsupported());
    }
    Ok(())
}
