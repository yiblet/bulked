use clap::{Parser, Subcommand};

mod apply;
mod search;

use self::apply::{ApplyArgs, handle_apply};
use self::search::{SearchArgs, handle_search};

/// Bulked - Search and modify code with context
///
/// A tool for searching code with context and applying modifications.
#[derive(Parser, Debug)]
#[command(name = "bulked")]
#[command(about = "Search and modify code with context", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Search for regex patterns in files with surrounding context
    Search(SearchArgs),
    /// Apply modifications from a format file to the filesystem
    Apply(ApplyArgs),
}

pub fn run() -> Result<(), String> {
    let cli = Cli::parse();

    // Initialize tracing based on verbosity
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .init();
    }

    match cli.command {
        Command::Search(args) => handle_search(args),
        Command::Apply(args) => handle_apply(args),
    }
}
