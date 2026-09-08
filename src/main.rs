//! Bulked - Recursive grep with context
//!
//! A tool for searching code with context and applying modifications.

// Internal modules
mod apply;
mod cli;
mod diff;
mod execute;
mod filesystem;
mod format;
mod ingest;
#[cfg(test)]
mod integration_tests;
mod matcher;
mod refresh;
mod searcher;
mod types;
mod walker;

use std::process::ExitCode;

/// Exit status when a command fails; 0 and 1 come from [`cli::Exit`].
const EXIT_ERROR: u8 = 2;

fn main() -> ExitCode {
    // Errors are rendered as miette reports on stderr (`cli::run` installs the
    // handler once it knows `--color`), so parse errors show their source spans
    // and stdout stays reserved for the chunk format and status output.
    match cli::run() {
        Ok(exit) => ExitCode::from(exit.code()),
        Err(err) => {
            eprintln!("{:?}", miette::Report::new(err));
            ExitCode::from(EXIT_ERROR)
        }
    }
}
