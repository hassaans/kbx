//! How much a search index has left to embed, and whether a run waits for it.
//! See `docs/search-engine.md`.

use std::env;

use clap::ValueEnum;
use kbx_gen::Search;

/// The longest embed `auto` waits for. Long enough for a first run on the
/// 10,000-note vault with the built-in engine, and far from `~/HQ`'s 3.6 hours.
const AUTO_MINUTES: f64 = 15.0;

/// Replaces [`AUTO_MINUTES`].
const AUTO_MINUTES_VAR: &str = "KBX_EMBED_AUTO_MINUTES";

/// A search backend, and how much of its index has vectors, if it says.
pub type Engine = (Box<dyn Search>, Option<Coverage>);

/// What a run does about what has no vector yet.
#[derive(Clone, Copy, ValueEnum)]
pub enum Embed {
    /// Embed if that looks like a short wait, else go on without it.
    Auto,
    /// Embed everything first, however long it takes.
    Wait,
    /// Embed nothing: search the vectors there are, and keywords.
    Skip,
}

impl Embed {
    /// Whether to embed, given how long it should take, if that's known. An
    /// embed of unknown size runs, as it did before there were estimates.
    pub fn waits_for(self, seconds: Option<f64>) -> bool {
        match self {
            Embed::Auto => seconds.is_none_or(|seconds| seconds <= auto_seconds()),
            Embed::Wait => true,
            Embed::Skip => false,
        }
    }
}

fn auto_seconds() -> f64 {
    let minutes = env::var(AUTO_MINUTES_VAR).ok();
    let minutes = minutes.and_then(|minutes| minutes.parse().ok());
    minutes.unwrap_or(AUTO_MINUTES) * 60.0
}

/// What has no vector yet.
pub struct Backlog {
    pub count: usize,
    /// What `count` counts, in the plural.
    pub unit: &'static str,
    /// The chunks that come to, for an engine that splits its units up.
    pub chunks: Option<usize>,
    pub seconds: f64,
}

impl Backlog {
    /// Says what's about to happen, before it does. `by_hand` is how to embed
    /// in full later.
    pub fn announce(&self, embedding: bool, by_hand: &str) {
        let Backlog { count, unit, .. } = self;
        let size = match self.chunks {
            Some(chunks) => format!(" (about {chunks} chunks)"),
            None => String::new(),
        };
        let time = duration(self.seconds);
        if embedding {
            eprintln!("note: embedding {count} {unit}{size}, {time}; `--embed skip` goes without");
        } else {
            eprintln!(
                "note: {count} {unit}{size} have no vector yet, {time} to embed; going on without"
            );
            eprintln!("  to embed them, run {by_hand}");
        }
    }
}

/// `seconds` in the largest unit that reads well. It's an estimate, so no more
/// than that.
fn duration(seconds: f64) -> String {
    match seconds {
        ..60.0 => "under a minute".to_string(),
        ..3600.0 => format!("about {:.0} minutes", (seconds / 60.0).max(2.0)),
        _ => format!("about {:.1} hours", seconds / 3600.0),
    }
}

/// How much of an index had vectors when it was searched.
pub struct Coverage {
    pub embedded: usize,
    pub total: usize,
    /// What's counted, in the plural.
    pub unit: &'static str,
}

impl Coverage {
    pub fn print(&self) {
        let Coverage {
            embedded,
            total,
            unit,
        } = self;
        if embedded >= total {
            println!("vectors: {embedded} of {total} {unit}");
        } else {
            let percent = 100 * embedded / total.max(&1);
            println!(
                "vectors: {embedded} of {total} {unit} ({percent}%); the rest can be found by keyword alone"
            );
        }
    }
}
