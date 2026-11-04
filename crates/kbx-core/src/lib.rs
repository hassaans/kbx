//! Vault parsing, ontology and graph storage for kbx.
//!
//! [`parse_note`] turns one markdown file into a [`Note`], [`Ontology::parse`]
//! reads `ontology.yaml`, and a [`Vault`] resolves links between notes and
//! validates them, producing [`Finding`]s. [`index_vault`] compiles a folder
//! of notes into a [`Graph`], the SQLite file described by a [`Project`], and
//! [`lint_vault`] checks one against the ontology. [`accept`], [`reject`],
//! [`defer`] and [`unreject`] record review decisions, a [`Queue`] takes them
//! one candidate at a time, and [`add_relation`] writes accepted relations to
//! frontmatter. This crate never touches a model and never prints.

mod config;
mod graph;
mod index;
mod link;
mod lint;
mod note;
mod ontology;
mod review;
mod section;
mod vault;
mod writer;

pub use config::{
    CONFIG_FILE, Config, ConfigError, EXAMPLE_ONTOLOGY, InitReport, Project, Qmd, Search, init,
};
pub use graph::{Graph, GraphStats, Neighbor, Node};
pub use index::{
    IndexError, IndexOptions, IndexReport, IndexState, MENTIONS, index_state, index_vault,
    is_note_path,
};
pub use link::Link;
pub use lint::lint_vault;
pub use note::{
    Frontmatter, Note, ParseError, Property, RelationEntry, parse_note, split_frontmatter,
};
pub use ontology::{
    ANY, Class, Direction, Judge, NONE, Ontology, OntologyError, Relation, RelationKey,
};
pub use review::{
    Accepted, Decision, Deferred, Item, Queue, Rejected, ReviewError, STRIKES, Unrejected, accept,
    cid, defer, excerpt, ordered, reject, suppressed_pairs, unreject,
};
pub use section::{Block, Section, section_key};
pub use vault::{Finding, FindingCode, Resolved, ResolvedVia, Resolver, Severity, Vault};
pub use writer::{WriteError, add_relation, write_atomic};
