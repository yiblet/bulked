use std::path::PathBuf;

use clap::Args;

use crate::execute::{Execute, ExecuteConfig};
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
    let is_tty = atty::is(atty::Stream::Stdout);

    // Configure and execute search
    let config = ExecuteConfig::new(args.pattern, args.path)
        .with_context_lines(args.context)
        .with_respect_gitignore(!args.no_ignore)
        .with_hidden(args.hidden);

    let result = Execute::new(&config).map_err(|e| format!("Error: {}", e))?;

    if args.plain {
        // Output results with formatting
        for page in result.search_iter() {
            let result = page.map_err(|e| format!("Error: {}", e))?;

            for m in result.matches.iter() {
                println!("\n{}:{}", m.file_path.display(), m.line_number);

                // Context before
                for ctx in &m.context_before {
                    print!("  {:4} │ {}", ctx.line_number, ctx.content);
                }

                let (start_red, end_red) = if is_tty {
                    let start_red = "\x1b[31m";
                    let end_red = "\x1b[0m";
                    (start_red, end_red)
                } else {
                    ("", "")
                };

                if let Some(range) = &m.line_match {
                    // Highlight match
                    print!(
                        "  {:4} > {}{}{}{}{}",
                        m.line_number,
                        m.line_content.get(..range.start).unwrap_or_default(),
                        start_red,
                        &m.line_content[range.clone()],
                        end_red,
                        m.line_content.get(range.end..).unwrap_or_default()
                    );
                } else {
                    // Match line (highlighted with >)
                    print!("  {:4} > {}", m.line_number, m.line_content);
                }

                // Context after
                for ctx in &m.context_after {
                    print!("  {:4} │ {}", ctx.line_number, ctx.content);
                }
            }

            // Summary
            if result.matches.is_empty() {
                println!("\nNo matches found");
            }
        }
    } else {
        for page in result.search_iter() {
            let result = page.map_err(|e| format!("Error: {}", e))?;
            let format = Format::from_matches(&result.matches);

            if is_tty {
                print!("{}", format.highlight())
            } else {
                print!("{}", &format)
            }
        }
    };

    Ok(())
}
