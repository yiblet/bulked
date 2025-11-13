// Phase 3: Full CLI interface with clap

use bulked::{ExecuteConfig, Format, apply_format_to_fs, execute};
use clap::{Args, Parser, Subcommand};
use std::io::{self, Read};
use std::path::PathBuf;

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

#[derive(Args, Debug)]
struct SearchArgs {
    /// Regex pattern to search for
    pattern: String,

    /// Directory or file to search (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Lines of context before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// Don't respect .gitignore files
    #[arg(long)]
    no_ignore: bool,

    /// Include hidden files in search
    #[arg(long)]
    hidden: bool,

    /// output as plain text (human-readable format)
    #[arg(long)]
    plain: bool,
}

#[derive(Args, Debug)]
struct ApplyArgs {
    /// Input file containing the format to apply (reads from stdin if not specified)
    #[arg(short, long)]
    input: Option<PathBuf>,
}

fn main() {
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

fn handle_search(args: SearchArgs) {
    // Configure and execute search
    let config = ExecuteConfig::new(args.pattern, args.path)
        .with_context_lines(args.context)
        .with_respect_gitignore(!args.no_ignore)
        .with_hidden(args.hidden);

    let result = match execute(&config) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    if args.plain {
        // Output results with formatting
        for m in &result.matches {
            println!("\n{}:{}", m.file_path.display(), m.line_number);

            // Context before
            for ctx in &m.context_before {
                println!("  {:4} │ {}", ctx.line_number, ctx.content);
            }

            // Match line (highlighted with >)
            println!("  {:4} > {}", m.line_number, m.line_content);

            // Context after
            for ctx in &m.context_after {
                println!("  {:4} │ {}", ctx.line_number, ctx.content);
            }
        }
        // Summary
        if result.matches.is_empty() {
            println!("\nNo matches found");
        }
    } else {
        let format = bulked::Format::from_matches(&result.matches);
        print!("{}", &format)
    }
}

fn handle_apply(args: ApplyArgs) {
    // Read format from input file or stdin
    let input = match args.input {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Error reading file '{}': {}", path.display(), e);
                std::process::exit(1);
            }
        },
        None => {
            let mut buffer = String::new();
            match io::stdin().read_to_string(&mut buffer) {
                Ok(_) => buffer,
                Err(e) => {
                    eprintln!("Error reading from stdin: {}", e);
                    std::process::exit(1);
                }
            }
        }
    };

    // Parse the format
    let mut format = match input.parse::<Format>() {
        Ok(format) => format,
        Err(e) => {
            eprintln!("Error parsing format: {}", e);
            std::process::exit(1);
        }
    };

    // Apply the format to the filesystem
    let mut fs = bulked::filesystem::physical::PhysicalFS;
    match apply_format_to_fs(&mut format, &mut fs) {
        Ok(()) => {
            println!("Successfully applied changes to {} chunks", format.len());
        }
        Err(errors) => {
            eprintln!("Errors occurred while applying changes:");
            for error in errors {
                eprintln!("  - {}", error);
            }
            std::process::exit(1);
        }
    }
}
