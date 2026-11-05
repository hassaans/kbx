//! The terminal side of `kbx review`: keys in, a screen out. Every decision is
//! a call on [`Queue`], and accepting reindexes as `kbx accept` does.

use std::error::Error;
use std::io;

use kbx_core::{Item, Queue, ReviewError, STRIKES, excerpt_of};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::review::Session;

const KEYS: &str = "a accept  r reject  d defer  e edit relation  s skip  q quit";
const PICK_KEYS: &str = "up/down choose  enter confirm  esc cancel";
const REASON_KEYS: &str = "enter reject  esc cancel";

/// What keys mean right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// The keys of §7.
    Browse,
    /// Choosing a relation, for the current item.
    Pick {
        selected: usize,
        /// Accept once it's chosen, because `a` found no relation to accept.
        then_accept: bool,
    },
    /// Typing why the current item is rejected.
    Reason(String),
}

/// What came of a line of the session, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Idle,
    Done(String),
    Refused(String),
}

/// How many decisions the session took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    accepted: usize,
    rejected: usize,
    deferred: usize,
    skipped: usize,
}

/// A review session: the queue, and what's on screen.
pub(crate) struct App {
    session: Session,
    queue: Queue,
    mode: Mode,
    status: Status,
    tally: Tally,
    /// The current item's cid and both excerpts, read when it became current.
    excerpts: Option<(String, [String; 2])>,
    /// Why the index is behind the notes, kept because the status bar may
    /// never be drawn again: the last acceptance ends the session.
    index_failure: Option<String>,
    quit: bool,
}

/// Runs the session until the queue ends or the user quits. Assumes a terminal.
pub fn run(session: Session, queue: Queue) -> Result<(), Box<dyn Error>> {
    let mut app = App::new(session, queue);
    let mut terminal = ratatui::try_init()?;
    let result = app.drive(&mut terminal);
    ratatui::try_restore()?;
    result?;
    println!("{}", app.summary());
    Ok(app.outcome()?)
}

impl App {
    pub(crate) fn new(session: Session, queue: Queue) -> App {
        let mut app = App {
            session,
            queue,
            mode: Mode::Browse,
            status: Status::Idle,
            tally: Tally::default(),
            excerpts: None,
            index_failure: None,
            quit: false,
        };
        app.read_excerpts();
        app
    }

    fn drive(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.finished() {
            terminal.draw(|frame| self.draw(frame))?;
            if let Event::Key(key) = event::read()? {
                self.key(key);
            }
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.quit || self.queue.current().is_none()
    }

    /// Takes one key. Windows reports a release for every press, and acting on
    /// both would decide two candidates.
    pub(crate) fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if control || key.modifiers.contains(KeyModifiers::ALT) {
            return;
        }
        match self.mode.clone() {
            Mode::Browse => self.browse(key.code),
            Mode::Pick {
                selected,
                then_accept,
            } => self.pick(key.code, selected, then_accept),
            Mode::Reason(text) => self.reason(key.code, text),
        }
        self.read_excerpts();
    }

    fn browse(&mut self, code: KeyCode) {
        let Some(item) = self.queue.current() else {
            return;
        };
        match code {
            KeyCode::Char('a') if item.rel.is_none() => self.open_picker(true),
            KeyCode::Char('a') => self.accept(),
            KeyCode::Char('r') => self.mode = Mode::Reason(String::new()),
            KeyCode::Char('d') => self.defer(),
            KeyCode::Char('e') => self.open_picker(false),
            KeyCode::Char('s') => {
                self.queue.skip();
                self.tally.skipped += 1;
                self.status = Status::Idle;
            }
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn open_picker(&mut self, then_accept: bool) {
        let relations = Queue::relations(&self.session.ontology);
        let rel = self.queue.current().and_then(|item| item.rel.as_deref());
        let selected = relations.iter().position(|word| Some(*word) == rel);
        self.mode = Mode::Pick {
            selected: selected.unwrap_or(0),
            then_accept,
        };
    }

    fn pick(&mut self, code: KeyCode, selected: usize, then_accept: bool) {
        let relations = Queue::relations(&self.session.ontology);
        let moved = match code {
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected
                .saturating_add(1)
                .min(relations.len().saturating_sub(1)),
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                return;
            }
            KeyCode::Enter => {
                self.mode = Mode::Browse;
                let Some(rel) = relations.get(selected).map(|word| word.to_string()) else {
                    self.status = Status::Refused("the ontology's vocabulary is empty".into());
                    return;
                };
                match self.queue.edit(&self.session.ontology, &rel) {
                    Ok(()) if then_accept => self.accept(),
                    Ok(()) => self.status = Status::Done(format!("relation is now {rel}")),
                    Err(error) => self.refused(error),
                }
                return;
            }
            _ => selected,
        };
        self.mode = Mode::Pick {
            selected: moved,
            then_accept,
        };
    }

    fn reason(&mut self, code: KeyCode, mut text: String) {
        match code {
            KeyCode::Esc => self.mode = Mode::Browse,
            KeyCode::Enter => {
                self.mode = Mode::Browse;
                self.reject(Some(text.trim()).filter(|reason| !reason.is_empty()));
            }
            KeyCode::Backspace => {
                text.pop();
                self.mode = Mode::Reason(text);
            }
            KeyCode::Char(c) => {
                text.push(c);
                self.mode = Mode::Reason(text);
            }
            _ => {}
        }
    }

    fn accept(&mut self) {
        let session = &self.session;
        let vault = session.vault();
        let accepted = self
            .queue
            .accept(&session.graph, vault, &session.ontology, None);
        match accepted {
            Ok(accepted) => {
                self.tally.accepted += 1;
                let mut line = format!(
                    "accepted: {} -{}-> {}",
                    accepted.src, accepted.rel, accepted.dst
                );
                if let Err(error) = self.session.reindex(&accepted.written) {
                    let failure = format!("the index is out of date, run `kbx index`: {error}");
                    line.push_str(&format!("; {failure}"));
                    self.index_failure = Some(failure);
                }
                self.status = Status::Done(line);
            }
            Err(error) => self.refused(error),
        }
    }

    fn reject(&mut self, reason: Option<&str>) {
        let session = &self.session;
        let cid = self.queue.current().map(|item| item.cid.clone());
        match self.queue.reject(&session.graph, &session.ontology, reason) {
            Ok(rejected) => {
                self.tally.rejected += 1;
                let unstaged = cid.map_or(Ok(()), |cid| self.session.unstage(&cid));
                let outcome = if rejected.suppressed() {
                    ", suppressed; `kbx unreject` lifts it"
                } else {
                    ""
                };
                let strikes = rejected.strikes;
                let mut line = format!("rejected: strike {strikes} of {STRIKES}{outcome}");
                if let Err(error) = unstaged {
                    line.push_str(&format!("; its staged line stays: {error}"));
                }
                self.status = Status::Done(line);
            }
            Err(error) => self.refused(error),
        }
    }

    fn defer(&mut self) {
        let session = &self.session;
        match self.queue.defer(&session.graph, &session.ontology, None) {
            Ok(_) => {
                self.tally.deferred += 1;
                self.status = Status::Done("deferred".to_string());
            }
            Err(error) => self.refused(error),
        }
    }

    /// The item stays current, so the user can edit it, skip it or quit.
    fn refused(&mut self, error: ReviewError) {
        self.status = Status::Refused(format!("refused: {error}"));
    }

    fn read_excerpts(&mut self) {
        let Some(item) = self.queue.current() else {
            self.excerpts = None;
            return;
        };
        if self
            .excerpts
            .as_ref()
            .is_some_and(|(cid, _)| *cid == item.cid)
        {
            return;
        }
        let read = |id: &str, section: &Option<String>| {
            let section = section.as_deref();
            let text = excerpt_of(&self.session.graph, self.session.vault(), id, section);
            match text {
                Ok(Some(text)) => plain(&text),
                Ok(None) => "(not on disk)".to_string(),
                Err(error) => format!("(can't be read: {error})"),
            }
        };
        let both = [
            read(&item.src, &item.src_section),
            read(&item.dst, &item.dst_section),
        ];
        self.excerpts = Some((item.cid.clone(), both));
    }

    /// What the session leaves for the user to repair.
    fn outcome(&self) -> Result<(), String> {
        self.index_failure.clone().map_or(Ok(()), Err)
    }

    fn summary(&self) -> String {
        let Tally {
            accepted,
            rejected,
            deferred,
            skipped,
        } = self.tally;
        format!(
            "accepted {accepted}, rejected {rejected}, deferred {deferred}, skipped {skipped}; \
             {} left in the queue",
            self.queue.items().len()
        )
    }

    pub(crate) fn draw(&self, frame: &mut Frame<'_>) {
        let Some(item) = self.queue.current() else {
            return;
        };
        let [header, notes, why, status, keys] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        frame.render_widget(self.header(item), header);
        self.draw_notes(frame, item, notes);
        frame.render_widget(why_paragraph(item), why);
        frame.render_widget(self.status_line(), status);
        frame.render_widget(self.keys_line(), keys);
        if let Mode::Pick { selected, .. } = self.mode {
            self.draw_picker(frame, selected);
        }
    }

    fn header(&self, item: &Item) -> Paragraph<'_> {
        let position = self.queue.passed() + 1;
        let total = self.queue.items().len();
        let mut count = format!("{position} of {total}  score {:.2}", item.score);
        if self.queue.hidden() > 0 {
            let hidden = self.queue.hidden();
            count.push_str(&format!("  ({hidden} hidden, already rejected)"));
        }
        if item.unwritten {
            count.push_str("  accepted, not written: a writes it");
        }
        let ontology = &self.session.ontology;
        let rel = item.rel.as_deref();
        let name = rel.map(|rel| ontology.lookup(rel).map_or(rel, |key| key.name));
        let src = plain(title(&item.src_title, &item.src));
        let dst = plain(title(&item.dst_title, &item.dst));
        let (from, to) = match item.swapped(ontology) {
            true => (dst, src),
            false => (src, dst),
        };
        let name = name.unwrap_or("no relation yet");
        let pair = format!("{from} -{name}-> {to}");
        let bold = Style::default().add_modifier(Modifier::BOLD);
        Paragraph::new(vec![Line::raw(count), Line::styled(pair, bold)])
    }

    fn draw_notes(&self, frame: &mut Frame<'_>, item: &Item, area: Rect) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(area);
        let (src, dst) = match &self.excerpts {
            Some((_, [src, dst])) => (src.as_str(), dst.as_str()),
            None => ("", ""),
        };
        let sides = [
            (left, &item.src_title, &item.src, &item.src_anchor, src),
            (right, &item.dst_title, &item.dst, &item.dst_anchor, dst),
        ];
        for (area, name, id, anchor, text) in sides {
            let anchor = anchor.as_deref().and_then(|a| a.split_once('#'));
            let heading = match anchor {
                Some((_, anchor)) => format!("{} #{anchor}", title(name, id)),
                None => title(name, id).to_string(),
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(plain(&heading));
            let paragraph = Paragraph::new(Text::raw(text))
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
    }

    fn status_line(&self) -> Paragraph<'_> {
        match &self.status {
            Status::Idle => Paragraph::new(""),
            Status::Done(line) => Paragraph::new(plain(line)),
            Status::Refused(line) => {
                Paragraph::new(plain(line)).style(Style::default().fg(Color::Red))
            }
        }
    }

    fn keys_line(&self) -> Paragraph<'_> {
        match &self.mode {
            Mode::Browse => Paragraph::new(KEYS),
            Mode::Pick { .. } => Paragraph::new(PICK_KEYS),
            Mode::Reason(text) => {
                Paragraph::new(format!("reason: {}_  ({REASON_KEYS})", plain(text)))
            }
        }
    }

    fn draw_picker(&self, frame: &mut Frame<'_>, selected: usize) {
        let relations = Queue::relations(&self.session.ontology);
        let width = relations.iter().map(|w| w.len()).max().unwrap_or(0) as u16 + 6;
        let height = relations.len() as u16 + 2;
        let area = frame.area().centered(
            Constraint::Length(width.max(20)),
            Constraint::Length(height),
        );
        let items: Vec<ListItem<'_>> = relations.into_iter().map(ListItem::new).collect();
        let block = Block::default().borders(Borders::ALL).title("relation");
        let list = List::new(items)
            .block(block)
            .highlight_symbol("> ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut state);
    }
}

fn title<'a>(title: &'a Option<String>, id: &'a str) -> &'a str {
    title.as_deref().unwrap_or(id)
}

fn why_paragraph(item: &Item) -> Paragraph<'_> {
    let why = match item.why_proposed.is_empty() {
        true => item.generators.join(", "),
        false => item.why_proposed.join(", "),
    };
    let judge = match (&item.verdict, &item.rationale) {
        (Some(verdict), Some(rationale)) => format!("judge: {verdict}: {rationale}"),
        (Some(verdict), None) => format!("judge: {verdict}"),
        _ => "judge: no verdict".to_string(),
    };
    let lines = vec![Line::raw(plain(&why)), Line::raw(plain(&judge))];
    Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("why proposed"))
        .wrap(Wrap { trim: false })
}

/// Note text as the terminal may show it. A note can come from anyone with
/// access to the vault, and escape sequences must not reach the screen.
pub(crate) fn plain(text: &str) -> String {
    let spaced = text.replace('\t', "    ");
    spaced
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use kbx_core::{Graph, IndexOptions, Project, index_vault, init};
    use kbx_gen::{Budget, Scope, generators, store, suggest};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyEventState;
    use rusqlite::params;
    use tempfile::TempDir;

    use super::*;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/vault");

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// The fixture vault after `kbx init`, `kbx index` and `kbx suggest`.
    fn session() -> (TempDir, Session) {
        let dir = TempDir::new().unwrap();
        copy_dir(Path::new(FIXTURE), dir.path());
        let location = kbx_core::ConfigLocation::Root;
        init(dir.path(), None, kbx_core::Search::Builtin, location).unwrap();
        let project = Project::discover(dir.path()).unwrap();
        let yaml = fs::read_to_string(&project.ontology).unwrap();
        let mut graph = Graph::open(&project.graph).unwrap();
        let exclusions = project.exclusions().unwrap();
        let options = IndexOptions::default();
        index_vault(
            &mut graph,
            &project.vault,
            &yaml,
            &exclusions,
            &Default::default(),
            &options,
        )
        .unwrap();
        drop(graph);
        let session = Session::of(project).unwrap();
        let names = ["common_neighbors".to_string()];
        let chosen = generators(Some(&names), None).unwrap();
        let scope = Scope {
            vault: session.vault(),
            ontology: &session.ontology,
            note: None,
        };
        let run = suggest(&session.graph, &chosen, &scope, Budget::default()).unwrap();
        store(&session.graph, &run).unwrap();
        (dir, session)
    }

    fn app() -> (TempDir, App) {
        let (dir, session) = session();
        (dir, loaded(session))
    }

    fn loaded(session: Session) -> App {
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        App::new(session, queue)
    }

    fn verdict(session: &Session, cid: &str, verdict: &str, rel: &str, rationale: &str) {
        session
            .graph
            .connection()
            .execute(
                "INSERT INTO verdicts (cid, backend, model, prompt_hash, verdict, rel, rationale,
                     created_at)
                 VALUES (?, 'test', 'm', 'p', ?, ?, ?, 1)",
                params![cid, verdict, rel, rationale],
            )
            .unwrap();
    }

    fn press(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// In the relation picker, moves to `rel` and confirms it.
    fn choose(app: &mut App, rel: &str) {
        let Mode::Pick { selected, .. } = app.mode else {
            panic!("no picker: {:?}", app.mode);
        };
        let relations = Queue::relations(&app.session.ontology);
        let wanted = relations.iter().position(|word| *word == rel).unwrap();
        for _ in 0..selected {
            press(app, KeyCode::Up);
        }
        for _ in 0..wanted {
            press(app, KeyCode::Down);
        }
        press(app, KeyCode::Enter);
    }

    fn current(app: &App) -> Item {
        app.queue.current().unwrap().clone()
    }

    fn note(app: &App, id: &str) -> String {
        let db = app.session.graph.connection();
        let path: String = db
            .query_row("SELECT path FROM files WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .unwrap();
        fs::read_to_string(app.session.vault().join(path)).unwrap()
    }

    fn review(app: &App, a: &str, b: &str) -> (String, String, Option<String>, i64) {
        app.session
            .graph
            .connection()
            .query_row(
                "SELECT rel, decision, reason, strikes FROM reviews
                 WHERE (src = ?1 AND dst = ?2) OR (src = ?2 AND dst = ?1)",
                params![a, b],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    fn count_reviews(app: &App) -> i64 {
        let db = app.session.graph.connection();
        db.query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap()
    }

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let cells: Vec<&str> = buffer.content.iter().map(|cell| cell.symbol()).collect();
        let rows = cells.chunks(width).map(|row| row.concat());
        rows.collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn rejecting_a_staged_candidate_takes_its_line_out_of_the_note() {
        let (_dir, mut session) = session();
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        let cids: Vec<String> = queue.items().iter().map(|i| i.cid.clone()).collect();
        let vault = session.vault().to_path_buf();
        let report = kbx_core::stage(&session.graph, &vault, &session.ontology, &cids).unwrap();
        session.reindex(&report.written).unwrap();

        let mut app = loaded(session);
        let cid = app.queue.current().unwrap().cid.clone();
        let path: String = app
            .session
            .graph
            .connection()
            .query_row(
                "SELECT f.path FROM staged s JOIN files f ON f.id = s.note WHERE s.cid = ?",
                [&cid],
                |row| row.get(0),
            )
            .unwrap();
        let note = vault.join(path);
        assert!(fs::read_to_string(&note).unwrap().contains(&cid));

        app.reject(None);
        assert!(!fs::read_to_string(&note).unwrap().contains(&cid));
        let line = kbx_core::Queue::load(&app.session.graph, &app.session.ontology).unwrap();
        assert!(!line.items().iter().any(|item| item.cid == cid));
    }

    #[test]
    fn a_full_session_accepts_rejects_edits_a_relation_and_defers() {
        let (_dir, session) = session();
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        assert!(queue.items().len() >= 5, "{}", queue.items().len());
        let edited = queue.items()[2].cid.clone();
        verdict(
            &session,
            &edited,
            "accept",
            "see_also",
            "they share a topic",
        );
        let mut app = loaded(session);

        // With no verdict, `a` asks for the relation first.
        let accepted = current(&app);
        press(&mut app, KeyCode::Char('a'));
        choose(&mut app, "see_also");
        assert!(note(&app, &accepted.src).contains(&format!("[[{}]]", accepted.dst)));
        assert!(note(&app, &accepted.dst).contains(&format!("[[{}]]", accepted.src)));
        let row = review(&app, &accepted.src, &accepted.dst);
        assert_eq!(row, ("see_also".into(), "accept".into(), None, 0));

        let rejected = current(&app);
        press(&mut app, KeyCode::Char('r'));
        typed(&mut app, "not related");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, Status::Done("rejected: strike 1 of 3".into()));
        let row = review(&app, &rejected.src, &rejected.dst);
        let reason = Some("not related".to_string());
        assert_eq!(row, ("none".into(), "reject".into(), reason, 1));

        let overruled = current(&app);
        assert_eq!(overruled.rel.as_deref(), Some("see_also"));
        press(&mut app, KeyCode::Char('e'));
        choose(&mut app, "part_of");
        assert_eq!(current(&app).rel.as_deref(), Some("part_of"));
        press(&mut app, KeyCode::Char('a'));
        let row = review(&app, &overruled.src, &overruled.dst);
        assert_eq!(row, ("part_of".into(), "accept".into(), None, 0));
        let holder = note(&app, &overruled.src) + &note(&app, &overruled.dst);
        assert!(holder.contains("part_of:"), "{holder}");
        assert!(holder.contains("has_part:"), "{holder}");
        assert!(!holder.contains(&format!("see_also: [\"[[{}]]\"]", overruled.dst)));

        let deferred = current(&app);
        press(&mut app, KeyCode::Char('d'));
        let row = review(&app, &deferred.src, &deferred.dst);
        assert_eq!(row, ("none".into(), "defer".into(), None, 0));

        let skipped = current(&app);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(count_reviews(&app), 4);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.finished());
        assert_eq!(
            app.summary(),
            format!(
                "accepted 2, rejected 1, deferred 1, skipped 1; {} left in the queue",
                app.queue.items().len()
            )
        );

        // Accepting reindexed, as `kbx accept` does.
        let neighbors = app.session.graph.neighbors(&accepted.src).unwrap();
        assert!(
            neighbors
                .iter()
                .any(|n| n.rel == "see_also" && n.node.id == accepted.dst)
        );

        let session = app.session;
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        let left: Vec<&str> = queue.items().iter().map(|i| i.cid.as_str()).collect();
        assert!(left.contains(&deferred.cid.as_str()));
        assert!(left.contains(&skipped.cid.as_str()));
        for gone in [&accepted.cid, &rejected.cid, &overruled.cid] {
            assert!(!left.contains(&gone.as_str()));
        }
    }

    #[test]
    fn the_screen_shows_both_excerpts_the_provenance_and_the_rationale() {
        let (_dir, session) = session();
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        let first = queue.items()[0].clone();
        verdict(
            &session,
            &first.cid,
            "accept",
            "see_also",
            "both cover logins",
        );
        let app = loaded(session);

        let text = screen(&app);
        let (src, dst) = app.excerpts.as_ref().map(|e| (&e.1[0], &e.1[1])).unwrap();
        for excerpt in [src, dst] {
            let start: String = excerpt.lines().next().unwrap().chars().take(20).collect();
            assert!(text.contains(&start), "{start} not in\n{text}");
        }
        assert!(text.contains("common_neighbors:"), "{text}");
        assert!(text.contains("judge: accept: both cover logins"), "{text}");
        assert!(text.contains("-see_also->"), "{text}");
        assert!(text.contains(KEYS), "{text}");
    }

    #[test]
    fn the_header_reads_the_way_accepting_would_write_it() {
        let (_dir, session) = session();
        let db = session.graph.connection();
        let of_class = |class: &str| -> (String, String) {
            db.query_row(
                "SELECT id, title FROM files WHERE type = ? ORDER BY id LIMIT 1",
                [class],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        let (concept, concept_title) = of_class("Concept");
        let (procedure, procedure_title) = of_class("Procedure");
        let cid = kbx_core::cid(&concept, &procedure);
        db.execute("DELETE FROM candidates WHERE cid = ?", [&cid])
            .unwrap();
        db.execute(
            "INSERT INTO candidates (cid, src, dst, score, src_hash, dst_hash)
             VALUES (?1, ?2, ?3, 2.0,
                 (SELECT content_hash FROM files WHERE id = ?2),
                 (SELECT content_hash FROM files WHERE id = ?3))",
            params![cid, concept, procedure],
        )
        .unwrap();
        verdict(&session, &cid, "accept", "depends_on", "x");
        let mut app = loaded(session);
        assert_eq!(current(&app).src, concept);

        let shown = format!("{procedure_title} -depends_on-> {concept_title}");
        assert!(screen(&app).contains(&shown), "{}", screen(&app));

        press(&mut app, KeyCode::Char('a'));
        let written = format!("accepted: {procedure} -depends_on-> {concept}");
        assert_eq!(app.status, Status::Done(written));
    }

    #[test]
    fn a_reindex_that_fails_on_the_last_acceptance_is_not_lost() {
        let (_dir, session) = session();
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        let cids: Vec<String> = queue.items().iter().map(|i| i.cid.clone()).collect();
        for cid in &cids[1..] {
            let db = session.graph.connection();
            db.execute("DELETE FROM candidates WHERE cid = ?", [cid])
                .unwrap();
        }
        let mut app = loaded(session);
        assert_eq!(app.outcome(), Ok(()));
        app.session.yaml = "not an ontology".to_string();

        press(&mut app, KeyCode::Char('a'));
        choose(&mut app, "see_also");

        assert!(app.finished());
        assert_eq!(app.tally.accepted, 1);
        let error = app.outcome().unwrap_err();
        assert!(error.contains("kbx index"), "{error}");
    }

    #[test]
    fn a_candidate_nobody_judged_says_so() {
        let (_dir, app) = app();
        let text = screen(&app);
        assert!(text.contains("judge: no verdict"), "{text}");
        assert!(text.contains("no relation yet"), "{text}");
    }

    #[test]
    fn key_releases_are_not_presses() {
        let (_dir, mut app) = app();
        let first = current(&app);
        let release = KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        app.key(release);
        assert_eq!(current(&app).cid, first.cid);
        assert_eq!(count_reviews(&app), 0);
    }

    #[test]
    fn a_held_key_repeats_like_a_press() {
        let (_dir, mut app) = app();
        let first = current(&app);
        let repeat = KeyEvent {
            code: KeyCode::Char('s'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        };
        app.key(repeat);
        assert_ne!(current(&app).cid, first.cid);
    }

    #[test]
    fn control_combinations_decide_nothing_and_control_c_quits() {
        let (_dir, mut app) = app();
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!(count_reviews(&app), 0);
        assert!(!app.finished());

        press(&mut app, KeyCode::Char('r'));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.finished());
        assert_eq!(count_reviews(&app), 0);
    }

    #[test]
    fn escape_backs_out_of_a_rejection_or_a_pick_with_nothing_recorded() {
        let (_dir, mut app) = app();
        let first = current(&app);
        press(&mut app, KeyCode::Char('r'));
        typed(&mut app, "no");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Browse);
        assert_eq!(current(&app), first);
        assert_eq!(count_reviews(&app), 0);
    }

    #[test]
    fn decision_keys_typed_into_a_reason_are_text() {
        let (_dir, mut app) = app();
        let first = current(&app);
        press(&mut app, KeyCode::Char('r'));
        typed(&mut app, "  a dq");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        let row = review(&app, &first.src, &first.dst);
        assert_eq!(row.2.as_deref(), Some("a d"));
    }

    #[test]
    fn an_empty_reason_records_none() {
        let (_dir, mut app) = app();
        let first = current(&app);
        press(&mut app, KeyCode::Char('r'));
        typed(&mut app, "   ");
        press(&mut app, KeyCode::Enter);
        assert_eq!(review(&app, &first.src, &first.dst).2, None);
    }

    #[test]
    fn the_picker_stops_at_both_ends_and_starts_on_the_current_relation() {
        let (_dir, session) = session();
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        verdict(&session, &queue.items()[0].cid, "accept", "see_also", "x");
        let mut app = loaded(session);
        let relations: Vec<String> = Queue::relations(&app.session.ontology)
            .iter()
            .map(|word| word.to_string())
            .collect();
        let at = relations.iter().position(|w| w == "see_also").unwrap();

        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.mode, Mode::Pick { selected, .. } if selected == at));
        for _ in 0..relations.len() + 1 {
            press(&mut app, KeyCode::Down);
        }
        let last = relations.len() - 1;
        assert!(matches!(app.mode, Mode::Pick { selected, .. } if selected == last));
        for _ in 0..relations.len() + 1 {
            press(&mut app, KeyCode::Up);
        }
        assert!(matches!(app.mode, Mode::Pick { selected: 0, .. }));
        let text = screen(&app);
        assert!(text.contains(&relations[0]), "{text}");
        press(&mut app, KeyCode::Enter);
        assert_eq!(current(&app).rel.as_deref(), Some(relations[0].as_str()));
    }

    #[test]
    fn a_refused_decision_keeps_the_item_and_says_why() {
        let (dir, mut app) = app();
        let first = current(&app);
        let path: String = app
            .session
            .graph
            .connection()
            .query_row("SELECT path FROM files WHERE id = ?", [&first.src], |row| {
                row.get(0)
            })
            .unwrap();
        let edited = fs::read_to_string(dir.path().join(&path)).unwrap() + "\nA late edit.\n";
        fs::write(dir.path().join(&path), &edited).unwrap();

        press(&mut app, KeyCode::Char('a'));
        choose(&mut app, "see_also");

        assert!(matches!(&app.status, Status::Refused(line) if line.contains("changed")));
        assert_eq!(current(&app).cid, first.cid);
        assert_eq!(app.tally, Tally::default());
        assert_eq!(fs::read_to_string(dir.path().join(&path)).unwrap(), edited);
        assert_eq!(count_reviews(&app), 0);
        assert!(screen(&app).contains("refused:"));
    }

    #[test]
    fn the_third_rejection_says_the_pair_is_suppressed() {
        let (_dir, session) = session();
        let queue = Queue::load(&session.graph, &session.ontology).unwrap();
        let first = queue.items()[0].clone();
        let db = session.graph.connection();
        db.execute(
            "INSERT INTO reviews (src, dst, rel, decision, strikes) VALUES (?, ?, 'none', 'reject', 2)",
            params![first.src, first.dst],
        )
        .unwrap();
        let mut app = loaded(session);
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(&app.status, Status::Done(line) if line.contains("suppressed")));
    }

    #[test]
    fn the_session_ends_with_the_queue() {
        let (_dir, mut app) = app();
        for _ in 0..app.queue.items().len() {
            assert!(!app.finished());
            press(&mut app, KeyCode::Char('s'));
        }
        assert!(app.finished());
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(count_reviews(&app), 0);
    }

    #[test]
    fn excerpts_follow_the_current_item() {
        let (_dir, mut app) = app();
        let first = app.excerpts.clone().unwrap();
        assert_eq!(first.0, current(&app).cid);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.excerpts.as_ref().unwrap().0, current(&app).cid);
    }

    #[test]
    fn escape_sequences_in_a_note_never_reach_the_terminal() {
        let cleaned = plain("a\u{1b}[2Jb\tc\r\nd\u{7}");
        assert_eq!(cleaned, "a[2Jb    c\nd");
    }
}
