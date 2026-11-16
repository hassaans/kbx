//! Running the built-in search engine for a vault. See `docs/search-engine.md`.

use std::error::Error;
use std::io::{self, IsTerminal, Write};

use kbx_core::{Graph, Project};
use kbx_gen::{
    BertEmbedder, EmbedError, Embedder, Hit, MODEL, Query, Search, SearchError, SearchIndex,
};

use crate::embedding::{Backlog, Coverage, Embed, Engine};

/// The engine's index, next to the graph.
const SEARCH_INDEX: &str = "search-index.sqlite";

/// With Accelerate on an M2 Pro: 45,183 in about 10 minutes on the 10,000-note
/// vault, 7,097 in 111 s on the mirror. Without it, about half this.
const SECTIONS_PER_SECOND: f64 = 60.0;

const SECTIONS: &str = "sections";

/// Stands in for an index a read-only run can't build.
struct Unbuilt;

impl Search for Unbuilt {
    fn search(&self, _: &Query<'_>, _: usize) -> Result<Vec<Hit>, SearchError> {
        let failure = "the search index isn't built yet; run `kbx suggest`";
        Err(SearchError::Failed(failure.to_string()))
    }
}

/// The engine, up to date with the graph unless `read_only`, and how much of
/// its index has vectors. The keywords always are; `embed` decides about the
/// vectors. A vault without the model still gets keyword results: this only
/// says so.
pub fn backend(
    project: &Project,
    graph: &Graph,
    searching: bool,
    read_only: bool,
    embed: Embed,
) -> Result<Engine, Box<dyn Error>> {
    let path = project.graph.with_file_name(SEARCH_INDEX);
    if read_only {
        return Ok(match SearchIndex::open_read_only(&path) {
            Some(index) => {
                let coverage = coverage(&index)?;
                (Box::new(index.searcher(graph, None)?), Some(coverage))
            }
            None => (Box::new(Unbuilt), None),
        });
    }
    if !searching {
        return Ok((Box::new(Unbuilt), None));
    }
    let index = SearchIndex::open(&path)?;
    index.sync(graph, &project.vault, None, &mut |_, _| {})?;
    let before = coverage(&index)?;
    let backlog = Backlog {
        count: before.total - before.embedded,
        unit: SECTIONS,
        chunks: None,
        seconds: (before.total - before.embedded) as f64 / SECTIONS_PER_SECOND,
    };
    let embedding = backlog.count > 0 && embed.waits_for(Some(backlog.seconds));
    if backlog.count > 0 {
        backlog.announce(embedding, "`kbx suggest --embed wait`");
    }
    let embedder = match embedding {
        true => load_model().map_or_else(say_keywords_only, Some),
        false => None,
    };
    if let Some(model) = &embedder {
        let mut progress = Progress::new();
        index.sync(
            graph,
            &project.vault,
            Some(model.as_ref()),
            &mut |done, total| progress.report(done, total),
        )?;
        progress.finish();
    }
    let coverage = coverage(&index)?;
    Ok((Box::new(index.searcher(graph, embedder)?), Some(coverage)))
}

fn coverage(index: &SearchIndex) -> rusqlite::Result<Coverage> {
    let (embedded, total) = index.vectors(&MODEL.id())?;
    Ok(Coverage {
        embedded,
        total,
        unit: SECTIONS,
    })
}

/// Empties an index that the graph has no notes for any more, since what it
/// holds may be what was since excluded. Builds none.
pub fn forget(project: &Project, graph: &Graph) -> Result<(), Box<dyn Error>> {
    let path = project.graph.with_file_name(SEARCH_INDEX);
    if path.is_file() {
        SearchIndex::open(&path)?.sync(graph, &project.vault, None, &mut |_, _| {})?;
    }
    Ok(())
}

fn load_model() -> Result<Box<dyn Embedder>, EmbedError> {
    if !MODEL.is_present() {
        eprintln!(
            "downloading the embedding model ({}, {} MB), once",
            MODEL.repo, MODEL.megabytes
        );
    }
    let dir = MODEL.fetch()?;
    Ok(Box::new(BertEmbedder::load(&MODEL, &dir)?))
}

fn say_keywords_only(error: EmbedError) -> Option<Box<dyn Embedder>> {
    eprintln!("warning: searching by keyword alone, without vectors: {error}");
    eprintln!(
        "  `kbx suggest` tries again next time; see docs/search-engine.md to install the model by hand"
    );
    None
}

/// Says how far embedding has got, on a terminal, a line at a time.
struct Progress {
    shown: bool,
    terminal: bool,
}

impl Progress {
    fn new() -> Progress {
        Progress {
            shown: false,
            terminal: io::stderr().is_terminal(),
        }
    }

    fn report(&mut self, done: usize, total: usize) {
        if self.terminal {
            eprint!("\rembedding sections: {done} of {total}");
            let _ = io::stderr().flush();
            self.shown = true;
        }
    }

    fn finish(&self) {
        if self.shown {
            eprintln!();
        }
    }
}
