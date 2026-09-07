use clap::{Parser, Subcommand};

mod apply;
mod error;
mod ingest;
mod search;

pub(crate) use self::apply::ApplyArgs;
pub(crate) use self::ingest::IngestArgs;
use self::search::SearchArgs;
pub use error::Error;

/// bulked (Bulk Editor): edit many files at once through one plain-text file.
#[derive(Parser, Debug)]
#[command(name = "bulked")]
#[command(about = "Edit many files at once: collect lines as text, edit the text, apply it back")]
#[command(long_about = "\
bulked (Bulk Editor) edits many files at once. Collect the lines you want to \
change into one plain-text file of `chunks`, edit that file however you like, \
then apply it back to every source file in a single atomic step.")]
#[command(after_long_help = r#"WORKFLOW
  1. Collect   bulked search PATTERN [PATHS]   find the lines yourself, or
               TOOL | bulked ingest            reuse any tool's path:line output
  2. Edit      change the text inside the chunks (editor, script, or LLM)
  3. Apply     bulked apply -i FILE            validate, then write it all back

EXAMPLE
  grep -rn 'TODO' src/ | bulked ingest > edits.bk  # or: bulked search TODO src/
  $EDITOR edits.bk
  bulked apply -i edits.bk --dry-run                # preview the diff
  bulked apply -i edits.bk

CHUNK FORMAT
  @src/main.rs:10:3 #9f2c1e07     <- @path:start-line:line-count #fingerprint
  fn main() {
      println!("hello");
  }
  @@@

  Edit anything between the header and `@@@`; add or remove lines freely.
  Text outside chunks is ignored. See `bulked apply --help` for the full rules.

EXIT STATUS
  0 output produced   1 nothing to do (no matches, empty input)   2 error"#)]
#[command(version)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Log debug details to stderr
    #[arg(short, long, global = true, display_order = 998)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Find regex matches and print them as editable chunks
    Search(SearchArgs),
    /// Turn another tool's path:line output into editable chunks
    Ingest(IngestArgs),
    /// Validate edited chunks and write them back to the files
    Apply(ApplyArgs),
}

/// Process exit status, following the grep convention: 0 when the command
/// produced output, 1 when it ran cleanly but had nothing to produce (no matches,
/// no locations), and 2 (mapped in `main`) when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Output was produced.
    Ok,
    /// Nothing to do: no matches, no locations, empty input.
    Nothing,
}

impl Exit {
    /// The process exit code for this outcome.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Nothing => 1,
        }
    }
}

/// `1 chunk`, `2 chunks`: a count with its noun, pluralized with a plain `s`.
pub(crate) fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

pub fn run() -> Result<Exit, Error> {
    let cli = Cli::parse();

    // Initialize tracing based on verbosity. Logs always go to stderr so that
    // stdout stays reserved for the chunk format and status output, and they are
    // formatted as plain `LEVEL message` lines: a CLI user does not need
    // timestamps or module paths.
    let level = if cli.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .init();

    match cli.command {
        Command::Ingest(args) => args.handle(),
        Command::Search(args) => args.handle(),
        Command::Apply(args) => args.handle(),
    }
}
