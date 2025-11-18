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

impl SearchArgs {
    pub fn handle(self) -> Result<(), String> {
        let is_tty = atty::is(atty::Stream::Stdout);

        // Configure and execute search
        let config = ExecuteConfig::new(self.pattern, self.path)
            .with_context_lines(self.context)
            .with_respect_gitignore(!self.no_ignore)
            .with_hidden(self.hidden);

        let result = Execute::new(&config).map_err(|e| format!("Error: {}", e))?;

        for page in result.search_iter() {
            let result = page.map_err(|e| format!("Error: {}", e))?;
            let format = Format::from_matches(&result.matches);
            print!("{}", format.display(self.plain, is_tty))
        }

        Ok(())
    }
}
