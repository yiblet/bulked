use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::path::PathBuf;

use clap::Args;

use crate::cli::Exit;
use crate::execute::{Execute, ExecuteConfig};
use crate::format::Format;

#[derive(Args, Debug)]
#[command(
    after_long_help = r#"Recursively searches PATHS for PATTERN and prints each match, with context, as
an editable chunk for `bulked apply`. Respects .gitignore and skips hidden files
and bulked's own .bk files unless told otherwise.

EXAMPLES
  bulked search 'TODO' src/ > edits.bk      # save the chunks (or: -o edits.bk)
  bulked search 'fn main' -C 5 --hidden     # less context, include dotfiles
  bulked search 'TODO' src/ --plain         # readable listing, not for apply

Then edit edits.bk and run `bulked apply -i edits.bk`."#
)]
pub(crate) struct SearchArgs {
    /// Regular expression to search for
    pattern: String,

    /// Files or directories to search
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Write the chunks to this file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Lines of context before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// Also search files excluded by .gitignore
    #[arg(long)]
    no_ignore: bool,

    /// Also search hidden files and directories
    #[arg(long)]
    hidden: bool,

    /// Also search .bk files (bulked's own output)
    #[arg(long)]
    include_bk: bool,

    /// Print a readable listing instead of chunks (cannot be applied)
    #[arg(long)]
    plain: bool,
}

impl SearchArgs {
    /// Run `search`, writing the chunk format to `out` and status lines to `err`.
    /// Returns [`Exit::Nothing`] when there were no matches (like `grep`).
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
    ) -> Result<Exit, super::Error> {
        // Configure and execute search
        let pattern = self.pattern.clone();
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
            writeln!(
                err,
                "bulked search wrote {} to {}",
                super::plural(chunks, "chunk"),
                path.display()
            )?;
        }

        if chunks == 0 {
            writeln!(err, "bulked search: no matches for {pattern:?}")?;
            return Ok(Exit::Nothing);
        }
        Ok(Exit::Ok)
    }

    pub fn handle(self, global: super::GlobalArgs) -> Result<Exit, super::Error> {
        let mut stderr = io::stderr();
        match self.output.clone() {
            // A file is never a terminal, so `--color auto` never colors it.
            Some(path) => {
                let mut file = BufWriter::new(File::create(path)?);
                self.run(&mut file, &mut stderr, global.color.enabled(false))
            }
            None => {
                let color = global.color.enabled(io::stdout().is_terminal());
                self.run(&mut io::stdout(), &mut stderr, color)
            }
        }
    }
}
