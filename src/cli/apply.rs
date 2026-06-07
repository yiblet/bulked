use std::io::{self, Read};
use std::path::PathBuf;

use clap::Args;

use crate::apply::{apply_format_to_fs, verify_format_to_fs};
use crate::filesystem;
use crate::format::Format;

#[derive(Args, Debug)]
#[command(after_long_help = "\
`apply` reads the (edited) chunk format produced by `bulked ingest` or
`bulked search`, checks that the chunks are valid, and writes each change back
into the right place in each file. Text outside chunks is ignored, so notes and
comments you leave in the file are harmless.

Before writing, every chunk is validated together (errors are reported all at
once, not one at a time): chunks must stay sorted, must not overlap, must point
at lines that exist in the file, and must have a non-zero length. If anything
fails, nothing is written.

THE CHUNK FORMAT:
  @path/to/file.rs:<start-line>:<num-lines>
  <the replacement content for those lines>
  @@@

  * Use `@@@-` instead of `@@@` to mean \"no trailing newline at end of file\".
  * Inside content, write `\\@` for a literal `@` and `\\\\` for a literal `\\`.
  * You may add, remove, or change lines freely inside a chunk — the line count
    in the header describes the ORIGINAL lines being replaced.

EXAMPLES:
  # preview what would change, without touching anything
  bulked apply --input edits.bk --dry-run

  # apply the edits from a file
  bulked apply --input edits.bk

  # apply edits straight from a pipe
  bulked ingest locations.csv | my-edit-script | bulked apply")]
pub(super) struct ApplyArgs {
    /// Edited chunk file to apply (reads from stdin if not specified)
    #[arg(short, long)]
    input: Option<PathBuf>,

    /// Validate and report what would change, without writing any files
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

        let fs = filesystem::physical::PhysicalFS;
        if self.dry_run {
            // Phase 1 only: verify every file (reads + reconstructs, writes nothing).
            verify_format_to_fs(&mut format, &fs).map_err(super::Error::ApplyMultiple)?;
            format.file_chunks().into_iter().for_each(|(path, chunks)| {
                println!("Would apply {} chunks to {}", chunks.len(), path.display());
            });
        } else {
            apply_format_to_fs(&mut format, &fs).map_err(super::Error::ApplyMultiple)?;
            println!("Successfully applied changes to {} chunks", format.len());
        }

        Ok(())
    }
}
