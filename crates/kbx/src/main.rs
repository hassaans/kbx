mod doctor;
mod qmd;

use std::env;
use std::error::Error;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use kbx_core::{
    Finding, Graph, IndexOptions, Neighbor, Node, Project, index_vault, init, lint_vault,
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
  link.unstable    a link resolves by filename, not id
  link.unresolved  a link resolves to no note, or to more than one
  note.invalid     a file with frontmatter can't be read as a note, so `kbx index`
                   leaves it out
  id.duplicate     several files claim one id; the lowest path keeps it, and links
                   and `kbx graph` go there

Every finding is an error. Plain markdown without frontmatter is not a note.
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
    /// Create .kbx/, kbx.toml and the example ontology, and install qmd if
    /// it's missing.
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
    /// Check notes against the ontology. Exits 1 if there are findings.
    #[command(after_help = LINT_CODES)]
    Lint {
        /// How to print the findings.
        #[arg(long, value_enum, default_value_t = LintFormat::Text)]
        format: LintFormat,
        /// Report on these files and folders alone, such as the ones you
        /// changed. They are still checked against the whole vault.
        paths: Vec<PathBuf>,
    },
    /// Query the index.
    Graph {
        #[command(subcommand)]
        query: GraphQuery,
    },
    /// Check the vault, the index, RAM and qmd, installing qmd if it's missing.
    Doctor,
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
        Command::Index { full, paths } => run_index(IndexOptions { full, paths }),
        Command::Lint { format, paths } => run_lint(format, &paths),
        Command::Graph { query } => run_graph(query),
        Command::Doctor => doctor::run(),
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
    println!(
        "indexed {}, {} unchanged, removed {}, {} skipped, {} failed, {} hidden links",
        report.indexed,
        report.unchanged,
        report.removed,
        report.skipped,
        report.failed,
        report.hidden
    );
    Ok(ExitCode::SUCCESS)
}

fn run_lint(format: LintFormat, paths: &[PathBuf]) -> Result<ExitCode, Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let ontology = read_ontology(&project)?;
    let findings = lint_vault(&project.vault, &ontology, paths)?;
    match format {
        LintFormat::Text => print_findings(&findings),
        LintFormat::Json => {
            let rows: Vec<_> = findings.iter().map(finding_json).collect();
            println!("{}", json!({ "findings": rows }));
        }
    }
    Ok(if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn print_findings(findings: &[Finding]) {
    for finding in findings {
        println!(
            "{}:{}: {}: {}",
            finding.path.display(),
            finding.line,
            finding.code,
            finding.message
        );
    }
    match findings.len() {
        0 => println!("no findings"),
        1 => println!("1 finding"),
        count => println!("{count} findings"),
    }
}

fn finding_json(finding: &Finding) -> serde_json::Value {
    json!({
        "code": finding.code.as_str(),
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
