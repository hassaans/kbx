//! Vault parsing, ontology and graph storage for kbx.
//!
//! [`parse_note`] turns one markdown file into a [`Note`], [`Ontology::parse`]
//! reads `ontology.yaml`, and a [`Vault`] resolves links between notes and
//! validates them, producing [`Finding`]s. [`index_vault`] compiles a folder
//! of notes into a [`Graph`], the SQLite file described by a [`Project`], and
//! [`lint_vault`] checks one against the ontology. [`accept`], [`reject`],
//! [`defer`] and [`unreject`] record review decisions, a [`Queue`] takes them
//! one candidate at a time, and [`add_relation`] writes accepted relations to
//! frontmatter. [`stage`] puts candidates under `kbx.proposed` for editors, and
//! [`sync_stage`] takes back what was decided there. [`Graph::edges`],
//! [`Graph::reviews`] and [`turtle`] export what the database holds. This crate
//! never touches a model and never prints.

mod config;
mod export;
mod graph;
mod index;
mod link;
mod lint;
mod note;
mod ontology;
mod path;
mod review;
mod section;
mod vault;
mod writer;

pub use config::{
    CONFIG_FILE, Config, ConfigError, EXAMPLE_ONTOLOGY, InitReport, JudgeBackend, JudgeConfig,
    Project, Qmd, Search, init,
};
pub use export::{
    DEFAULT_NAMESPACE, EdgeRow, ImportError, NOTE_NAMESPACE, Review, parse_reviews, turtle,
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
    ANY, Class, Direction, Judge, NONE, Ontology, OntologyError, Orientation, Relation, RelationKey,
};
pub use path::find_on_path;
pub use review::{
    Accepted, Decision, Deferred, Item, Queue, Rejected, ReviewError, STRIKES, StageReport, Synced,
    Unrejected, accept, cid, defer, excerpt, locked_notes, mark_reviewed, ordered, reject, stage,
    suppressed_pairs, sync_stage, unreject, unstage,
};
pub use section::{Block, Section, section_key};
pub use vault::{Finding, FindingCode, Resolved, ResolvedVia, Resolver, Severity, Vault};
pub use writer::{
    StagedEntry, WriteError, add_relation, proposed_ids, set_reviewed, stage_entries,
    unstage_entries, write_atomic,
};
