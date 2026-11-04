/// A `[[wikilink]]` from a frontmatter relation or a note body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// What the link points at: an id, `{id}-slug`, a filename or an alias.
    pub target: String,
    /// The part after `#`: a heading, or `^block-id`.
    pub fragment: Option<String>,
    /// The part after `|`.
    pub display: Option<String>,
    /// 1-based line in the file. For a frontmatter link, the line of its key.
    pub line: usize,
}

impl Link {
    /// Parses `[[target#fragment|display]]`. Returns `None` for anything else,
    /// including same-note links (`[[#heading]]`), which have no target.
    pub(crate) fn parse(text: &str, line: usize) -> Option<Link> {
        let inner = text.trim().strip_prefix("[[")?.strip_suffix("]]")?;
        if inner.contains("[[") || inner.contains("]]") {
            return None;
        }
        let (reference, display) = match inner.split_once('|') {
            // Inside a markdown table the pipe has to be written `\|`.
            Some((reference, display)) => (reference.trim_end_matches('\\'), Some(display)),
            None => (inner, None),
        };
        let (target, fragment) = match reference.split_once('#') {
            Some((target, fragment)) => (target, Some(fragment)),
            None => (reference, None),
        };
        let target = target.trim();
        (!target.is_empty()).then(|| Link {
            target: target.to_string(),
            fragment: fragment.map(|f| f.trim().to_string()),
            display: display.map(|d| d.trim().to_string()),
            line,
        })
    }
}
