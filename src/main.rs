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

fn main() {
    if let Err(err) = cli::run() {
        println!("{}", err); // print errors to stdout so logs stay in stderr
        std::process::exit(1);
    }
}
