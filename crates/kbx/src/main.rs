use clap::Parser;

/// Turn a markdown vault into a typed knowledge graph.
#[derive(Parser)]
#[command(version)]
struct Cli {}

fn main() {
    Cli::parse();
}
