//! Link candidate generators for kbx.
//!
//! Each [`Generator`] proposes pairs of unlinked notes from one signal.
//! [`suggest`] runs them, unions their proposals on the pair, drops linked and
//! suppressed pairs and applies the [`Budget`]; [`store`] writes the result to
//! the `candidates` table. This crate never prints.

mod alias;
mod knn;
mod pipeline;
mod schema;
mod search;
mod snapshot;
mod structure;

use std::path::Path;

use kbx_core::{Graph, Ontology};
use serde_json::Value;

pub use alias::AliasCooccur;
pub use kbx_core::cid;
pub use knn::QmdKnn;
pub use pipeline::{
    Budget, Proposal, Run, UnknownGenerator, estimate_tokens, generators, store, suggest,
};
pub use schema::SchemaGap;
pub use search::{Hit, QmdSearch, Query, Search, SearchError};
pub use structure::{AdamicAdar, CommonNeighbors, TwoHopBridge};

/// One signal that two notes should be linked.
///
/// `run` returns a `Result`, unlike the §5 sketch, so that a broken search
/// backend is reported rather than looking like a vault with no candidates.
pub trait Generator {
    /// The name used in `--generators` and in the `generators` column.
    fn name(&self) -> &'static str;
    /// Proposes each pair at most once in each direction, and no pair that is
    /// already linked. A generator whose direction means something says so
    /// with a `src` in `features`, because the union may keep the other one.
    fn run(&self, g: &Graph, scope: &Scope<'_>) -> Result<Vec<Candidate>, GenError>;
}

/// What a run may look at.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    /// The vault root, for note bodies.
    pub vault: &'a Path,
    /// The ontology the index was built from.
    pub ontology: &'a Ontology,
    /// Keep only pairs touching this note.
    pub note: Option<&'a str>,
}

/// A pair proposed by one generator.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The note a relation would be written to.
    pub src: String,
    /// The note it would point at.
    pub dst: String,
    /// The raw score, on the generator's own scale. Never negative.
    pub score: f64,
    /// Why, as a JSON object. The union adds `score` to it.
    pub features: Value,
}

/// Why a generator could not run.
#[derive(Debug, thiserror::Error)]
pub enum GenError {
    /// The database failed.
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
    /// The search backend failed.
    #[error(transparent)]
    Search(#[from] SearchError),
}
