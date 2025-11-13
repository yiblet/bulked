use std::path::PathBuf;

use clap::Args;

use crate::execute::{ExecuteConfig, execute};
use crate::format::Format;

#[derive(Args, Debug)]
pub(super) struct SearchArgs {
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

pub(super) fn handle_search(args: SearchArgs) -> Result<(), String> {
    // Configure and execute search
    let config = ExecuteConfig::new(args.pattern, args.path)
        .with_context_lines(args.context)
        .with_respect_gitignore(!args.no_ignore)
        .with_hidden(args.hidden);

    let result = execute(&config).map_err(|e| format!("Error: {}", e))?;

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
        let format = Format::from_matches(&result.matches);
        print!("{}", &format)
    };

    Ok(())
}
