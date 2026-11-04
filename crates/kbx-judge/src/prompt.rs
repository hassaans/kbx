use std::fmt::Write;

use kbx_core::Ontology;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// `prompts/judge.system.md`. A copy, because `cargo publish` packages only the
/// crate.
pub const SYSTEM_PROMPT: &str = include_str!("../assets/judge.system.md");

/// `schema/judge.schema.json`, likewise.
pub const SCHEMA: &str = include_str!("../assets/judge.schema.json");

/// What every batch of a run shares.
#[derive(Debug, Clone)]
pub struct JudgeContext {
    /// The ontology the verdicts are checked against.
    pub ontology: Ontology,
    /// The stable prefix: [`SYSTEM_PROMPT`] byte for byte, then the ontology.
    /// The same across calls, so the provider's prefix cache can serve it.
    pub system: String,
    /// [`SCHEMA`] on one line, with the ontology's vocabulary as `rel`'s enum.
    pub schema: String,
    /// See [`prompt_hash`].
    pub prompt_hash: String,
}

impl JudgeContext {
    /// `yaml` is the text `ontology` was parsed from.
    pub fn new(ontology: Ontology, yaml: &str) -> JudgeContext {
        JudgeContext {
            system: format!("{SYSTEM_PROMPT}\n{}", render_ontology(&ontology)),
            schema: schema_for(&ontology.judge.vocabulary),
            prompt_hash: prompt_hash(yaml.as_bytes()),
            ontology,
        }
    }
}

/// The SHA-256, in hex, of [`SYSTEM_PROMPT`], [`SCHEMA`] and the ontology file.
/// A verdict stands until one of those changes.
pub fn prompt_hash(ontology: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SYSTEM_PROMPT);
    hasher.update(SCHEMA);
    hasher.update(ontology);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The vault's classes and relations, for the end of the stable prefix.
fn render_ontology(ontology: &Ontology) -> String {
    let mut text = String::from("## Ontology\n\nClasses:\n");
    for (name, class) in &ontology.classes {
        let _ = writeln!(text, "- {name}: {}", class.description);
    }
    text.push_str("\nRelations, as source -> target:\n");
    for word in &ontology.judge.vocabulary {
        if let Some(relation) = ontology.relations.get(word) {
            let (domain, range) = (relation.domain.join(", "), relation.range.join(", "));
            let _ = writeln!(text, "- {word}: {domain} -> {range}");
        }
    }
    let vocabulary = ontology.judge.vocabulary.join(", ");
    let _ = writeln!(text, "\nThe vocabulary for `rel`: {vocabulary}.");
    text
}

/// Edits [`SCHEMA`] as text, because a `serde_json` value sorts its keys, and
/// models fill in fields in schema order: `rationale` has to come before
/// `confidence`. One line, because `.cmd` shims can't take a newline.
fn schema_for(vocabulary: &[String]) -> String {
    let compact: String = SCHEMA.lines().map(str::trim).collect();
    let rel = compact.find("\"rel\"").expect("the schema has `rel`");
    let start = rel + compact[rel..].find('[').expect("`rel` has an enum");
    let end = start + compact[start..].find(']').expect("the enum closes");
    let words = Value::from(vocabulary.to_vec());
    format!("{}{words}{}", &compact[..start], &compact[end + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use kbx_core::EXAMPLE_ONTOLOGY;

    fn context(yaml: &str) -> JudgeContext {
        JudgeContext::new(Ontology::parse(yaml).unwrap(), yaml)
    }

    #[test]
    fn the_embedded_files_match_the_published_ones() {
        assert_eq!(
            SYSTEM_PROMPT,
            include_str!("../../../prompts/judge.system.md")
        );
        assert_eq!(SCHEMA, include_str!("../../../schema/judge.schema.json"));
    }

    #[test]
    fn the_prefix_opens_with_the_system_prompt_byte_for_byte() {
        let ctx = context(EXAMPLE_ONTOLOGY);
        assert!(ctx.system.starts_with(SYSTEM_PROMPT));
        assert!(ctx.system.contains("- Concept: A definable idea or term."));
        assert!(
            ctx.system
                .contains("- depends_on: Procedure -> Concept, Procedure, Entity")
        );
    }

    #[test]
    fn the_schema_is_one_line_of_json_with_the_rationale_before_the_confidence() {
        let ctx = context(EXAMPLE_ONTOLOGY);
        assert!(!ctx.schema.contains('\n'));
        let schema: Value = serde_json::from_str(&ctx.schema).unwrap();
        let published: Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(schema, published);
        let (rationale, confidence) = (
            ctx.schema.find("\"rationale\":").unwrap(),
            ctx.schema.find("\"confidence\":").unwrap(),
        );
        assert!(rationale < confidence);
    }

    #[test]
    fn the_schema_offers_the_vaults_own_vocabulary() {
        let yaml = EXAMPLE_ONTOLOGY.replace(
            "vocabulary: [part_of, depends_on, implements, supersedes, contradicts, example_of, see_also, none]",
            "vocabulary: [part_of, none]",
        );
        let ctx = context(&yaml);
        let schema: Value = serde_json::from_str(&ctx.schema).unwrap();
        let rel = &schema["properties"]["verdicts"]["items"]["properties"]["rel"];
        assert_eq!(rel["enum"], serde_json::json!(["part_of", "none"]));
    }

    #[test]
    fn the_hash_is_of_the_prompt_the_schema_and_the_ontology() {
        let bytes = [SYSTEM_PROMPT, SCHEMA, EXAMPLE_ONTOLOGY].concat();
        let digest = Sha256::digest(bytes);
        let expected: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(prompt_hash(EXAMPLE_ONTOLOGY.as_bytes()), expected);
        assert_ne!(prompt_hash(b"other"), expected);
    }
}
