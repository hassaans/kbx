//! `kbx review`, `accept`, `reject`, `defer` and `unreject`.

use std::env;
use std::error::Error;
use std::io::{IsTerminal, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use kbx_core::{
    AcceptedRelation, Decision, Graph, IndexError, IndexOptions, IndexState, Item, Ontology,
    Preview, Project, Queue, ReviewError, STRIKES, accepted_relations, cid, index_state,
    index_vault, suppressed_pairs, written_paths,
};
use serde_json::{Value, json};
use tracing::warn;

use crate::tui;

pub(crate) const NO_INDEX: &str = "the vault has no index yet; run `kbx index`";

/// Refuses `what`, which only writes to notes, on a vault that keeps them as they
/// are.
pub(crate) fn require_frontmatter(project: &Project, what: &str) -> Result<(), String> {
    if project.config.write.frontmatter {
        return Ok(());
    }
    Err(format!(
        "{what} writes to your notes, and this vault keeps everything in .kbx; \
         set `frontmatter = true` under `[write]` in {} to turn that on",
        project.config_file.display()
    ))
}

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
        let defaults = project.config.type_defaults();
        if index_state(&graph, &yaml, &defaults)? == IndexState::Unbuilt {
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

    /// Whether accepting writes to notes.
    pub(crate) fn writes_notes(&self) -> bool {
        self.project.config.write.frontmatter
    }

    /// The queue as this vault shows it.
    fn queue(&self) -> rusqlite::Result<Queue> {
        let queue = Queue::load(&self.graph, &self.ontology)?;
        Ok(match self.writes_notes() {
            true => queue,
            false => queue.without_accepted(),
        })
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
        let exclusions = self.project.exclusions()?;
        let defaults = self.project.config.type_defaults();
        index_vault(
            &mut self.graph,
            vault,
            &self.yaml,
            &exclusions,
            &defaults,
            &options,
        )?;
        Ok(())
    }

    /// Takes a decided candidate's line out of its note, if it has one. The
    /// decision stands either way, and `kbx sync-stage` clears the line later,
    /// which is all that happens to one while the vault doesn't write to notes.
    pub(crate) fn unstage(&mut self, cid: &str) -> Result<(), Box<dyn Error>> {
        if !self.writes_notes() {
            return Ok(());
        }
        let written = kbx_core::unstage(&self.graph, self.vault(), cid)?;
        self.reindex(&written.into_iter().collect::<Vec<_>>())?;
        Ok(())
    }
}

pub fn run(json: bool, accepted: bool) -> Result<ExitCode, Box<dyn Error>> {
    let session = if json || accepted {
        Session::read_only()?
    } else {
        Session::open()?
    };
    let held = accepted_relations(session.graph.connection())?;
    if accepted {
        print_accepted(&session, &held, json)?;
        return Ok(ExitCode::SUCCESS);
    }
    let queue = session.queue()?;
    if json {
        println!("{}", queue_json(&queue));
        return Ok(ExitCode::SUCCESS);
    }
    if queue.current().is_none() {
        let mut line = "nothing to review".to_string();
        if queue.hidden() > 0 {
            line.push_str(&format!("; {} hidden, already rejected", queue.hidden()));
        }
        if !held.is_empty() {
            let count = crate::plural(held.len(), "accepted relation");
            line.push_str(&format!("; {count} in .kbx, see `kbx review --accepted`"));
        }
        println!("{line}");
        return Ok(ExitCode::SUCCESS);
    }
    if !stdin().is_terminal() || !stdout().is_terminal() {
        return Err("`kbx review` needs a terminal; `kbx review --json` prints the queue".into());
    }
    let yaml = &session.yaml;
    let defaults = session.project.config.type_defaults();
    if index_state(&session.graph, yaml, &defaults)? == IndexState::Stale {
        warn!("the index is out of date; run `kbx index`");
    }
    tui::run(session, queue, held.len())?;
    Ok(ExitCode::SUCCESS)
}

/// `kbx review --accepted`: the relations kept in `.kbx/`, each read as a note
/// would state it. The cid is what `kbx reject` takes to withdraw one.
fn print_accepted(
    session: &Session,
    held: &[AcceptedRelation],
    json: bool,
) -> Result<(), Box<dyn Error>> {
    if json {
        let title = |id: &str| -> rusqlite::Result<Option<String>> {
            Ok(session.graph.node(id)?.map(|node| node.title))
        };
        let mut rows = Vec::new();
        for relation in held {
            rows.push(json!({
                "cid": relation.cid,
                "src": relation.src,
                "dst": relation.dst,
                "src_title": title(&relation.src)?,
                "dst_title": title(&relation.dst)?,
                "rel": relation.rel,
            }));
        }
        println!("{}", Value::from(rows));
        return Ok(());
    }
    for relation in held {
        let (src, dst, rel) = (&relation.src, &relation.dst, &relation.rel);
        println!("{} {src} -{rel}-> {dst}", relation.cid);
    }
    Ok(())
}

/// The queue for editors and agents, in the order `kbx review` shows it.
fn queue_json(queue: &Queue) -> Value {
    queue.items().iter().map(item_json).collect()
}

fn item_json(item: &Item) -> Value {
    let mut json = json!({
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
    });
    // Only where it's so, as with the anchors below.
    if item.unwritten {
        json["accepted_unwritten"] = json!(true);
    }
    // Only where there are any, so a note-level item reads as it always has.
    for (key, anchor) in [
        ("src_anchor", &item.src_anchor),
        ("dst_anchor", &item.dst_anchor),
    ] {
        if let Some(anchor) = anchor {
            json[key] = json!(anchor);
        }
    }
    json
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

/// `kbx accept`: into the notes on a vault that writes to them, unless told not
/// to, and otherwise into `.kbx/`.
pub fn accept(
    cids: &[String],
    rel: &Option<String>,
    reason: &Option<String>,
    no_write: bool,
) -> Result<ExitCode, Box<dyn Error>> {
    let mut session = Session::open()?;
    if no_write || !session.writes_notes() {
        return accept_unwritten(session, cids, rel, reason);
    }
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

/// The diffs come last, one per note, with everything the run accepted in them.
fn accept_unwritten(
    session: Session,
    cids: &[String],
    rel: &Option<String>,
    reason: &Option<String>,
) -> Result<ExitCode, Box<dyn Error>> {
    let mut preview = Preview::default();
    let code = each(cids, |cid| {
        let accepted = kbx_core::accept_unwritten(
            &session.graph,
            &session.project.vault,
            &session.ontology,
            cid,
            decision(rel, reason),
            &mut preview,
        )?;
        println!(
            "accepted {cid}, not written: {} -{}-> {}",
            accepted.src, accepted.rel, accepted.dst
        );
        let key = session.ontology.lookup(&accepted.rel);
        if key.is_some_and(|key| key.relation.derived_only) {
            println!("  its inverse is derived by `kbx index`, so only one note would change");
        }
        Ok(())
    });
    print_preview(&preview);
    Ok(code)
}

/// Only a terminal gets the diffs cleaned up: note text can hold escape
/// sequences, and piped output has to stay exact.
fn print_preview(preview: &Preview) {
    if preview.is_empty() {
        println!("no note would change");
    }
    for diff in preview.diffs() {
        match stdout().is_terminal() {
            true => print!("{}", tui::plain(&diff)),
            false => print!("{diff}"),
        }
    }
}

/// `kbx apply`: the diff of everything kept in `.kbx/`, then with the setting on
/// and without `--dry-run`, the writes.
pub fn apply(dry_run: bool) -> Result<ExitCode, Box<dyn Error>> {
    let mut session = Session::open()?;
    if !dry_run {
        require_frontmatter(&session.project, "`kbx apply`")?;
    }
    let (vault, ontology) = (session.vault(), &session.ontology);
    let mut preview = Preview::default();
    let planned = kbx_core::apply(&session.graph, vault, ontology, Some(&mut preview))?;
    print_preview(&preview);
    let outcome = match dry_run {
        true => planned,
        false => kbx_core::apply(&session.graph, vault, ontology, None)?,
    };
    let verb = if dry_run { "would write" } else { "wrote" };
    for accepted in &outcome.applied {
        println!(
            "{verb} {} -{}-> {}",
            accepted.src, accepted.rel, accepted.dst
        );
    }
    let mut code = ExitCode::SUCCESS;
    for (relation, error) in &outcome.refused {
        eprintln!("error: {}: {error}", relation.cid);
        code = ExitCode::FAILURE;
    }
    session.reindex(&written_paths(&outcome))?;
    Ok(code)
}

/// `kbx mark-reviewed`: locks notes from new proposals until their content
/// changes, in the notes or else in `.kbx/`.
pub fn mark_reviewed(notes: &[String]) -> Result<ExitCode, Box<dyn Error>> {
    let mut session = Session::open()?;
    let date = today();
    let mut written = Vec::new();
    let code = each(notes, |note| {
        let (graph, vault) = (&session.graph, session.vault());
        match session.writes_notes() {
            true => written.extend(kbx_core::mark_reviewed(graph, vault, note, &date)?),
            false => kbx_core::hold_reviewed(graph, vault, note, &date)?,
        }
        println!("reviewed {note}: {date}");
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
    require_frontmatter(&session.project, "`kbx sync-stage`")?;
    let vault = session.project.vault.clone();
    let options = IndexOptions::default();
    let exclusions = session.project.exclusions()?;
    let defaults = session.project.config.type_defaults();
    index_vault(
        &mut session.graph,
        &vault,
        &session.yaml,
        &exclusions,
        &defaults,
        &options,
    )?;

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
