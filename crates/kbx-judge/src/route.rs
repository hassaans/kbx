use std::collections::BTreeMap;
use std::path::Path;

use kbx_core::Graph;
use kbx_gen::Proposal;

use crate::batch::pinned_views;
use crate::{Availability, CandidateView, Judge, JudgeId, first_ready, views};

/// A backend and the pairs it may see.
pub struct Assignment {
    /// The backend.
    pub judge: Box<dyn Judge>,
    /// Its pairs.
    pub views: Vec<CandidateView>,
}

/// What [`route`] came to.
#[derive(Default)]
pub struct Routes {
    /// Who judges what. No pair is in two of these.
    pub assignments: Vec<Assignment>,
    /// The backend chosen for pairs with no pin, if one was ready.
    pub chosen: Option<JudgeId>,
    /// What was found of the backends that can't be used.
    pub passed_over: Vec<(JudgeId, Availability)>,
    /// Pairs kept from [`Routes::chosen`] by `[judge.overrides]`.
    pub pinned: usize,
    /// Pairs left out because a note is gone or can't be read.
    pub unreadable: usize,
}

/// Shares `candidates` out. Pairs with no pin go to the first of `general` that
/// is ready. A pair with a note pinned to a backend in `pinned` goes to that
/// backend or to nobody: if it's down the pair waits, and a pair pinned two
/// ways is never judged. Checking on a backend shows it no pair.
pub async fn route(
    g: &Graph,
    vault: &Path,
    candidates: &[Proposal],
    overrides: &BTreeMap<String, String>,
    general: Vec<Box<dyn Judge>>,
    pinned: Vec<Box<dyn Judge>>,
) -> rusqlite::Result<Routes> {
    let (chosen, passed_over) = first_ready(general).await;
    let mut routes = Routes {
        passed_over,
        ..Routes::default()
    };
    if let Some(judge) = chosen {
        let found = views(g, vault, candidates, overrides, judge.id())?;
        routes.chosen = Some(judge.id());
        routes.pinned = found.pinned;
        routes.unreadable = found.unreadable;
        routes.assignments.push(Assignment {
            judge,
            views: found.ready,
        });
    }
    for judge in pinned {
        let id = judge.id();
        if routes.chosen == Some(id) || routes.passed_over.iter().any(|seen| seen.0 == id) {
            continue;
        }
        let availability = judge.available().await;
        if availability != Availability::Ready {
            routes.passed_over.push((id, availability));
            continue;
        }
        let found = pinned_views(g, vault, candidates, overrides, id)?;
        routes.unreadable += found.unreadable;
        if !found.ready.is_empty() {
            routes.assignments.push(Assignment {
                judge,
                views: found.ready,
            });
        }
    }
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use kbx_core::{EXAMPLE_ONTOLOGY, Ontology};
    use rusqlite::params;
    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;
    use crate::{JudgeContext, JudgeError, Verdict, judge_all};

    const PRIVATE: &str = "PRIVATE-EXCERPT";

    /// Records every pair it's given, in full, and every time it's checked on.
    #[derive(Clone)]
    struct Recorder {
        id: JudgeId,
        availability: Availability,
        pairs: Arc<Mutex<Vec<CandidateView>>>,
    }

    impl Recorder {
        fn new(id: JudgeId, availability: Availability) -> Recorder {
            Recorder {
                id,
                availability,
                pairs: Arc::default(),
            }
        }

        fn boxed(&self) -> Box<dyn Judge> {
            Box::new(self.clone())
        }

        fn cids(&self) -> Vec<String> {
            let pairs = self.pairs.lock().unwrap();
            let mut cids: Vec<String> = pairs.iter().map(|view| view.cid.clone()).collect();
            cids.sort();
            cids
        }

        /// Everything it was ever shown, as text.
        fn shown(&self) -> String {
            serde_json::to_string(&*self.pairs.lock().unwrap()).unwrap()
        }
    }

    #[async_trait]
    impl Judge for Recorder {
        fn id(&self) -> JudgeId {
            self.id
        }

        fn model(&self) -> &str {
            "fake"
        }

        async fn available(&self) -> Availability {
            self.availability
        }

        async fn judge(
            &self,
            batch: &[CandidateView],
            _ctx: &JudgeContext,
        ) -> Result<Vec<Verdict>, JudgeError> {
            self.pairs.lock().unwrap().extend_from_slice(batch);
            let verdict = |view: &CandidateView| Verdict {
                cid: view.cid.clone(),
                decision: crate::Decision::Reject,
                rel: "none".to_string(),
                confidence: 0.5,
                rationale: "r".to_string(),
                cost_usd: None,
                cached_tokens: None,
            };
            Ok(batch.iter().map(verdict).collect())
        }
    }

    /// Notes `public/a.md`, `public/b.md`, `legal/c.md`, `legal/d.md` and
    /// `hr/e.md`, the last three private, indexed.
    struct Vault {
        dir: TempDir,
        graph: Graph,
    }

    impl Vault {
        fn new() -> Vault {
            let dir = TempDir::new().unwrap();
            let graph = Graph::open(&dir.path().join("graph.sqlite")).unwrap();
            let notes = [
                ("public/a.md", "a"),
                ("public/b.md", "b"),
                ("legal/c.md", "c"),
                ("legal/d.md", "d"),
                ("hr/e.md", "e"),
            ];
            for (path, id) in notes {
                let file = dir.path().join(path);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                let private = !path.starts_with("public");
                let body = if private { PRIVATE } else { "Public text." };
                fs::write(file, format!("---\ntitle: {id}\n---\n\n{body}\n")).unwrap();
                graph
                    .connection()
                    .execute(
                        "INSERT INTO files (path, id, type, title) VALUES (?, ?, 'Concept', ?)",
                        params![path, id, id],
                    )
                    .unwrap();
            }
            Vault { dir, graph }
        }

        async fn route(
            &self,
            pins: &[(&str, &str)],
            pairs: &[(&str, &str)],
            general: &[&Recorder],
            pinned: &[&Recorder],
        ) -> Routes {
            let overrides = pins
                .iter()
                .map(|(folder, id)| (folder.to_string(), id.to_string()))
                .collect();
            let boxed = |list: &[&Recorder]| list.iter().map(|r| r.boxed()).collect::<Vec<_>>();
            route(
                &self.graph,
                self.dir.path(),
                &pairs.iter().map(proposal).collect::<Vec<_>>(),
                &overrides,
                boxed(general),
                boxed(pinned),
            )
            .await
            .unwrap()
        }

        /// Runs every assignment, as `kbx suggest` does.
        async fn judge(&self, routes: &Routes) {
            let ctx =
                JudgeContext::new(Ontology::parse(EXAMPLE_ONTOLOGY).unwrap(), EXAMPLE_ONTOLOGY);
            for assignment in &routes.assignments {
                let judge = assignment.judge.as_ref();
                judge_all(judge, &self.graph, &assignment.views, &ctx, 20)
                    .await
                    .unwrap();
            }
        }
    }

    fn proposal(pair: &(&str, &str)) -> Proposal {
        Proposal {
            cid: format!("{}{}", pair.0, pair.1),
            src: pair.0.to_string(),
            dst: pair.1.to_string(),
            generators: Vec::new(),
            features: Map::new(),
            score: 0.5,
            src_section: None,
            dst_section: None,
        }
    }

    fn ready(id: JudgeId) -> Recorder {
        Recorder::new(id, Availability::Ready)
    }

    /// One pair of each kind: no pin, pinned on both sides, pinned on one.
    const PAIRS: [(&str, &str); 4] = [("a", "b"), ("c", "d"), ("a", "c"), ("d", "b")];

    #[tokio::test]
    async fn a_scope_pinned_to_ollama_never_reaches_a_ready_cloud_cli() {
        let vault = Vault::new();
        let (claude, codex) = (ready(JudgeId::ClaudeCli), ready(JudgeId::CodexCli));
        let ollama = ready(JudgeId::Ollama);
        let pins = [("legal", "ollama"), ("hr/", "ollama")];
        let routes = vault
            .route(&pins, &PAIRS, &[&claude, &codex, &ollama], &[&ollama])
            .await;
        assert_eq!(routes.chosen, Some(JudgeId::ClaudeCli));
        assert_eq!(routes.pinned, 3);
        vault.judge(&routes).await;

        assert_eq!(claude.cids(), ["ab"]);
        assert!(!claude.shown().contains(PRIVATE), "{}", claude.shown());
        assert_eq!(codex.cids(), Vec::<String>::new());
        assert_eq!(ollama.cids(), ["ac", "cd", "db"]);
        assert!(ollama.shown().contains(PRIVATE));
    }

    #[tokio::test]
    async fn with_the_pinned_backend_down_its_pairs_wait_rather_than_fall_back() {
        let vault = Vault::new();
        let claude = ready(JudgeId::ClaudeCli);
        let ollama = Recorder::new(JudgeId::Ollama, Availability::Unreachable);
        let routes = vault
            .route(&[("legal", "ollama")], &PAIRS, &[&claude], &[&ollama])
            .await;
        vault.judge(&routes).await;

        assert_eq!(claude.cids(), ["ab"]);
        assert!(!claude.shown().contains(PRIVATE));
        assert_eq!(ollama.cids(), Vec::<String>::new());
        assert_eq!(
            routes.passed_over,
            [(JudgeId::Ollama, Availability::Unreachable)]
        );
    }

    #[tokio::test]
    async fn with_no_cloud_cli_ready_the_pinned_scope_is_still_judged_and_nothing_else_is() {
        let vault = Vault::new();
        let claude = Recorder::new(JudgeId::ClaudeCli, Availability::NotLoggedIn);
        let ollama = ready(JudgeId::Ollama);
        let routes = vault
            .route(&[("legal", "ollama")], &PAIRS, &[&claude], &[&ollama])
            .await;
        vault.judge(&routes).await;

        assert_eq!(routes.chosen, None);
        assert_eq!(claude.cids(), Vec::<String>::new());
        assert_eq!(ollama.cids(), ["ac", "cd", "db"]);
    }

    #[tokio::test]
    async fn the_chosen_backend_takes_its_own_pinned_pairs_once() {
        let vault = Vault::new();
        let ollama = ready(JudgeId::Ollama);
        let routes = vault
            .route(&[("legal", "ollama")], &PAIRS, &[&ollama], &[&ollama])
            .await;
        vault.judge(&routes).await;

        assert_eq!(routes.assignments.len(), 1);
        assert_eq!(ollama.cids(), ["ab", "ac", "cd", "db"]);
    }

    #[tokio::test]
    async fn a_pair_pinned_two_ways_is_shown_to_neither() {
        let vault = Vault::new();
        let ollama = ready(JudgeId::Ollama);
        let llama = ready(JudgeId::LlamaServer);
        let pins = [("legal", "ollama"), ("hr", "llama-server")];
        let routes = vault
            .route(&pins, &[("c", "e")], &[&ollama], &[&ollama, &llama])
            .await;
        vault.judge(&routes).await;

        assert_eq!(ollama.cids(), Vec::<String>::new());
        assert_eq!(llama.cids(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_scope_pinned_to_no_backend_is_shown_to_none() {
        let vault = Vault::new();
        let claude = ready(JudgeId::ClaudeCli);
        for name in ["none", "olama"] {
            let routes = vault
                .route(&[("legal", name)], &PAIRS, &[&claude], &[])
                .await;
            vault.judge(&routes).await;
        }
        assert_eq!(claude.cids(), ["ab", "ab"]);
    }

    #[tokio::test]
    async fn a_backend_passed_over_is_not_checked_twice() {
        let vault = Vault::new();
        let ollama = Recorder::new(JudgeId::Ollama, Availability::NotInstalled);
        let routes = vault
            .route(&[("legal", "ollama")], &PAIRS, &[&ollama], &[&ollama])
            .await;
        assert_eq!(routes.passed_over.len(), 1);
    }
}
