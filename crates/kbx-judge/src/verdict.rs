use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use kbx_core::{Graph, NONE};
use rusqlite::{Transaction, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::Value;

use crate::{CandidateView, JudgeContext, JudgeId};

/// What a judge decided about a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The relation holds.
    Accept,
    /// No relation holds.
    Reject,
    /// For a person to decide.
    Unsure,
}

impl Decision {
    /// The value of `verdicts.verdict`.
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::Reject => "reject",
            Decision::Unsure => "unsure",
        }
    }
}

/// A judge's answer on one pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// The candidate.
    pub cid: String,
    /// The decision.
    pub decision: Decision,
    /// A word from the ontology's vocabulary. It says nothing about direction:
    /// [`kbx_core::Ontology::orient`] decides that.
    pub rel: String,
    /// From 0 to 1. Stored, but never a reason to accept anything (§6.3).
    pub confidence: f64,
    /// The judge's reasoning.
    pub rationale: String,
    /// This pair's share of the batch's cost, so that the rows sum to it.
    /// `None` when the backend doesn't report one.
    pub cost_usd: Option<f64>,
    /// This pair's share of the batch's cached input tokens, likewise.
    pub cached_tokens: Option<i64>,
}

/// What a call cost, as the backend reported it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Spend {
    pub cost_usd: Option<f64>,
    pub cached_tokens: Option<i64>,
}

#[derive(Deserialize)]
struct Answer {
    verdicts: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    cid: String,
    rationale: String,
    verdict: Decision,
    rel: String,
    confidence: f64,
}

/// Checks a schema-shaped answer against the batch it answers. Every pair must
/// be there exactly once, because a pair silently left out would look judged.
/// The reason never quotes the answer.
pub(crate) fn read_answer(
    answer: Value,
    batch: &[CandidateView],
    ctx: &JudgeContext,
    spend: Spend,
) -> Result<Vec<Verdict>, String> {
    let answer: Answer = serde_json::from_value(answer)
        .map_err(|_| "the answer doesn't match the schema".to_string())?;
    if answer.verdicts.len() != batch.len() {
        return Err(format!(
            "{} verdicts for {} pairs",
            answer.verdicts.len(),
            batch.len()
        ));
    }
    let mut pairs: HashMap<&str, &CandidateView> =
        batch.iter().map(|view| (view.cid.as_str(), view)).collect();

    let count = batch.len() as i64;
    let cached = |index: i64| {
        spend
            .cached_tokens
            .map(|total| total / count + i64::from(index < total % count))
    };
    let mut verdicts = Vec::with_capacity(batch.len());
    for (index, item) in answer.verdicts.into_iter().enumerate() {
        let Some(view) = pairs.remove(item.cid.as_str()) else {
            return Err("a verdict for a pair that wasn't asked about, or a repeat".to_string());
        };
        if !ctx.ontology.judge.vocabulary.contains(&item.rel) {
            return Err("a `rel` outside the vocabulary".to_string());
        }
        verdicts.push(Verdict {
            decision: settle(item.verdict, &item.rel, view, ctx),
            cid: item.cid,
            rel: item.rel,
            confidence: item.confidence.clamp(0.0, 1.0),
            rationale: item.rationale,
            cost_usd: spend.cost_usd.map(|total| total / count as f64),
            cached_tokens: cached(index as i64),
        });
    }
    Ok(verdicts)
}

/// An accepted relation that can't be written, being `none` or fitting the
/// pair's classes neither way, is for a person to look at.
fn settle(decision: Decision, rel: &str, view: &CandidateView, ctx: &JudgeContext) -> Decision {
    let writable = rel != NONE
        && ctx
            .ontology
            .orient(rel, &view.src.class, &view.dst.class)
            .is_some();
    if decision == Decision::Accept && !writable {
        Decision::Unsure
    } else {
        decision
    }
}

/// Writes a batch's verdicts, in one transaction.
pub(crate) fn store(
    g: &Graph,
    backend: JudgeId,
    model: &str,
    prompt_hash: &str,
    verdicts: &[Verdict],
) -> rusqlite::Result<()> {
    let connection = g.connection();
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    for verdict in verdicts {
        tx.execute(
            "INSERT OR REPLACE INTO verdicts (cid, backend, model, prompt_hash, verdict, rel,
                 confidence, rationale, cost_usd, cached_tokens, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                verdict.cid,
                backend.as_str(),
                model,
                prompt_hash,
                verdict.decision.as_str(),
                verdict.rel,
                verdict.confidence,
                verdict.rationale,
                verdict.cost_usd,
                verdict.cached_tokens,
                now
            ],
        )?;
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoteView;
    use kbx_core::{EXAMPLE_ONTOLOGY, Ontology};
    use serde_json::json;

    fn view(cid: &str, src: &str, dst: &str) -> CandidateView {
        let note = |class: &str| NoteView {
            id: format!("{class}-id"),
            class: class.to_string(),
            title: "t".to_string(),
            excerpt: "e".to_string(),
        };
        CandidateView {
            cid: cid.to_string(),
            src: note(src),
            dst: note(dst),
            why_proposed: Vec::new(),
        }
    }

    fn ctx() -> JudgeContext {
        JudgeContext::new(Ontology::parse(EXAMPLE_ONTOLOGY).unwrap(), EXAMPLE_ONTOLOGY)
    }

    fn item(cid: &str, verdict: &str, rel: &str, confidence: f64) -> Value {
        json!({"cid": cid, "rationale": "r", "verdict": verdict, "rel": rel, "confidence": confidence})
    }

    fn read(items: Vec<Value>, batch: &[CandidateView]) -> Result<Vec<Verdict>, String> {
        read_answer(
            json!({ "verdicts": items }),
            batch,
            &ctx(),
            Spend::default(),
        )
    }

    #[test]
    fn the_spend_is_shared_out_so_that_the_rows_sum_to_it() {
        let batch = [
            view("a", "Concept", "Concept"),
            view("b", "Concept", "Concept"),
            view("c", "Concept", "Concept"),
        ];
        let items = ["a", "b", "c"].map(|cid| item(cid, "reject", "none", 0.5));
        let spend = Spend {
            cost_usd: Some(0.03),
            cached_tokens: Some(100),
        };
        let answer = json!({ "verdicts": items });
        let verdicts = read_answer(answer, &batch, &ctx(), spend).unwrap();
        let cached: Vec<_> = verdicts.iter().map(|v| v.cached_tokens.unwrap()).collect();
        assert_eq!(cached, [34, 33, 33]);
        let cost: f64 = verdicts.iter().map(|v| v.cost_usd.unwrap()).sum();
        assert!((cost - 0.03).abs() < 1e-9);
    }

    #[test]
    fn a_backend_with_no_figures_stores_none_rather_than_zero() {
        let verdicts = read(
            vec![item("a", "reject", "none", 0.5)],
            &[view("a", "Concept", "Concept")],
        )
        .unwrap();
        assert_eq!(verdicts[0].cost_usd, None);
        assert_eq!(verdicts[0].cached_tokens, None);
    }

    #[test]
    fn confidence_is_clamped() {
        let batch = [view("a", "Concept", "Concept")];
        let verdicts = read(vec![item("a", "reject", "none", 7.0)], &batch).unwrap();
        assert_eq!(verdicts[0].confidence, 1.0);
    }

    #[test]
    fn a_relation_that_only_fits_reversed_is_still_accepted() {
        // `depends_on` runs from a Procedure, which here is the `dst`.
        let batch = [view("a", "Concept", "Procedure")];
        let verdicts = read(vec![item("a", "accept", "depends_on", 0.9)], &batch).unwrap();
        assert_eq!(verdicts[0].decision, Decision::Accept);
    }

    #[test]
    fn an_accepted_relation_that_fits_neither_way_goes_to_a_person() {
        let batch = [view("a", "Concept", "Concept")];
        let verdicts = read(vec![item("a", "accept", "supersedes", 0.9)], &batch).unwrap();
        assert_eq!(verdicts[0].decision, Decision::Unsure);
        assert_eq!(verdicts[0].rel, "supersedes");
    }

    #[test]
    fn accepting_no_relation_goes_to_a_person() {
        let batch = [view("a", "Concept", "Concept")];
        let verdicts = read(vec![item("a", "accept", "none", 0.9)], &batch).unwrap();
        assert_eq!(verdicts[0].decision, Decision::Unsure);
    }

    #[test]
    fn answers_that_dont_match_the_batch_are_refused_without_quoting_them() {
        let batch = [
            view("a", "Concept", "Concept"),
            view("b", "Concept", "Concept"),
        ];
        let good = |cid| item(cid, "reject", "none", 0.5);
        let bad = [
            vec![good("a")],
            vec![good("a"), good("a")],
            vec![good("a"), good("SECRET")],
            vec![good("a"), item("b", "reject", "SECRET", 0.5)],
            vec![good("a"), item("b", "SECRET", "none", 0.5)],
            vec![good("a"), json!({"cid": "b"})],
        ];
        for items in bad {
            let error = read(items, &batch).unwrap_err();
            assert!(!error.contains("SECRET"), "{error}");
        }
    }
}
