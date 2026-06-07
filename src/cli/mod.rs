use clap::{Parser, Subcommand};

mod apply;
mod error;
mod ingest;
mod search;

use crate::cli::ingest::IngestArgs;

use self::apply::ApplyArgs;
use self::search::SearchArgs;
pub use error::Error;

/// bulked (Bulk Editor) - edit many files at once through a plain-text format
#[derive(Parser, Debug)]
#[command(name = "bulked")]
#[command(
    about = "bulked (Bulk Editor): turn search results into editable text, then apply your edits back"
)]
#[command(long_about = "\
bulked (Bulk Editor) lets you make edits across many files at once by treating \
search results as a single editable document. You collect the lines you want to \
change, edit them as one plain-text file, then apply the edits back to every \
source file in one shot.")]
#[command(after_long_help = "\
THE CORE FLOW:

  1. INGEST  — collect the lines you want to edit and print them, with context,
               as an editable text format. Pipe in output from any tool
               (ripgrep, compiler errors, a CSV of locations, ...), or use
               `bulked search` to find matches yourself.

  2. EDIT    — open that text in your editor (or pipe it through a script / an
               LLM) and change the content however you like. Each block is a
               `chunk` tagged with its file path and line range.

  3. APPLY   — feed the edited text back to `bulked apply`. bulked validates the
               chunks and writes every change back to the right place in every
               file in one shot.

QUICK START:

  # 1. Find the lines you care about and save them to a file
  grep -rn 'TODO' src/ | bulked ingest > edits.bk

  # (or search directly with bulked)
  bulked search 'TODO' src/ > edits.bk

  # 2. Edit edits.bk in your editor — change the content inside the chunks

  # 3. Preview, then apply your changes back to the files
  bulked apply --input edits.bk --dry-run
  bulked apply --input edits.bk

THE CHUNK FORMAT:

  Each editable block looks like this:

      @src/main.rs:10:3
      fn main() {
          println!(\"hello\");
      }
      @@@

  The header is `@<path>:<start-line>:<num-lines>`. Edit the lines between the
  header and the closing `@@@`; everything outside chunks is treated as comments
  and ignored. Run `bulked search --help` or `bulked apply --help` for details.")]
#[command(version)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Enable verbose logging (DEBUG-level) to stderr
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// STEP 1: turn a stream of (path, line) locations into the editable format
    Ingest(IngestArgs),
    /// STEP 1 (alt): find regex matches yourself and emit the editable format
    Search(SearchArgs),
    /// STEP 3: validate edited chunks and write the changes back to your files
    Apply(ApplyArgs),
}

pub fn run() -> Result<(), Error> {
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

    match cli.command {
        Command::Ingest(args) => args.handle(),
        Command::Search(args) => args.handle(),
        Command::Apply(args) => args.handle(),
    }
}
