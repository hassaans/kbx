//! Vault parsing, ontology and graph storage for kbx.
//!
//! [`parse_note`] turns one markdown file into a [`Note`], [`Ontology::parse`]
//! reads `ontology.yaml`, and a [`Vault`] resolves links between notes and
//! validates them, producing [`Finding`]s. [`index_vault`] compiles a folder
//! of notes into a [`Graph`], the SQLite file described by a [`Project`], and
//! [`lint_vault`] checks one against the ontology. This crate never touches a
//! model and never prints.

mod config;
mod graph;
mod index;
mod link;
mod lint;
mod note;
mod ontology;
mod section;
mod vault;

pub use config::{
    CONFIG_FILE, Config, ConfigError, EXAMPLE_ONTOLOGY, InitReport, Project, Qmd, Search, init,
};
pub use graph::{Graph, GraphStats, Neighbor, Node};
pub use index::{
    IndexError, IndexOptions, IndexReport, IndexState, MENTIONS, index_state, index_vault,
};
pub use link::Link;
pub use lint::lint_vault;
pub use note::{
    Frontmatter, Note, ParseError, Property, RelationEntry, parse_note, split_frontmatter,
};
pub use ontology::{
    ANY, Class, Direction, Judge, NONE, Ontology, OntologyError, Relation, RelationKey,
};
pub use section::{Block, Section, section_key};
pub use vault::{Finding, FindingCode, Resolved, ResolvedVia, Resolver, Severity, Vault};
