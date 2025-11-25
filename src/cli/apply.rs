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

impl ApplyArgs {
    pub fn handle(self) -> Result<(), super::Error> {
        // Read format from input file or stdin
        let input = match self.input {
            Some(path) => std::fs::read_to_string(&path)?,
            None => {
                let mut buffer = String::new();
                io::stdin().read_to_string(&mut buffer)?;
                buffer
            }
        };

        // Parse the format
        let mut format = input.parse::<Format>()?;

        if !self.dry_run {
            // Apply the format to the filesystem
            let mut fs = filesystem::physical::PhysicalFS;
            apply_format_to_fs(&mut format, &mut fs).map_err(super::Error::ApplyMultiple)?;
            println!("Successfully applied changes to {} chunks", format.len());
        } else {
            format.file_chunks().into_iter().for_each(|(path, chunks)| {
                println!("Would apply {} chunks to {}", chunks.len(), path.display());
            });
        }

        Ok(())
    }
}
