use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use uuid::Uuid;

use crate::link::Link;

/// A heading and the text up to the next heading of any level, so sections
/// never overlap. Text before the first heading is a section with an empty
/// `heading_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The stable key, see [`section_key`].
    pub key: Uuid,
    /// Heading texts from the outermost heading down to this one.
    pub heading_path: Vec<String>,
    /// `heading_path` as slugs joined by `/`, as in `{id}#{slug_path}`.
    pub slug_path: String,
    /// Counts earlier sections with the same `heading_path`, from 0.
    pub ordinal: u32,
    /// Byte offsets into the whole file, not just the body.
    pub byte_range: Range<usize>,
    /// 1-based first line.
    pub start_line: usize,
    /// 1-based last line, inclusive.
    pub end_line: usize,
    /// Blocks with a `^block-id`, addressable as `{id}#^{block-id}`.
    pub blocks: Vec<Block>,
    /// Inline `[[links]]`, indexed as untyped `mentions` edges.
    pub mentions: Vec<Link>,
}

impl Section {
    /// The block on the heading line itself, if the heading carries an id.
    pub fn heading_block(&self) -> Option<&Block> {
        if self.heading_path.is_empty() {
            return None;
        }
        let start = self.byte_range.start;
        self.blocks.iter().find(|b| b.byte_range.start == start)
    }
}

/// A block labelled with a `^block-id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The id, without the `^`.
    pub id: String,
    /// Byte offsets into the whole file.
    pub byte_range: Range<usize>,
    /// 1-based first line.
    pub start_line: usize,
    /// 1-based last line, inclusive.
    pub end_line: usize,
}

/// The file types Obsidian embeds that aren't notes.
const ATTACHMENT_EXTENSIONS: [&str; 19] = [
    "avif", "base", "bmp", "canvas", "flac", "gif", "jpeg", "jpg", "m4a", "mkv", "mov", "mp3",
    "mp4", "ogg", "pdf", "png", "svg", "wav", "webp",
];

static SECTION_NAMESPACE: LazyLock<Uuid> = LazyLock::new(|| {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"https://github.com/hassaans/kbx#section",
    )
});

/// `UUID5(id + heading path + ordinal)`, the stable section key from §3.
///
/// `ordinal` tells apart sections with the same heading path. It is not a
/// position in the note, so inserting a section leaves other keys alone.
pub fn section_key(note_id: &str, heading_path: &[String], ordinal: u32) -> Uuid {
    // The unit separator keeps ["a", "b"] and ["a b"] from colliding.
    let mut name = note_id.to_string();
    for heading in heading_path {
        name.push('\u{1f}');
        name.push_str(heading);
    }
    name.push('\u{1f}');
    name.push_str(&ordinal.to_string());
    Uuid::new_v5(&SECTION_NAMESPACE, name.as_bytes())
}

/// Maps byte offsets in a file to 1-based line numbers.
pub(crate) struct Lines(Vec<usize>);

impl Lines {
    pub(crate) fn new(src: &str) -> Self {
        let starts = src.match_indices('\n').map(|(i, _)| i + 1);
        Lines(std::iter::once(0).chain(starts).collect())
    }

    pub(crate) fn at(&self, offset: usize) -> usize {
        self.0.partition_point(|&start| start <= offset)
    }

    fn span(&self, range: &Range<usize>) -> (usize, usize) {
        (
            self.at(range.start),
            self.at(range.end.saturating_sub(1).max(range.start)),
        )
    }
}

/// Walks the body of `src`, which starts at `body_start`.
pub(crate) fn extract_sections(
    src: &str,
    body_start: usize,
    note_id: &str,
    lines: &Lines,
) -> Vec<Section> {
    let body = &src[body_start..];
    let block = |id: &str, range: Range<usize>| {
        let range = range.start + body_start..range.end + body_start;
        let (start_line, end_line) = lines.span(&range);
        Block {
            id: id.to_string(),
            byte_range: range,
            start_line,
            end_line,
        }
    };

    let mut sections = vec![empty_section(body_start)];
    let mut open_headings: Vec<(u8, String)> = Vec::new();
    let mut seen_paths: HashMap<Vec<String>, u32> = HashMap::new();
    let mut heading: Option<(u8, String)> = None;
    let mut depth = 0;
    // A tight list item has no paragraph, so its text ends at the first nested
    // list or at the end of the item.
    let mut open_item = None;
    let mut last_top_block: Option<Range<usize>> = None;

    let options = Options::ENABLE_TABLES | Options::ENABLE_WIKILINKS;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        let current = sections.len() - 1;
        match event {
            Event::Start(Tag::Heading { level, .. }) if depth == 0 => {
                sections[current].byte_range.end = range.start + body_start;
                sections.push(empty_section(range.start + body_start));
                heading = Some((level as u8, String::new()));
                last_top_block = None;
                depth += 1;
            }
            Event::End(TagEnd::Heading(_)) if depth == 1 => {
                depth -= 1;
                let Some((level, mut text)) = heading.take() else {
                    continue;
                };
                if let Some(id) = heading_block_id(&text, &body[range.clone()]) {
                    sections[current].blocks.push(block(id, range.clone()));
                    text.truncate(text.trim_end().len() - id.len() - 1);
                }
                while open_headings.last().is_some_and(|(open, _)| *open >= level) {
                    open_headings.pop();
                }
                open_headings.push((level, text.trim().to_string()));

                let path: Vec<String> = open_headings.iter().map(|(_, t)| t.clone()).collect();
                let count = seen_paths.entry(path.clone()).or_default();
                sections[current].ordinal = *count;
                *count += 1;
                sections[current].slug_path =
                    path.iter().map(|h| slug(h)).collect::<Vec<_>>().join("/");
                sections[current].heading_path = path;
            }
            Event::Start(tag) => {
                match &tag {
                    Tag::Item => open_item = Some(range.start),
                    Tag::List(_) => {
                        if let Some(start) = open_item.take()
                            && let Some(id) = trailing_block_id(&body[start..range.start])
                        {
                            sections[current].blocks.push(block(id, start..range.start));
                        }
                    }
                    Tag::Link {
                        link_type: LinkType::WikiLink { .. },
                        ..
                    } => {
                        let line = lines.at(range.start + body_start);
                        sections[current]
                            .mentions
                            .extend(Link::parse(&body[range], line));
                    }
                    Tag::Image {
                        link_type: LinkType::WikiLink { .. },
                        ..
                    } => {
                        let line = lines.at(range.start + body_start);
                        let link = Link::parse(body[range].trim_start_matches('!'), line);
                        sections[current]
                            .mentions
                            .extend(link.filter(embeds_a_note));
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::End(tag) => {
                depth -= 1;
                match tag {
                    TagEnd::Paragraph => {
                        open_item = None;
                        if let Some(id) = trailing_block_id(&body[range.clone()]) {
                            let alone = body[range.clone()].trim() == format!("^{id}");
                            let start = match &last_top_block {
                                Some(previous) if alone && depth == 0 => previous.start,
                                _ => range.start,
                            };
                            sections[current].blocks.push(block(id, start..range.end));
                        }
                    }
                    TagEnd::Item => {
                        if let Some(start) = open_item.take()
                            && let Some(id) = trailing_block_id(&body[start..range.end])
                        {
                            sections[current].blocks.push(block(id, start..range.end));
                        }
                    }
                    _ => {}
                }
                if depth == 0 {
                    last_top_block = Some(range);
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, heading_text)) = &mut heading {
                    heading_text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, heading_text)) = &mut heading {
                    heading_text.push(' ');
                }
            }
            _ => {}
        }
    }
    if let Some(last) = sections.last_mut() {
        last.byte_range.end = src.len();
    }

    sections.retain(|section| {
        !section.heading_path.is_empty() || !src[section.byte_range.clone()].trim().is_empty()
    });
    for section in &mut sections {
        section.key = section_key(note_id, &section.heading_path, section.ordinal);
        (section.start_line, section.end_line) = lines.span(&section.byte_range);
    }
    sections
}

fn empty_section(start: usize) -> Section {
    Section {
        key: Uuid::nil(),
        heading_path: Vec::new(),
        slug_path: String::new(),
        ordinal: 0,
        byte_range: start..start,
        start_line: 0,
        end_line: 0,
        blocks: Vec::new(),
        mentions: Vec::new(),
    }
}

/// `![[pic.png]]` embeds an attachment, which is not a note and can't resolve.
/// Only known types count: `![[OAuth 2.0]]` is a note with a dot in its name,
/// and an embed of an unlisted type surfaces as `link.unresolved`, not silently.
fn embeds_a_note(link: &Link) -> bool {
    let extension = link.target.rsplit_once('.').map(|(_, e)| e.to_lowercase());
    !extension.is_some_and(|e| ATTACHMENT_EXTENSIONS.contains(&e.as_str()))
}

/// The `^block-id` ending a heading. `text` has lost its markup, so `` `^x` ``
/// and `\\^x` look like ids there; `source` shows whether the marker is real,
/// once closing `#`s or a setext underline are gone.
fn heading_block_id<'a>(text: &'a str, source: &str) -> Option<&'a str> {
    let source = source.trim_end_matches(['#', '=', '-', ' ', '\t', '\r', '\n']);
    trailing_block_id(text).filter(|id| trailing_block_id(source) == Some(*id))
}

/// The `^block-id` ending `text`, without the `^`.
fn trailing_block_id(text: &str) -> Option<&str> {
    let token = text.trim_end().rsplit(char::is_whitespace).next()?;
    let id = token.strip_prefix('^')?;
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    valid.then_some(id)
}

/// Lowercases `heading` and joins its runs of letters and digits with `-`.
fn slug(heading: &str) -> String {
    let lower = heading.to_lowercase();
    let words = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty());
    words.collect::<Vec<_>>().join("-")
}
