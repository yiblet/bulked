use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
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
pub(crate) struct SearchArgs {
    /// Regex pattern to search for
    pattern: String,

    /// Directory or file to search (default: current directory)
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

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
    /// Run `search`, writing the chunk format to `out` and status lines to `err`.
    ///
    /// `search` keeps using [`Execute`] (the production composition root over
    /// the real filesystem and walker); only the output side is injected.
    /// `color` says whether to ANSI-highlight matches in `out`. When `--output`
    /// is set, [`SearchArgs::handle`] opens that file and passes it as `out`;
    /// `run` then reports a status line to `err`.
    pub fn run(
        self,
        out: &mut dyn Write,
        err: &mut dyn Write,
        color: bool,
    ) -> Result<(), super::Error> {
        // Configure and execute search
        let config = ExecuteConfig::new(self.pattern, self.paths)
            .with_context_lines(self.context)
            .with_respect_gitignore(!self.no_ignore)
            .with_hidden(self.hidden)
            .with_include_bk(self.include_bk);

        let result = Execute::new(&config)?;

        let mut chunks = 0;
        for page in result.search_iter() {
            let matches = page?;
            let format = Format::from_matches(&matches);
            chunks += format.len();
            write!(out, "{}", format.display(self.plain, color))?;
        }

        out.flush()?;

        // When the output went to a file, report a status line.
        if let Some(path) = &self.output {
            let plural = if chunks == 1 { "chunk" } else { "chunks" };
            writeln!(
                err,
                "bulked search wrote {} {} to {}",
                chunks,
                plural,
                path.display()
            )?;
        }

        Ok(())
    }

    pub fn handle(self) -> Result<(), super::Error> {
        let mut stderr = io::stderr();
        match self.output.clone() {
            // When writing to a file, never colorize (it's not a terminal).
            Some(path) => {
                let mut file = BufWriter::new(File::create(path)?);
                self.run(&mut file, &mut stderr, false)
            }
            None => {
                let color = io::stdout().is_terminal();
                self.run(&mut io::stdout(), &mut stderr, color)
            }
        }
    }
}
