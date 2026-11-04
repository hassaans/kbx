use std::path::PathBuf;

use serde_yaml::{Mapping, Value};

use crate::link::Link;
use crate::section::{Lines, Section, extract_sections};

/// A markdown file with its frontmatter and sections parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    /// The path the note was parsed from, normally relative to the vault.
    pub path: PathBuf,
    /// The ULID from `id`.
    pub id: String,
    /// 1-based line of the `id` key.
    pub id_line: usize,
    /// The `type` key, which names a class in the ontology.
    pub class: String,
    /// 1-based line of the `type` key.
    pub class_line: usize,
    /// The `title` key.
    pub title: String,
    /// The `aliases` key, which links can resolve through.
    pub aliases: Vec<String>,
    /// The `tags` key.
    pub tags: Vec<String>,
    /// Keys holding wikilinks, in file order. Whether a key is a known
    /// relation is for [`Vault::validate`](crate::Vault::validate) to say.
    pub relations: Vec<RelationEntry>,
    /// Every other key outside the tool namespaces, in file order.
    pub properties: Vec<Property>,
    /// The tool-owned `kbx:` namespace, as written. Left untyped so a bad
    /// hand edit of a staged proposal can't stop the note from compiling.
    pub kbx: Option<Value>,
    /// The tool-owned `qmd:` namespace, as written.
    pub qmd: Option<Value>,
    /// The sections of the body, in file order.
    pub sections: Vec<Section>,
}

impl Note {
    /// The entry for a relation key, if the note has one.
    pub fn relation(&self, key: &str) -> Option<&RelationEntry> {
        self.relations.iter().find(|entry| entry.key == key)
    }

    /// Every inline `[[link]]` in the body, in file order.
    pub fn mentions(&self) -> impl Iterator<Item = &Link> {
        self.sections.iter().flat_map(|section| &section.mentions)
    }
}

/// A frontmatter key whose value is a wikilink or a list of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationEntry {
    /// The frontmatter key.
    pub key: String,
    /// 1-based line of the key.
    pub line: usize,
    /// The links, in file order.
    pub links: Vec<Link>,
}

/// A frontmatter key that holds anything other than wikilinks.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The frontmatter key.
    pub key: String,
    /// 1-based line of the key.
    pub line: usize,
    /// The value, as written.
    pub value: Value,
}

/// The two halves of a file with frontmatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frontmatter<'a> {
    /// The text between the `---` fences.
    pub yaml: &'a str,
    /// Byte offset of the first byte of `yaml`.
    pub yaml_start: usize,
    /// Byte offset of the first byte after the closing fence's line.
    pub body_start: usize,
}

/// Why a file could not be read as a note.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The file doesn't open with `---`. Plain markdown is common in a vault,
    /// so callers will usually skip these rather than report them.
    #[error("no frontmatter")]
    NoFrontmatter,
    /// The file opens with `---` and never closes it.
    #[error("frontmatter has no closing `---`")]
    UnterminatedFrontmatter,
    /// The frontmatter is not valid YAML, or repeats a key.
    #[error("invalid frontmatter: {0}")]
    Yaml(#[from] serde_yaml::Error),
    /// The frontmatter is valid YAML but not a mapping.
    #[error("frontmatter is not a mapping")]
    NotAMapping,
    /// A key is a list or a map.
    #[error("frontmatter key is not a string: {0:?}")]
    NonStringKey(Value),
    /// `id`, `type` or `title` is absent or null.
    #[error("frontmatter is missing `{0}`")]
    MissingKey(&'static str),
    /// `id`, `type` or `title` is a list or a map.
    #[error("frontmatter key `{0}` must be a single value, not a list or map")]
    NotAScalar(&'static str),
    /// A key outside the tool namespaces holds a map, at any depth.
    #[error("frontmatter key `{0}` holds a nested map, which only `kbx:` and `qmd:` may")]
    NestedMap(String),
}

impl ParseError {
    /// 1-based line in the file, where the YAML parser gave one. The rest are
    /// about the frontmatter as a whole, so they point at its opening fence.
    pub fn line(&self) -> usize {
        match self {
            ParseError::Yaml(error) => error.location().map_or(1, |at| at.line()),
            _ => 1,
        }
    }
}

/// Finds the frontmatter block: a `---` first line, up to the next `---` line.
/// Returns `None` if the file doesn't open with one. Handles a BOM and CRLF.
pub fn split_frontmatter(src: &str) -> Result<Option<Frontmatter<'_>>, ParseError> {
    let start = if src.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let mut lines = src[start..].split_inclusive('\n');
    let Some(first) = lines.next().filter(|line| is_fence(line)) else {
        return Ok(None);
    };
    let yaml_start = start + first.len();
    let mut offset = yaml_start;
    for line in lines {
        if is_fence(line) {
            let yaml = &src[yaml_start..offset];
            return Ok(Some(Frontmatter {
                yaml,
                yaml_start,
                body_start: offset + line.len(),
            }));
        }
        offset += line.len();
    }
    Err(ParseError::UnterminatedFrontmatter)
}

fn is_fence(line: &str) -> bool {
    line.trim_end() == "---"
}

/// Parses one markdown file. `path` is stored on the note and is what
/// filename links match, so pass it relative to the vault root.
pub fn parse_note(path: impl Into<PathBuf>, src: &str) -> Result<Note, ParseError> {
    let frontmatter = split_frontmatter(src)?.ok_or(ParseError::NoFrontmatter)?;
    // The YAML starts on line 2, so a blank line makes its errors count lines
    // of the file.
    let mapping = match serde_yaml::from_str(&format!("\n{}", frontmatter.yaml))? {
        Value::Mapping(mapping) => mapping,
        Value::Null => Mapping::new(),
        _ => return Err(ParseError::NotAMapping),
    };

    let (mut id, mut class, mut title, mut kbx, mut qmd) = (None, None, None, None, None);
    let (mut aliases, mut tags) = (Vec::new(), Vec::new());
    let (mut relations, mut properties) = (Vec::new(), Vec::new());
    for (key, value) in mapping {
        let Value::String(key) = key else {
            return Err(ParseError::NonStringKey(key));
        };
        if !matches!(key.as_str(), "kbx" | "qmd") && holds_map(&value) {
            return Err(ParseError::NestedMap(key));
        }
        match key.as_str() {
            "id" => id = scalar(value, "id")?,
            "type" => class = scalar(value, "type")?,
            "title" => title = scalar(value, "title")?,
            "aliases" => aliases = string_list(value),
            "tags" => tags = string_list(value),
            "kbx" => kbx = Some(value),
            "qmd" => qmd = Some(value),
            _ => {
                let line = key_line(frontmatter.yaml, &key);
                match wikilinks(&value, line) {
                    Some(links) => relations.push(RelationEntry { key, line, links }),
                    None => properties.push(Property { key, line, value }),
                }
            }
        }
    }

    let id = id.ok_or(ParseError::MissingKey("id"))?;
    let sections = extract_sections(src, frontmatter.body_start, &id, &Lines::new(src));
    Ok(Note {
        path: path.into(),
        class: class.ok_or(ParseError::MissingKey("type"))?,
        id_line: key_line(frontmatter.yaml, "id"),
        class_line: key_line(frontmatter.yaml, "type"),
        title: title.ok_or(ParseError::MissingKey("title"))?,
        id,
        aliases,
        tags,
        relations,
        properties,
        kbx,
        qmd,
        sections,
    })
}

/// 1-based line of a top-level key. serde_yaml drops positions, so this looks
/// for the key at the start of a line. The YAML always starts on line 2.
fn key_line(yaml: &str, key: &str) -> usize {
    let is_key = |line: &str| {
        let line = line.trim_start_matches(['"', '\'']);
        line.strip_prefix(key).is_some_and(|rest| {
            rest.trim_start_matches(['"', '\''])
                .trim_start()
                .starts_with(':')
        })
    };
    yaml.lines().position(is_key).unwrap_or(0) + 2
}

fn holds_map(value: &Value) -> bool {
    match value {
        Value::Mapping(_) => true,
        Value::Sequence(items) => items.iter().any(holds_map),
        Value::Tagged(tagged) => holds_map(&tagged.value),
        _ => false,
    }
}

/// Reads a required single value. YAML types `title: 1984` as a number, but
/// it's text here. Null counts as missing.
fn scalar(value: Value, key: &'static str) -> Result<Option<String>, ParseError> {
    match value {
        Value::Null => Ok(None),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => {
            Err(ParseError::NotAScalar(key))
        }
        other => Ok(scalar_text(&other)),
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Obsidian accepts `aliases: one` as well as a list.
fn string_list(value: Value) -> Vec<String> {
    match value {
        Value::Sequence(items) => items.iter().filter_map(scalar_text).collect(),
        other => scalar_text(&other).into_iter().collect(),
    }
}

/// The links in a value that is one quoted wikilink or a non-empty list of
/// them. Anything else, including a list mixing links and text, is `None`.
fn wikilinks(value: &Value, line: usize) -> Option<Vec<Link>> {
    let parse = |item: &Value| item.as_str().and_then(|text| Link::parse(text, line));
    match value {
        Value::Sequence(items) if !items.is_empty() => items.iter().map(parse).collect(),
        Value::String(_) => parse(value).map(|link| vec![link]),
        _ => None,
    }
}
