// Phase 3: Full CLI interface with clap

use bulked::filesystem::physical::PhysicalFS;
use bulked::matcher::grep::GrepMatcher;
use bulked::matcher::Matcher;
use bulked::searcher::Searcher;
use bulked::types::SearchConfig;
use bulked::walker::ignore_walker::IgnoreWalker;
use clap::Parser;
use std::path::PathBuf;

/// Bulked - Recursive grep with context
///
/// Search for regex patterns in files with surrounding context lines.
#[derive(Parser, Debug)]
#[command(name = "bulked")]
#[command(about = "Recursive grep with context", long_about = None)]
#[command(version)]
struct Cli {
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

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
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

    // Create production adapters
    let fs = PhysicalFS::new();

    let matcher = match GrepMatcher::compile(&cli.pattern) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let walker = IgnoreWalker::new(&cli.path, !cli.no_ignore);

    let config = SearchConfig {
        pattern: cli.pattern.clone(),
        root_path: cli.path.clone(),
        context_lines: cli.context,
        respect_gitignore: !cli.no_ignore,
    };

    // Run search
    let searcher = Searcher::new(fs, matcher, walker, config);
    let result = searcher.search_all();

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
    if !result.matches.is_empty() {
        println!("\nFound {} matches", result.matches.len());
    }

    if !result.errors.is_empty() {
        eprintln!("{} errors encountered (use --verbose for details)", result.errors.len());
    }

    // Exit with appropriate code
    if result.matches.is_empty() && result.errors.is_empty() {
        std::process::exit(0);
    } else if !result.matches.is_empty() {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
