// Phase 1: Minimal main to verify compilation
// Full CLI will be implemented in Phase 3

use bulked::filesystem::physical::PhysicalFS;
use bulked::matcher::grep::GrepMatcher;
use bulked::matcher::Matcher;
use bulked::searcher::Searcher;
use bulked::types::SearchConfig;
use bulked::walker::ignore_walker::IgnoreWalker;
use std::path::PathBuf;

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Phase 1: Basic functionality test
    // Full CLI argument parsing will be added in Phase 3
    println!("Bulked - Recursive grep with context");
    println!("Phase 1: Core search engine implemented");
    println!();

    // For now, just show that the components can be wired together
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        println!("Usage: {} <pattern> [path]", args[0]);
        println!("Phase 3 will add full CLI interface");
        return;
    }

    let pattern = &args[1];
    let path = if args.len() > 2 {
        &args[2]
    } else {
        "."
    };

    // Create production adapters
    let fs = PhysicalFS::new();

    let matcher = match GrepMatcher::compile(pattern) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error compiling pattern: {}", e);
            std::process::exit(1);
        }
    };

    let walker = IgnoreWalker::new(path, true);

    let config = SearchConfig {
        pattern: pattern.to_string(),
        root_path: PathBuf::from(path),
        context_lines: 0, // Phase 2 will add context
        respect_gitignore: true,
    };

    // Run search
    let searcher = Searcher::new(fs, matcher, walker, config);
    let result = searcher.search_all();

    // Simple output (Phase 3 will add proper formatting)
    for m in &result.matches {
        println!("{}:{}: {}", m.file_path.display(), m.line_number, m.line_content);
    }

    if result.errors.len() > 0 {
        eprintln!("\n{} errors encountered", result.errors.len());
    }

    println!("\nFound {} matches", result.matches.len());
}
