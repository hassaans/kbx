//! `kbx review`, `accept`, `reject`, `defer` and `unreject`.

use std::env;
use std::error::Error;
use std::io::{IsTerminal, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use kbx_core::{
    Decision, Graph, IndexError, IndexOptions, IndexState, Item, Ontology, Project, Queue,
    ReviewError, STRIKES, cid, index_state, index_vault, suppressed_pairs,
};
use serde_json::{Value, json};
use tracing::warn;

use crate::tui;

pub(crate) const NO_INDEX: &str = "the vault has no index yet; run `kbx index`";

/// What every decision starts from.
pub(crate) struct Session {
    pub(crate) project: Project,
    pub(crate) yaml: String,
    pub(crate) ontology: Ontology,
    pub(crate) graph: Graph,
}

impl Session {
    fn open() -> Result<Session, Box<dyn Error>> {
        Session::of(Project::discover(&env::current_dir()?)?)
    }

    pub(crate) fn of(project: Project) -> Result<Session, Box<dyn Error>> {
        let graph = Graph::open(&project.graph)?;
        Session::with(project, graph)
    }

    /// For looking only: the database stays as it is.
    pub(crate) fn read_only() -> Result<Session, Box<dyn Error>> {
        let project = Project::discover(&env::current_dir()?)?;
        if !project.graph.is_file() {
            return Err(NO_INDEX.into());
        }
        let graph = Graph::open_read_only(&project.graph)?;
        Session::with(project, graph)
    }

    fn with(project: Project, graph: Graph) -> Result<Session, Box<dyn Error>> {
        let yaml = crate::read_ontology(&project)?;
        let ontology = Ontology::parse(&yaml)?;
        if index_state(&graph, &yaml)? == IndexState::Unbuilt {
            return Err(NO_INDEX.into());
        }
        Ok(Session {
            project,
            yaml,
            ontology,
            graph,
        })
    }

    pub(crate) fn vault(&self) -> &Path {
        &self.project.vault
    }

    /// Brings the index up to date with what accepting wrote, given as paths
    /// from the vault. The links are on disk by now, so a failure here is for
    /// `kbx index` to repair.
    pub(crate) fn reindex(&mut self, written: &[PathBuf]) -> Result<(), IndexError> {
        if written.is_empty() {
            return Ok(());
        }
        let options = IndexOptions {
            paths: written.iter().map(|p| self.vault().join(p)).collect(),
            ..IndexOptions::default()
        };
        let vault = &self.project.vault;
        index_vault(&mut self.graph, vault, &self.yaml, &options)?;
        Ok(())
    }

    /// Takes a decided candidate's line out of its note, if it has one. The
    /// decision stands either way, and `kbx sync-stage` clears the line later.
    pub(crate) fn unstage(&mut self, cid: &str) -> Result<(), Box<dyn Error>> {
        let written = kbx_core::unstage(&self.graph, self.vault(), cid)?;
        self.reindex(&written.into_iter().collect::<Vec<_>>())?;
        Ok(())
    }
}

pub fn run(json: bool) -> Result<ExitCode, Box<dyn Error>> {
    let session = if json {
        Session::read_only()?
    } else {
        Session::open()?
    };
    let queue = Queue::load(&session.graph, &session.ontology)?;
    if json {
        println!("{}", queue_json(&queue));
        return Ok(ExitCode::SUCCESS);
    }
    if queue.current().is_none() {
        match queue.hidden() {
            0 => println!("nothing to review"),
            hidden => println!("nothing to review; {hidden} hidden, already rejected"),
        }
        return Ok(ExitCode::SUCCESS);
    }
    if !stdin().is_terminal() || !stdout().is_terminal() {
        return Err("`kbx review` needs a terminal; `kbx review --json` prints the queue".into());
    }
    let yaml = &session.yaml;
    if index_state(&session.graph, yaml)? == IndexState::Stale {
        warn!("the index is out of date; run `kbx index`");
    }
    tui::run(session, queue)?;
    Ok(ExitCode::SUCCESS)
}

/// The queue for editors and agents, in the order `kbx review` shows it.
fn queue_json(queue: &Queue) -> Value {
    queue.items().iter().map(item_json).collect()
}

fn item_json(item: &Item) -> Value {
    json!({
        "cid": item.cid,
        "src": item.src,
        "dst": item.dst,
        "src_title": item.src_title,
        "dst_title": item.dst_title,
        "rel": item.rel,
        "score": item.score,
        "generators": item.generators,
        "verdict": item.verdict,
        "rationale": item.rationale,
    })
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
            written.push(path);
        }
        Ok(())
    });
    session.reindex(&written)?;
    Ok(code)
}

pub fn reject(
    cids: &[String],
    rel: &Option<String>,
    reason: &Option<String>,
) -> Result<ExitCode, Box<dyn Error>> {
    let mut session = Session::open()?;
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
        if let Err(error) = session.unstage(cid) {
            eprintln!("warning: {cid}: its staged line stays: {error}");
        }
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

/// `kbx sync-stage`. The notes were edited, so the index comes first: a link
/// someone just added has to resolve, and a file has to match its hash.
pub fn sync_stage(mark_reviewed: bool) -> Result<ExitCode, Box<dyn Error>> {
    let mut session = Session::open()?;
    let vault = session.project.vault.clone();
    let options = IndexOptions::default();
    index_vault(&mut session.graph, &vault, &session.yaml, &options)?;

    let synced = kbx_core::sync_stage(&session.graph, &vault, &session.ontology)?;
    for (cid, accepted) in &synced.accepted {
        println!(
            "accepted {cid}: {} -{}-> {}",
            accepted.src, accepted.rel, accepted.dst
        );
    }
    for (cid, rejected) in &synced.rejected {
        let outcome = match rejected.suppressed() {
            true => ", suppressed; `kbx unreject` lifts it",
            false => "",
        };
        println!(
            "rejected {cid}: strike {} of {STRIKES}{outcome}",
            rejected.strikes
        );
    }
    let mut written = synced.written.clone();
    let mut code = ExitCode::SUCCESS;
    for (cid, error) in &synced.refused {
        eprintln!("error: {cid}: {error}");
        code = ExitCode::FAILURE;
    }
    if mark_reviewed {
        let date = today();
        for note in &synced.finished {
            match kbx_core::mark_reviewed(&session.graph, &vault, note, &date) {
                Ok(path) => written.extend(path),
                Err(error) => {
                    eprintln!("error: {note}: {error}");
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    session.reindex(&written)?;
    println!(
        "{} accepted, {} rejected, {} cleared, {} still staged, {} refused",
        synced.accepted.len(),
        synced.rejected.len(),
        synced.cleared,
        synced.pending,
        synced.refused.len()
    );
    Ok(code)
}

/// Today in UTC as `YYYY-MM-DD`, by the civil-from-days algorithm: a date in a
/// note isn't worth a calendar dependency.
fn today() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    let days = now.map_or(0, |elapsed| elapsed.as_secs() / 86_400) as i64 + 719_468;
    let (era, day) = (days / 146_097, days % 146_097);
    let year = (day - day / 1460 + day / 36_524 - day / 146_096) / 365;
    let of_year = day - (365 * year + year / 4 - year / 100);
    let shifted = (5 * of_year + 2) / 153;
    let date = of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{date:02}")
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
