mod doctor;
mod export;
mod hook;
mod qmd;
mod review;
mod suggest;
mod tui;
mod watch;

use std::env;
use std::error::Error;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kbx_core::{
    Finding, Graph, IndexOptions, IndexReport, Neighbor, Node, Project, Severity, index_vault,
    init, lint_vault,
};
use serde_json::json;
use tracing_subscriber::EnvFilter;

const LINT_CODES: &str = "\
Finding codes:
  type.unknown     `type` is not a class in the ontology
  rel.unknown      a key holds wikilinks but is not a relation, or a relation
                   key holds something other than wikilinks
  rel.range        a link's target has a class outside the relation's range
  rel.domain       the note's class is outside the domain of a relation it holds
  schema.gap       a relation the note's class requires is missing or empty
  rel.cardinality  an exclusive relation has more than one target
  link.unstable    a link resolves by filename, not id (warning)
  link.unresolved  a link resolves to no note, or to more than one
  note.invalid     a file with frontmatter can't be read as a note, so `kbx index`
                   leaves it out
  id.duplicate     several files claim one id; the lowest path keeps it, and links
                   and `kbx graph` go there

Every code is an error except link.unstable, a warning. Only errors make kbx lint
exit 1; --strict promotes warnings to errors.

Plain markdown without frontmatter is not a note.
Paths in the output are relative to the vault root.";

/// Turn a markdown vault into a typed knowledge graph.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// How to write logs, which go to stderr. KBX_LOG sets the level.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log: LogFormat,
}

#[derive(Clone, Copy, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
enum LintFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Command {
    /// Create .kbx/, kbx.toml and the example ontology, install qmd if it's
    /// missing, and create a qmd collection per scope.
    Init {
        /// Use this ontology file instead of the example.
        #[arg(long)]
        ontology: Option<PathBuf>,
    },
    /// Compile the vault into .kbx/graph.sqlite, by content hash.
    Index {
        /// Ignore the stored hashes and rebuild.
        #[arg(long)]
        full: bool,
        /// Limit the run to these files and folders.
        paths: Vec<PathBuf>,
    },
    /// Check notes against the ontology. Exits 1 if there are errors.
    #[command(after_help = LINT_CODES)]
    Lint {
        /// How to print the findings.
        #[arg(long, value_enum, default_value_t = LintFormat::Text)]
        format: LintFormat,
        /// Promote warnings to errors.
        #[arg(long)]
        strict: bool,
        /// Report on these files and folders alone, such as the ones you
        /// changed. They are still checked against the whole vault.
        paths: Vec<PathBuf>,
    },
    /// Query the index.
    Graph {
        #[command(subcommand)]
        query: GraphQuery,
    },
    /// Propose missing links and store them as candidates.
    Suggest {
        /// Keep only pairs touching this note.
        #[arg(long)]
        note: Option<String>,
        /// Run only these generators, and fail if one can't run.
        #[arg(long, value_delimiter = ',')]
        generators: Option<Vec<String>>,
        /// Who judges the candidates.
        #[arg(long, value_enum, default_value_t = suggest::Judge::None)]
        judge: suggest::Judge,
        /// Keep at most this many candidates; qmd_knn stops searching there.
        #[arg(long, value_name = "CANDIDATES")]
        budget: Option<usize>,
        /// Print counts and token estimates; leave the database alone.
        #[arg(long)]
        dry_run: bool,
        /// Score each generator's precision and recall against the reviews
        /// table, and change nothing.
        #[arg(long, conflicts_with_all = ["note", "judge", "budget", "dry_run"])]
        eval: bool,
    },
    /// Keep the index current: re-index saved notes and lint them.
    Watch {
        /// Also run `kbx suggest` once the saves stop.
        #[arg(long)]
        suggest: bool,
        /// Wait this long after a file's last event before indexing it.
        #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(1..))]
        debounce_ms: u64,
        /// Hash the whole vault this often, for filesystems that drop events.
        /// 0 turns it off.
        #[arg(long, default_value_t = 300)]
        reconcile_secs: u64,
        /// How long after the last save `--suggest` runs (§8).
        #[arg(long, default_value_t = 45)]
        suggest_debounce_secs: u64,
    },
    /// Set up checks that run outside kbx.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Go through the candidates in a terminal, best first: a accepts, r
    /// rejects, d defers, e edits the relation, s skips, q quits.
    Review {
        /// Print the queue as JSON and change nothing.
        #[arg(long)]
        json: bool,
    },
    /// Write candidates to frontmatter: the relation on one note, its inverse
    /// on the other. Refused if either note changed since `kbx suggest`.
    Accept {
        /// The candidates.
        #[arg(required = true)]
        cids: Vec<String>,
        /// The relation, where no judge named one or to overrule it.
        #[arg(long)]
        rel: Option<String>,
        /// Why, kept in the reviews table.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Take candidates out of the queue. Three rejections of a pair suppress
    /// it from every generator.
    Reject {
        /// The candidates.
        #[arg(required = true)]
        cids: Vec<String>,
        /// The relation rejected, if not the one the judge named.
        #[arg(long)]
        rel: Option<String>,
        /// Why, kept in the reviews table.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Put off a decision, leaving candidates in the queue.
    Defer {
        /// The candidates.
        #[arg(required = true)]
        cids: Vec<String>,
        /// The relation in question, if not the one the judge named.
        #[arg(long)]
        rel: Option<String>,
        /// Why, kept in the reviews table.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Clear a pair's strikes, so generators can propose it again.
    Unreject {
        /// The pairs' cids, which `--list` prints.
        #[arg(required_unless_present = "list")]
        cids: Vec<String>,
        /// Print the suppressed pairs instead.
        #[arg(long, conflicts_with = "cids")]
        list: bool,
    },
    /// Print the graph or the review decisions, for other tools.
    Export {
        #[command(subcommand)]
        what: ExportWhat,
    },
    /// Load what `kbx export` printed.
    Import {
        #[command(subcommand)]
        what: ImportWhat,
    },
    /// Check the vault, the index, RAM, the judges and qmd, installing qmd if
    /// it's missing.
    Doctor {
        /// Create the qmd collections that are missing.
        #[arg(long)]
        fix: bool,
    },
}

#[derive(Subcommand)]
enum HookAction {
    /// Add a git pre-commit hook that lints staged notes, and print a recipe
    /// for Obsidian's Shell commands plugin.
    Install {
        /// Replace an existing pre-commit hook instead of adding to it.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ExportWhat {
    /// The edges table.
    Edges(Jsonl),
    /// The reviews table, which `kbx import reviews` reads back.
    Reviews(Jsonl),
    /// The graph as Turtle, with predicates from the ontology's `iri` keys.
    Rdf,
}

#[derive(Subcommand)]
enum ImportWhat {
    /// Add to the reviews table. A row that's already there keeps a newer
    /// decision, and notes aren't touched.
    Reviews {
        #[command(flatten)]
        format: Jsonl,
        /// What `kbx export reviews` printed. Without one, stdin.
        file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct Jsonl {
    /// One JSON object per line: the default, and so far the only format.
    #[arg(long)]
    #[allow(dead_code)]
    jsonl: bool,
}

#[derive(Subcommand)]
enum GraphQuery {
    /// List the edges at a note.
    Neighbors {
        /// The note's id.
        id: String,
    },
    /// Find a shortest path between two notes.
    Path {
        /// The id to start from.
        from: String,
        /// The id to reach.
        to: String,
    },
    /// Print node, edge and section counts.
    Stats,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = EnvFilter::try_from_env("KBX_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let logs = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match cli.log {
        LogFormat::Text => logs.with_ansi(std::io::stderr().is_terminal()).init(),
        LogFormat::Json => logs.json().init(),
    }

    let result = match cli.command {
        Command::Init { ontology } => run_init(ontology),
        Command::Index { full, paths } => run_index(IndexOptions {
            full,
            paths,
            ..IndexOptions::default()
        }),
        Command::Lint {
            format,
            strict,
            paths,
        } => run_lint(format, strict, &paths),
        Command::Graph { query } => run_graph(query),
        Command::Suggest {
            note,
            generators,
            judge,
            budget,
            dry_run,
            eval,
        } => suggest::run(suggest::Options {
            note,
            generators,
            judge,
            budget,
            dry_run,
            eval,
        }),
        Command::Watch {
            suggest,
            debounce_ms,
            reconcile_secs,
            suggest_debounce_secs,
        } => watch::run(watch::Options {
            debounce: Duration::from_millis(debounce_ms),
            reconcile: (reconcile_secs > 0).then(|| Duration::from_secs(reconcile_secs)),
            suggest: suggest.then(|| Duration::from_secs(suggest_debounce_secs)),
        }),
        Command::Hook {
            action: HookAction::Install { force },
        } => hook::install(force),
        Command::Review { json } => review::run(json),
        Command::Accept { cids, rel, reason } => review::accept(&cids, &rel, &reason),
        Command::Reject { cids, rel, reason } => review::reject(&cids, &rel, &reason),
        Command::Defer { cids, rel, reason } => review::defer(&cids, &rel, &reason),
        Command::Unreject { cids, list: false } => review::unreject(&cids),
        Command::Unreject { list: true, .. } => review::list_suppressed(),
        Command::Export { what } => match what {
            ExportWhat::Edges(_) => export::edges(),
            ExportWhat::Reviews(_) => export::reviews(),
            ExportWhat::Rdf => export::rdf(),
        },
        Command::Import {
            what: ImportWhat::Reviews { file, .. },
        } => export::import_reviews(file.as_deref()),
        Command::Doctor { fix } => doctor::run(fix),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_init(ontology: Option<PathBuf>) -> Result<ExitCode, Box<dyn Error>> {
    let dir = env::current_dir()?;
    let report = init(&dir, ontology.as_deref())?;
    if report.config {
        println!("created kbx.toml");
    }
    if report.ontology {
        println!("created .kbx/ontology.yaml");
    }
    println!("found qmd at {}", qmd::ensure()?.display());
    // The vault works without search, so this doesn't fail the command.
    let mut project = Project::load(&dir.join(kbx_core::CONFIG_FILE))?;
    match qmd::set_up_collections(&mut project) {
        Ok(created) => {
            for name in created {
                println!("created qmd collection {name}");
            }
        }
        Err(error) => {
            eprintln!("warning: qmd collections not created: {error}");
            eprintln!("run `kbx doctor --fix` to try again");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn read_ontology(project: &Project) -> Result<String, String> {
    fs::read_to_string(&project.ontology)
        .map_err(|error| format!("{}: {error}", project.ontology.display()))
}

fn run_index(options: IndexOptions) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let ontology = read_ontology(&project)?;
    let mut graph = Graph::open(&project.graph)?;
    let report = index_vault(&mut graph, &project.vault, &ontology, &options)?;
    print_index_report(&report);
    Ok(ExitCode::SUCCESS)
}

fn print_index_report(report: &IndexReport) {
    println!(
        "indexed {}, {} unchanged, removed {}, {} skipped, {} failed, {} hidden links",
        report.indexed,
        report.unchanged,
        report.removed,
        report.skipped,
        report.failed,
        report.hidden
    );
}

fn run_lint(
    format: LintFormat,
    strict: bool,
    paths: &[PathBuf],
) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    lint_project(&project, format, strict, paths)
}

fn lint_project(
    project: &Project,
    format: LintFormat,
    strict: bool,
    paths: &[PathBuf],
) -> Result<ExitCode, Box<dyn Error>> {
    let ontology = read_ontology(project)?;
    let findings = lint_vault(&project.vault, &ontology, paths)?;
    let severity = |finding: &Finding| match finding.code.severity() {
        Severity::Warning if strict => Severity::Error,
        severity => severity,
    };
    let rated: Vec<_> = findings.iter().map(|f| (f, severity(f))).collect();
    let errors = rated.iter().filter(|(_, s)| *s == Severity::Error).count();
    let warnings = rated.len() - errors;
    match format {
        LintFormat::Text => print_findings(&rated, errors, warnings),
        LintFormat::Json => {
            let rows: Vec<_> = rated.iter().map(|(f, s)| finding_json(f, *s)).collect();
            let report = json!({ "findings": rows, "errors": errors, "warnings": warnings });
            println!("{report}");
        }
    }
    Ok(if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn print_findings(rated: &[(&Finding, Severity)], errors: usize, warnings: usize) {
    for (finding, severity) in rated {
        println!(
            "{severity}: {}:{}: {}: {}",
            finding.path.display(),
            finding.line,
            finding.code,
            finding.message
        );
    }
    if rated.is_empty() {
        println!("no findings");
    } else {
        println!(
            "{}, {}",
            plural(errors, "error"),
            plural(warnings, "warning")
        );
    }
}

fn plural(count: usize, noun: &str) -> String {
    let ending = if count == 1 { "" } else { "s" };
    format!("{count} {noun}{ending}")
}

fn finding_json(finding: &Finding, severity: Severity) -> serde_json::Value {
    json!({
        "code": finding.code.as_str(),
        "severity": severity.as_str(),
        "path": finding.path.to_string_lossy(),
        "line": finding.line,
        "message": finding.message,
    })
}

fn run_graph(query: GraphQuery) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let graph = Graph::open(&project.graph)?;
    match query {
        GraphQuery::Neighbors { id } => {
            find_node(&graph, &id)?;
            for neighbor in graph.neighbors(&id)? {
                println!("{}", hop(&neighbor));
            }
        }
        GraphQuery::Path { from, to } => {
            let start = find_node(&graph, &from)?;
            find_node(&graph, &to)?;
            let hops = graph
                .path(&from, &to)?
                .ok_or_else(|| format!("no path from {from} to {to}"))?;
            println!("{}", label(&start));
            for neighbor in &hops {
                println!("  {}", hop(neighbor));
            }
        }
        GraphQuery::Stats => {
            let stats = graph.stats()?;
            println!("nodes {}", stats.nodes);
            println!("edges {}", stats.edges);
            println!("sections {}", stats.sections);
            println!("hidden links {}", stats.hidden);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn find_node(graph: &Graph, id: &str) -> Result<Node, Box<dyn Error>> {
    let node = graph.node(id)?;
    Ok(node.ok_or_else(|| format!("no note with id {id} in the index; run `kbx index`"))?)
}

fn label(node: &Node) -> String {
    format!("{} ({}) {}", node.id, node.class, node.title)
}

/// An edge as `-rel-> note`, or `<-rel- note` when it points this way.
fn hop(neighbor: &Neighbor) -> String {
    let (start, end) = if neighbor.incoming {
        ("<-", "-")
    } else {
        ("-", "->")
    };
    let weight = neighbor
        .weight
        .map(|w| format!(", {w}"))
        .unwrap_or_default();
    format!(
        "{start}{}{end} {} [{}{weight}]",
        neighbor.rel,
        label(&neighbor.node),
        neighbor.origin
    )
}
