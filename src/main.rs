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

fn main() {
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

    if let Err(err) = cli::run() {
        eprintln!("{:?}", miette::Report::new(err));
        std::process::exit(1);
    }
}
