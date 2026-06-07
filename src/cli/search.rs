use std::io::IsTerminal;
use std::path::PathBuf;

use clap::Args;

use crate::execute::{Execute, ExecuteConfig};
use crate::format::Format;

#[derive(Args, Debug)]
#[command(after_long_help = "\
`search` is a grep-like recursive search that prints each match together with
surrounding context as an editable `chunk`. It's the self-contained way to start
a bulk edit when you want bulked to do the finding; if you'd rather feed in
another tool's output, use `bulked ingest` instead.

By default it respects `.gitignore` and skips hidden files, just like ripgrep.
The output is the same chunk format `bulked apply` consumes.

EXAMPLES:
  # find matches and save the editable format
  bulked search 'TODO' src/ > edits.bk

  # tighter context, include hidden files, ignore .gitignore
  bulked search 'fn main' . -C 5 --hidden --no-ignore

  # human-readable view (not meant for `apply`)
  bulked search 'TODO' src/ --plain

Then edit edits.bk and run `bulked apply --input edits.bk`.")]
pub(super) struct SearchArgs {
    /// Regex pattern to search for
    pattern: String,

    /// Directory or file to search (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Lines of context to include before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// Search files normally excluded by .gitignore
    #[arg(long)]
    no_ignore: bool,

    /// Include hidden files and directories in the search
    #[arg(long)]
    hidden: bool,

    /// Print human-readable text instead of the editable chunk format
    #[arg(long)]
    plain: bool,
}

impl SearchArgs {
    pub fn handle(self) -> Result<(), super::Error> {
        let is_tty = std::io::stdout().is_terminal();

        // Configure and execute search
        let config = ExecuteConfig::new(self.pattern, self.path)
            .with_context_lines(self.context)
            .with_respect_gitignore(!self.no_ignore)
            .with_hidden(self.hidden);

        let result = Execute::new(&config)?;

        for page in result.search_iter() {
            let result = page?;
            let format = Format::from_matches(&result.matches);
            print!("{}", format.display(self.plain, is_tty))
        }

        Ok(())
    }
}
