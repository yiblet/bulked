use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use clap::Args;

use crate::apply::{apply_plan, verify_plan, write_preview};
use crate::cli::{Exit, plural};
use crate::filesystem::FileSystem;
use crate::filesystem::physical::PhysicalFS;
use crate::format::Format;

#[derive(Args, Debug)]
#[command(
    after_long_help = r#"Reads edited chunks from `bulked search` or `bulked ingest`, validates all of
them, then rewrites every file in one atomic step. If any chunk fails, nothing
is written: chunks must not overlap, must point at lines the file has, and their
fingerprint must still match the file (so a stale or already-applied .bk is
refused; --force skips this check). All problems are reported at once. Text
outside chunks is ignored.

CHUNK FORMAT
  @path/to/file.rs:<start-line>:<line-count> #<fingerprint>
  replacement text for those lines
  @@@

  * The header counts the ORIGINAL lines; the replacement may be any length.
  * `@@@-` instead of `@@@` means the file has no trailing newline.
  * A content line may not start with `@`. Put one extra `\` in front of a line
    that starts with `@`, `\@` or `\\` (search/ingest do this for you).
  * Hand-written chunks may omit the fingerprint.

EXAMPLES
  bulked apply -i edits.bk --dry-run        # print the diff, write nothing
  bulked apply -i edits.bk
  bulked apply -i edits.bk --force           # overwrite lines that changed since ingest
  bulked ingest locations.csv | my-script | bulked apply"#
)]
pub(crate) struct ApplyArgs {
    /// Edited chunk file to apply (default: stdin)
    #[arg(short, long)]
    pub(crate) input: Option<PathBuf>,

    /// Validate and print the diff; write nothing
    #[arg(short, long)]
    pub(crate) dry_run: bool,

    /// Ignore the header fingerprints: overwrite the lines even if they changed
    /// since the chunks were generated
    #[arg(short, long)]
    pub(crate) force: bool,
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
    ) -> Result<Exit, super::Error> {
        let mut buffer = String::new();
        match &self.input {
            Some(path) => fs.read(path)?.read_to_string(&mut buffer)?,
            None => input.read_to_string(&mut buffer)?,
        };

        // Parse the format, then validate it into a plan: one group of sorted,
        // non-overlapping chunks per file. Every structural error is reported here.
        let format = buffer.parse::<Format>()?;
        // `--force`: a chunk with no fingerprint is applied unchecked, so dropping
        // the fingerprints is exactly "overwrite regardless of what is there".
        let format = if self.force {
            format.without_fingerprints()
        } else {
            format
        };
        let plan = format.validate()?;

        if self.dry_run {
            // Phase 1 only: verify every file (reads + reconstructs, writes nothing),
            // then show what would change as a diff.
            verify_plan(&plan, fs)?;
            for (path, counts) in write_preview(&plan, fs, out)? {
                if counts.changed == 0 {
                    writeln!(
                        out,
                        "Nothing to change in {} ({} already applied)",
                        path.display(),
                        plural(counts.unchanged, "chunk")
                    )?;
                } else if counts.unchanged == 0 {
                    writeln!(
                        out,
                        "Would apply {} to {}",
                        plural(counts.changed, "chunk"),
                        path.display()
                    )?;
                } else {
                    writeln!(
                        out,
                        "Would apply {} to {} ({} unchanged)",
                        plural(counts.changed, "chunk"),
                        path.display(),
                        counts.unchanged
                    )?;
                }
            }
        } else {
            apply_plan(&plan, fs)?;
            writeln!(
                out,
                "Applied {} to {}",
                plural(plan.chunk_count(), "chunk"),
                plural(plan.files().len(), "file")
            )?;
        }

        out.flush()?;
        Ok(Exit::Ok)
    }

    pub fn handle(self) -> Result<Exit, super::Error> {
        if self.input.is_none() && io::stdin().is_terminal() {
            eprintln!(
                "bulked apply: reading chunks from standard input; pass --input edits.bk or pipe a .bk file (Ctrl-D to finish)"
            );
        }
        self.run(&PhysicalFS, &mut io::stdin(), &mut io::stdout())
    }
}
