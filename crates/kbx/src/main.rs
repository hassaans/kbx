use std::env;
use std::error::Error;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kbx_core::{Graph, IndexOptions, Project, index_vault, init};
use tracing_subscriber::EnvFilter;

/// Turn a markdown vault into a typed knowledge graph.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create .kbx/, kbx.toml and the example ontology.
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = EnvFilter::try_from_env("KBX_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let result = match cli.command {
        Command::Init { ontology } => run_init(ontology),
        Command::Index { full, paths } => run_index(IndexOptions { full, paths }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_init(ontology: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    let dir = env::current_dir()?;
    let report = init(&dir, ontology.as_deref())?;
    if report.config {
        println!("created kbx.toml");
    }
    if report.ontology {
        println!("created .kbx/ontology.yaml");
    }
    match find_on_path("qmd") {
        Some(path) => println!("found qmd at {}", path.display()),
        None => println!("qmd is not on PATH"),
    }
    Ok(())
}

fn run_index(options: IndexOptions) -> Result<(), Box<dyn Error>> {
    let project = Project::discover(&env::current_dir()?)?;
    let ontology = fs::read_to_string(&project.ontology)
        .map_err(|error| format!("{}: {error}", project.ontology.display()))?;
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
    Ok(())
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let names = [
        program.to_string(),
        format!("{program}.exe"),
        format!("{program}.cmd"),
    ];
    let folders = env::split_paths(&env::var_os("PATH")?).collect::<Vec<_>>();
    folders
        .iter()
        .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
        .find(|path| path.is_file())
}
