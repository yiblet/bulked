//! Bulked - Recursive grep with context
//!
//! A tool for searching code with context and applying modifications.

// Internal modules
mod apply;
mod cli;
mod execute;
mod filesystem;
mod format;
mod ingest;
#[cfg(test)]
mod integration_tests;
mod matcher;
mod searcher;
mod types;
mod walker;

use std::io::IsTerminal;
use std::process::ExitCode;

/// Exit status when a command fails; 0 and 1 come from [`cli::Exit`].
const EXIT_ERROR: u8 = 2;

fn main() -> ExitCode {
    // Render errors as miette reports on stderr, so parse errors show their
    // source spans and stdout stays reserved for the chunk format and status
    // output. Color only when stderr is a terminal.
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .color(std::io::stderr().is_terminal())
                .build(),
        )
    }));

    match cli::run() {
        Ok(exit) => ExitCode::from(exit.code()),
        Err(err) => {
            eprintln!("{:?}", miette::Report::new(err));
            ExitCode::from(EXIT_ERROR)
        }
    }
}
