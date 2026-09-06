use std::io::{self, Read, Write};
use std::path::PathBuf;

use clap::Args;

use crate::apply::{apply_plan, verify_plan};
use crate::filesystem::FileSystem;
use crate::filesystem::physical::PhysicalFS;
use crate::format::Format;

#[derive(Args, Debug)]
#[command(after_long_help = "\
`apply` reads the (edited) chunk format produced by `bulked ingest` or
`bulked search`, checks that the chunks are valid, and writes each change back
into the right place in each file. Text outside chunks is ignored, so notes and
comments you leave in the file are harmless.

Before writing, every chunk is validated together (errors are reported all at
once, not one at a time): chunks must stay sorted, must not overlap, must point
at lines that exist in the file, must have a non-zero length, and their original
lines must still match the header fingerprint. If anything fails, nothing is
written.

THE CHUNK FORMAT:
  @path/to/file.rs:<start-line>:<num-lines> #<fingerprint>
  <the replacement content for those lines>
  @@@

  * The fingerprint (8 hex digits) is computed by ingest/search from the original
    lines. apply refuses the plan if those lines changed since — the file was
    edited, or this .bk was already applied. Hand-written chunks may omit it.

  * Use `@@@-` instead of `@@@` to mean \"no trailing newline at end of file\".
  * A content line may not start with `@`. If a line of content starts with `@`,
    `\\@` or `\\\\`, put one extra `\\` in front of it (ingest/search do this for
    you). Nothing mid-line is escaped.
  * You may add, remove, or change lines freely inside a chunk — the line count
    in the header describes the ORIGINAL lines being replaced.

EXAMPLES:
  # preview what would change, without touching anything
  bulked apply --input edits.bk --dry-run

  # apply the edits from a file
  bulked apply --input edits.bk

  # apply edits straight from a pipe
  bulked ingest locations.csv | my-edit-script | bulked apply")]
pub(crate) struct ApplyArgs {
    /// Edited chunk file to apply (reads from stdin if not specified)
    #[arg(short, long)]
    pub(crate) input: Option<PathBuf>,

    /// Validate and report what would change, without writing any files
    #[arg(short, long)]
    pub(crate) dry_run: bool,
}

impl ApplyArgs {
    /// Run `apply` against an injected filesystem, input stream, and output sink.
    ///
    /// This owns all of the subcommand's behavior; [`ApplyArgs::handle`] is the
    /// thin production wrapper that supplies `PhysicalFS`, stdin, and stdout.
    /// `--input` is read through `fs`, otherwise the chunk format is read from
    /// `input`. Every status line goes to `out`.
    pub fn run(
        self,
        fs: &dyn FileSystem,
        input: &mut dyn Read,
        out: &mut dyn Write,
    ) -> Result<(), super::Error> {
        let mut buffer = String::new();
        match &self.input {
            Some(path) => fs.read(path)?.read_to_string(&mut buffer)?,
            None => input.read_to_string(&mut buffer)?,
        };

        // Parse the format, then validate it into a plan: one group of sorted,
        // non-overlapping chunks per file. Every structural error is reported here.
        let format = buffer.parse::<Format>()?;
        let plan = format.validate()?;

        if self.dry_run {
            // Phase 1 only: verify every file (reads + reconstructs, writes nothing).
            verify_plan(&plan, fs)?;
            for edits in plan.files() {
                writeln!(
                    out,
                    "Would apply {} chunks to {}",
                    edits.chunks().len(),
                    edits.path().display()
                )?;
            }
        } else {
            apply_plan(&plan, fs)?;
            writeln!(
                out,
                "Successfully applied changes to {} chunks",
                plan.chunk_count()
            )?;
        }

        out.flush()?;
        Ok(())
    }

    pub fn handle(self) -> Result<(), super::Error> {
        self.run(&PhysicalFS, &mut io::stdin(), &mut io::stdout())
    }
}
