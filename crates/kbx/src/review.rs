//! `kbx accept`, `reject`, `defer` and `unreject`.

use std::env;
use std::error::Error;
use std::process::ExitCode;

use kbx_core::{
    Decision, Graph, IndexOptions, IndexState, Ontology, Project, ReviewError, STRIKES, cid,
    index_state, index_vault, suppressed_pairs,
};

/// What every decision starts from.
struct Session {
    project: Project,
    yaml: String,
    ontology: Ontology,
    graph: Graph,
}

impl Session {
    fn open() -> Result<Session, Box<dyn Error>> {
        let project = Project::discover(&env::current_dir()?)?;
        let yaml = crate::read_ontology(&project)?;
        let ontology = Ontology::parse(&yaml)?;
        let graph = Graph::open(&project.graph)?;
        if index_state(&graph, &yaml)? == IndexState::Unbuilt {
            return Err("the vault has no index yet; run `kbx index`".into());
        }
        Ok(Session {
            project,
            yaml,
            ontology,
            graph,
        })
    }
}

/// Runs `decide` on each cid. One refusal doesn't stop the rest, and fails the
/// command at the end.
fn each(cids: &[String], mut decide: impl FnMut(&str) -> Result<(), ReviewError>) -> ExitCode {
    let mut code = ExitCode::SUCCESS;
    for cid in cids {
        if let Err(error) = decide(cid) {
            eprintln!("error: {cid}: {error}");
            code = ExitCode::FAILURE;
        }
    }
    code
}

fn decision<'a>(rel: &'a Option<String>, reason: &'a Option<String>) -> Decision<'a> {
    Decision {
        rel: rel.as_deref(),
        reason: reason.as_deref(),
    }
}

pub fn accept(
    cids: &[String],
    rel: &Option<String>,
    reason: &Option<String>,
) -> Result<ExitCode, Box<dyn Error>> {
    let mut session = Session::open()?;
    let mut written = Vec::new();
    let code = each(cids, |cid| {
        let accepted = kbx_core::accept(
            &session.graph,
            &session.project.vault,
            &session.ontology,
            cid,
            decision(rel, reason),
        )?;
        println!(
            "accepted {cid}: {} -{}-> {}",
            accepted.src, accepted.rel, accepted.dst
        );
        for path in accepted.written {
            println!("  wrote {}", path.display());
            written.push(session.project.vault.join(path));
        }
        Ok(())
    });
    // The links are on disk, so a failure here is for `kbx index` to repair.
    if !written.is_empty() {
        let options = IndexOptions {
            paths: written,
            ..IndexOptions::default()
        };
        let vault = &session.project.vault;
        index_vault(&mut session.graph, vault, &session.yaml, &options)?;
    }
    Ok(code)
}

pub fn reject(
    cids: &[String],
    rel: &Option<String>,
    reason: &Option<String>,
) -> Result<ExitCode, Box<dyn Error>> {
    let session = Session::open()?;
    Ok(each(cids, |cid| {
        let rejected = kbx_core::reject(
            &session.graph,
            &session.ontology,
            cid,
            decision(rel, reason),
        )?;
        let outcome = if rejected.suppressed() {
            ", suppressed; `kbx unreject` lifts it"
        } else {
            ""
        };
        println!(
            "rejected {cid}: strike {} of {STRIKES}{outcome}",
            rejected.strikes
        );
        Ok(())
    }))
}

pub fn defer(
    cids: &[String],
    rel: &Option<String>,
    reason: &Option<String>,
) -> Result<ExitCode, Box<dyn Error>> {
    let session = Session::open()?;
    Ok(each(cids, |cid| {
        kbx_core::defer(
            &session.graph,
            &session.ontology,
            cid,
            decision(rel, reason),
        )?;
        println!("deferred {cid}");
        Ok(())
    }))
}

pub fn unreject(cids: &[String]) -> Result<ExitCode, Box<dyn Error>> {
    let session = Session::open()?;
    Ok(each(cids, |cid| {
        let lifted = kbx_core::unreject(&session.graph, cid)?;
        println!(
            "unrejected {cid}: {} and {}, {} strikes cleared",
            lifted.pair.0, lifted.pair.1, lifted.strikes
        );
        Ok(())
    }))
}

pub fn list_suppressed() -> Result<ExitCode, Box<dyn Error>> {
    let session = Session::open()?;
    let mut pairs: Vec<_> = suppressed_pairs(session.graph.connection())?
        .into_iter()
        .collect();
    pairs.sort();
    for (a, b) in pairs {
        println!("{} {a} {b}", cid(&a, &b));
    }
    Ok(ExitCode::SUCCESS)
}
