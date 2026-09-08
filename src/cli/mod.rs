use std::io::IsTerminal;

use clap::{Args, Parser, Subcommand, ValueEnum};

mod apply;
mod error;
mod ingest;
mod refresh;
mod search;

pub(crate) use self::apply::ApplyArgs;
pub(crate) use self::ingest::IngestArgs;
pub(crate) use self::refresh::RefreshArgs;
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
               bulked refresh FILE             files changed since step 1? fix the
                                               stale fingerprints, then apply again

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

    #[command(flatten)]
    global: GlobalArgs,
}

/// Flags accepted by every subcommand, in any position (`global = true`), and
/// handed to each subcommand's `handle` so it can act on them.
#[derive(Args, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlobalArgs {
    /// Log debug details to stderr
    #[arg(short, long, global = true, display_order = 998)]
    pub verbose: bool,

    /// When to color output (match highlights, --dry-run diffs, error reports)
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        default_value = "auto",
        display_order = 999
    )]
    pub color: ColorChoice,
}

/// The `--color` flag, following the `grep`/`rg` convention.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Color only when the output is a terminal
    #[default]
    Auto,
    /// Always emit color codes, even into a file or pipe
    Always,
    /// Never emit color codes
    Never,
}

impl ColorChoice {
    /// Resolve the choice for one output sink, given whether that sink is a
    /// terminal. `handle` methods call this with the sink the colored text
    /// actually goes to (stdout, a file, stderr), so `Auto` never colors a file.
    #[must_use]
    pub fn enabled(self, sink_is_terminal: bool) -> bool {
        match self {
            Self::Auto => sink_is_terminal,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Find regex matches and print them as editable chunks
    Search(SearchArgs),
    /// Turn another tool's path:line output into editable chunks
    Ingest(IngestArgs),
    /// Validate edited chunks and write them back to the files
    Apply(ApplyArgs),
    /// Recompute stale chunk fingerprints from the files as they are now
    Refresh(RefreshArgs),
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
    let level = if cli.global.verbose {
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

    // Errors are rendered by `main` as miette reports on stderr; color them by
    // the same flag, resolved against stderr.
    install_error_reporting(cli.global.color.enabled(std::io::stderr().is_terminal()));

    match cli.command {
        Command::Ingest(args) => args.handle(cli.global),
        Command::Search(args) => args.handle(cli.global),
        Command::Apply(args) => args.handle(cli.global),
        Command::Refresh(args) => args.handle(cli.global),
    }
}

/// Configure how `main` renders a [`Error`] as a `miette::Report`: parse errors
/// show their source spans, and color is on or off as decided by `--color`.
fn install_error_reporting(color: bool) {
    // `set_hook` fails only if a hook is already installed, which happens when
    // `run` is called twice in one process (tests); the first hook then stands.
    let _ = miette::set_hook(Box::new(move |_| {
        Box::new(miette::MietteHandlerOpts::new().color(color).build())
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_flag_is_global_and_binds_after_the_subcommand() {
        // `global = true` lets clap accept the flag in any position and store it
        // on the parent `Cli`, so `bulked search --color always PAT` works.
        let cli = Cli::try_parse_from(["bulked", "search", "--color", "always", "TODO"]).unwrap();
        assert_eq!(cli.global.color, ColorChoice::Always);
        assert!(matches!(cli.command, Command::Search(_)));

        let cli = Cli::try_parse_from(["bulked", "--color", "never", "apply", "--dry-run", "-v"])
            .unwrap();
        assert_eq!(
            cli.global,
            GlobalArgs {
                verbose: true,
                color: ColorChoice::Never
            }
        );

        let cli = Cli::try_parse_from(["bulked", "refresh", "edits.bk"]).unwrap();
        assert_eq!(cli.global, GlobalArgs::default());

        assert!(Cli::try_parse_from(["bulked", "search", "--color", "sometimes", "x"]).is_err());
    }

    #[test]
    fn test_color_choice_resolves_against_the_sink() {
        assert!(ColorChoice::Auto.enabled(true));
        assert!(!ColorChoice::Auto.enabled(false));
        assert!(ColorChoice::Always.enabled(false));
        assert!(!ColorChoice::Never.enabled(true));
    }
}
