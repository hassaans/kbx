//! Running the built-in search engine for a vault. See `docs/search-engine.md`.

use std::error::Error;
use std::io::{self, IsTerminal, Write};

use kbx_core::{Graph, Project};
use kbx_gen::{
    BertEmbedder, EmbedError, Embedder, Hit, MODEL, Query, Search, SearchError, SearchIndex,
};
use tracing::warn;

/// The engine's index, next to the graph.
const SEARCH_INDEX: &str = "search-index.sqlite";

/// Stands in for an index a read-only run can't build.
struct Unbuilt;

impl Search for Unbuilt {
    fn search(&self, _: &Query<'_>, _: usize) -> Result<Vec<Hit>, SearchError> {
        let failure = "the search index isn't built yet; run `kbx suggest`";
        Err(SearchError::Failed(failure.to_string()))
    }
}

/// The engine, up to date with the graph unless `read_only`. A vault without
/// the model still gets keyword results: this only says so.
pub fn backend(
    project: &Project,
    graph: &Graph,
    searching: bool,
    read_only: bool,
) -> Result<Box<dyn Search>, Box<dyn Error>> {
    let path = project.graph.with_file_name(SEARCH_INDEX);
    if read_only {
        return Ok(match SearchIndex::open_read_only(&path) {
            Some(index) => Box::new(index.searcher(graph, None)?),
            None => Box::new(Unbuilt),
        });
    }
    if !searching {
        return Ok(Box::new(Unbuilt));
    }
    let index = SearchIndex::open(&path)?;
    let embedder = match index.needs_embedding(graph, &MODEL.id())? {
        true => load_model().map_or_else(say_keywords_only, Some),
        false => None,
    };
    let embedder_ref = embedder
        .as_ref()
        .map(|model| model.as_ref() as &dyn Embedder);
    let mut progress = Progress::new();
    let synced = index.sync(graph, &project.vault, embedder_ref, &mut |done, total| {
        progress.report(done, total);
    })?;
    progress.finish();
    if synced.unembedded > 0 {
        warn!("{} sections have no vector yet", synced.unembedded);
    }
    Ok(Box::new(index.searcher(graph, embedder)?))
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
