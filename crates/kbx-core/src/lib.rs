//! Vault parsing, ontology and graph storage for kbx.
//!
//! [`parse_note`] turns one markdown file into a [`Note`], [`Ontology::parse`]
//! reads `ontology.yaml`, and a [`Vault`] resolves links between notes and
//! validates them, producing [`Finding`]s. This crate never touches a model
//! and leaves walking the filesystem to its callers.

mod link;
mod note;
mod ontology;
mod section;
mod vault;

pub use link::Link;
pub use note::{
    Frontmatter, Note, ParseError, Property, RelationEntry, parse_note, split_frontmatter,
};
pub use ontology::{
    ANY, Class, Direction, Judge, NONE, Ontology, OntologyError, Relation, RelationKey,
};
pub use section::{Block, Section, section_key};
pub use vault::{Finding, FindingCode, Resolved, ResolvedVia, Vault};
