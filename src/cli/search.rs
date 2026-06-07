use std::fs::File;
use std::io::{BufWriter, IsTerminal, Write};
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

By default it respects `.gitignore`, skips hidden files, and skips bulked's own
`.bk` output (so search never matches files it produced). The output is the same
chunk format `bulked apply` consumes.

EXAMPLES:
  # find matches and save the editable format (redirect, or -o)
  bulked search 'TODO' src/ > edits.bk
  bulked search 'TODO' src/ -o edits.bk

  # tighter context, include hidden files, ignore .gitignore
  bulked search 'fn main' . -C 5 --hidden --no-ignore

  # also search previously generated .bk files
  bulked search 'TODO' . --include-bk

  # human-readable view (not meant for `apply`)
  bulked search 'TODO' src/ --plain

Then edit edits.bk and run `bulked apply --input edits.bk`.")]
pub(super) struct SearchArgs {
    /// Regex pattern to search for
    pattern: String,

    /// Directory or file to search (default: current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Write the editable format to this file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Lines of context to include before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// Search files normally excluded by .gitignore
    #[arg(long)]
    no_ignore: bool,

    /// Include hidden files and directories in the search
    #[arg(long)]
    hidden: bool,

    /// Also search bulked's own `.bk` output files (excluded by default)
    #[arg(long)]
    include_bk: bool,

    /// Print human-readable text instead of the editable chunk format
    #[arg(long)]
    plain: bool,
}

impl SearchArgs {
    pub fn handle(self) -> Result<(), super::Error> {
        // Configure and execute search
        let config = ExecuteConfig::new(self.pattern, self.path)
            .with_context_lines(self.context)
            .with_respect_gitignore(!self.no_ignore)
            .with_hidden(self.hidden)
            .with_include_bk(self.include_bk);

        let result = Execute::new(&config)?;

        // When writing to a file, never colorize (it's not a terminal).
        let mut sink: Box<dyn Write> = match &self.output {
            Some(path) => Box::new(BufWriter::new(File::create(path)?)),
            None => Box::new(std::io::stdout()),
        };
        let is_tty = self.output.is_none() && std::io::stdout().is_terminal();

        let mut chunks = 0;
        for page in result.search_iter() {
            let result = page?;
            let format = Format::from_matches(&result.matches);
            chunks += format.len();
            write!(sink, "{}", format.display(self.plain, is_tty))?;
        }

        sink.flush()?;

        // When the output went to a file, report a status line to stderr.
        if let Some(path) = &self.output {
            let plural = if chunks == 1 { "chunk" } else { "chunks" };
            eprintln!(
                "bulked search wrote {} {} to {}",
                chunks,
                plural,
                path.display()
            );
        }

        Ok(())
    }
}
