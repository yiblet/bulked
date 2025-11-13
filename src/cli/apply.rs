use std::fmt::Write;
use std::io::{self, Read};
use std::path::PathBuf;

use clap::Args;

use crate::apply::apply_format_to_fs;
use crate::filesystem;
use crate::format::Format;

#[derive(Args, Debug)]
pub(super) struct ApplyArgs {
    /// Input file containing the format to apply (reads from stdin if not specified)
    #[arg(short, long)]
    input: Option<PathBuf>,

    #[arg(short, long)]
    dry_run: bool,
}

pub(super) fn handle_apply(args: ApplyArgs) -> Result<(), String> {
    // Read format from input file or stdin
    let input = match args.input {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                return Err(format!("Error reading file '{}': {}", path.display(), e));
            }
        },
        None => {
            let mut buffer = String::new();
            match io::stdin().read_to_string(&mut buffer) {
                Ok(_) => buffer,
                Err(e) => {
                    return Err(format!("Error reading from stdin: {}", e));
                }
            }
        }
    };

    // Parse the format
    let mut format = match input.parse::<Format>() {
        Ok(format) => format,
        Err(e) => {
            return Err(format!("Error parsing format: {}", e));
        }
    };

    if !args.dry_run {
        // Apply the format to the filesystem
        let mut fs = filesystem::physical::PhysicalFS;
        match apply_format_to_fs(&mut format, &mut fs) {
            Ok(()) => {
                println!("Successfully applied changes to {} chunks", format.len());
            }
            Err(errors) => {
                let mut message = String::new();
                let _ = writeln!(&mut message, "Errors occurred while applying changes:");
                for error in errors {
                    let _ = writeln!(&mut message, "  - {}", error);
                }
                let _ = write!(&mut message, "Failed to apply changes");
                return Err(message);
            }
        }
    } else {
        format.file_chunks().into_iter().for_each(|(path, chunks)| {
            println!("Would apply {} chunks to {}", chunks.len(), path.display());
        });
    }

    Ok(())
}
