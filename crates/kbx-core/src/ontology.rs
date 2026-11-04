use std::collections::BTreeMap;

use serde::Deserialize;

/// In a `domain` or `range`, matches every class.
pub const ANY: &str = "any";

/// In `judge.vocabulary`, the verdict that no relation applies.
pub const NONE: &str = "none";

/// Frontmatter keys that are never relations.
const RESERVED_KEYS: [&str; 7] = ["id", "type", "title", "aliases", "tags", "kbx", "qmd"];

const SUPPORTED_VERSION: u32 = 1;

/// A parsed and cross-checked `ontology.yaml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ontology {
    /// The schema version, currently always 1.
    pub version: u32,
    /// Note types, by name.
    pub classes: BTreeMap<String, Class>,
    /// Relations, by name. Inverses are not listed: see [`Ontology::lookup`].
    pub relations: BTreeMap<String, Relation>,
    /// How the judge is constrained.
    pub judge: Judge,
}

/// A note type.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Class {
    /// What the class is for.
    pub description: String,
    /// Relation keys a note of this class must fill.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// A typed link between notes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relation {
    /// The key written on the target note. Equal to the relation's own name
    /// when symmetric.
    pub inverse: String,
    /// Classes the source may have, or [`ANY`].
    pub domain: Vec<String>,
    /// Classes the target may have, or [`ANY`].
    pub range: Vec<String>,
    /// How strongly this relation counts in graph scoring.
    pub weight: f64,
    /// A chain of this relation implies it end to end.
    #[serde(default)]
    pub transitive: bool,
    /// The relation reads the same in both directions.
    #[serde(default)]
    pub symmetric: bool,
    /// At most one target per note.
    #[serde(default)]
    pub exclusive: bool,
    /// The inverse lives only in the index: accepting the relation writes
    /// nothing to the target note, and `kbx index` derives the mirror edge.
    #[serde(default)]
    pub derived_only: bool,
}

/// How the judge is constrained.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judge {
    /// Relation names the judge may answer with, plus [`NONE`].
    pub vocabulary: Vec<String>,
    /// The direction rule.
    pub direction: Direction,
}

/// Who decides which way a judged relation points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// `domain` and `range` decide, never the model.
    FromSchema,
}

/// Which way a relation runs across a pair of notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// From the first note to the second.
    Forward,
    /// From the second note to the first.
    Reverse,
}

/// What a frontmatter key means: a relation, read forwards or as its inverse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelationKey<'a> {
    /// The relation's name in `relations`, not its inverse's.
    pub name: &'a str,
    /// The relation itself.
    pub relation: &'a Relation,
    /// The key was the relation's `inverse`, so source and target swap.
    pub inverted: bool,
}

impl<'a> RelationKey<'a> {
    /// Classes the note holding this key may have.
    pub fn domain(&self) -> &'a [String] {
        if self.inverted {
            &self.relation.range
        } else {
            &self.relation.domain
        }
    }

    /// Classes the linked notes may have.
    pub fn range(&self) -> &'a [String] {
        if self.inverted {
            &self.relation.domain
        } else {
            &self.relation.range
        }
    }

    /// Whether this key may hold only one target. The inverse of an exclusive
    /// relation may hold many: one decision can be superseded by several.
    pub fn exclusive(&self) -> bool {
        self.relation.exclusive && !self.inverted
    }
}

/// Why an `ontology.yaml` was refused.
#[derive(Debug, thiserror::Error)]
pub enum OntologyError {
    /// The file is not valid YAML, has an unknown key, or lacks a required one.
    #[error("invalid ontology: {0}")]
    Yaml(#[from] serde_yaml::Error),
    /// `version` is not one this build reads.
    #[error("unsupported ontology version {0}, expected {SUPPORTED_VERSION}")]
    UnsupportedVersion(u32),
    /// Something the file names does not exist or contradicts itself.
    #[error("invalid ontology: {0}")]
    Invalid(String),
}

impl Ontology {
    /// Parses `ontology.yaml` and checks that everything it names exists.
    pub fn parse(yaml: &str) -> Result<Ontology, OntologyError> {
        let ontology: Ontology = serde_yaml::from_str(yaml)?;
        if ontology.version != SUPPORTED_VERSION {
            return Err(OntologyError::UnsupportedVersion(ontology.version));
        }
        ontology.check().map_err(OntologyError::Invalid)?;
        Ok(ontology)
    }

    /// Finds the relation behind a frontmatter key, which may be an inverse.
    pub fn lookup(&self, key: &str) -> Option<RelationKey<'_>> {
        if let Some((name, relation)) = self.relations.get_key_value(key) {
            return Some(RelationKey {
                name,
                relation,
                inverted: false,
            });
        }
        let (name, relation) = self.relations.iter().find(|(_, r)| r.inverse == key)?;
        Some(RelationKey {
            name,
            relation,
            inverted: true,
        })
    }

    /// Which way `rel` runs between a note of class `a` and one of class `b`,
    /// from its `domain` and `range`. `None` if it fits neither way, or isn't a
    /// relation. When it fits both ways, the pair keeps its order.
    pub fn orient(&self, rel: &str, a: &str, b: &str) -> Option<Orientation> {
        let relation = self.relations.get(rel)?;
        let fits = |from, to| admits(&relation.domain, from) && admits(&relation.range, to);
        if fits(a, b) {
            Some(Orientation::Forward)
        } else if fits(b, a) {
            Some(Orientation::Reverse)
        } else {
            None
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.classes.contains_key(ANY) {
            return Err(format!("class name `{ANY}` is reserved"));
        }
        for (name, class) in &self.classes {
            if let Some(key) = class.requires.iter().find(|key| self.lookup(key).is_none()) {
                return Err(format!("class {name} requires unknown relation `{key}`"));
            }
        }
        for (name, relation) in &self.relations {
            self.check_relation(name, relation)?;
        }
        for word in &self.judge.vocabulary {
            if word != NONE && !self.relations.contains_key(word) {
                return Err(format!("judge vocabulary has unknown relation `{word}`"));
            }
        }
        Ok(())
    }

    fn check_relation(&self, name: &str, relation: &Relation) -> Result<(), String> {
        for key in [name, &relation.inverse] {
            if key == NONE || RESERVED_KEYS.contains(&key) {
                return Err(format!("relation {name} uses the reserved name `{key}`"));
            }
        }
        for class in relation.domain.iter().chain(&relation.range) {
            if class != ANY && !self.classes.contains_key(class) {
                return Err(format!("relation {name} names unknown class `{class}`"));
            }
        }
        if relation.symmetric && relation.inverse != name {
            return Err(format!("symmetric relation {name} must be its own inverse"));
        }
        let inverse = &relation.inverse;
        if inverse != name && self.relations.contains_key(inverse) {
            return Err(format!(
                "relation {name} has inverse `{inverse}`, also a relation"
            ));
        }
        let claims = self
            .relations
            .values()
            .filter(|r| r.inverse == *inverse)
            .count();
        if claims > 1 {
            return Err(format!(
                "`{inverse}` is the inverse of more than one relation"
            ));
        }
        Ok(())
    }
}

/// Whether `class` is in a `domain` or `range` list.
pub(crate) fn admits(classes: &[String], class: &str) -> bool {
    classes.iter().any(|c| c == ANY || c == class)
}
