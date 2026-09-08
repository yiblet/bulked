use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use clap::Args;

use crate::cli::{Exit, plural};
use crate::diff::write_line_diff;
use crate::filesystem::FileSystem;
use crate::filesystem::physical::PhysicalFS;
use crate::refresh::{Refresh, RefreshKind, Refreshed};

#[derive(Args, Debug)]
#[command(
    after_long_help = r#"Fixes the chunks that `bulked apply` refuses because their lines changed since
the chunks were generated. A chunk you did not edit is reread from the file, so
it shows the current lines again. A chunk you edited keeps your edit and gets a
new fingerprint, so applying it overwrites the changed lines. Nothing else in
the file is touched. `--dry-run` prints what would change, as a diff.

To skip the check instead, use `bulked apply --force`.

EXAMPLES
  bulked refresh edits.bk --dry-run         # show old vs new chunks
  bulked refresh edits.bk                   # rewrite them in place
  cat edits.bk | bulked refresh | bulked apply"#
)]
pub(crate) struct RefreshArgs {
    /// Chunk file to refresh in place; omit or use '-' to read stdin and write stdout
    #[arg(default_value = None)]
    pub(crate) path: Option<PathBuf>,

    /// Write the refreshed chunks here instead of back into the input file
    #[arg(short, long)]
    pub(crate) output: Option<PathBuf>,

    /// Print a diff of the chunks that would change; write nothing
    #[arg(short, long)]
    pub(crate) dry_run: bool,
}

impl RefreshArgs {
    /// The chunk file to read through the filesystem, unless input is stdin.
    fn source(&self) -> Option<&Path> {
        self.path.as_deref().filter(|p| p.as_os_str() != "-")
    }

    /// Run `refresh` against an injected filesystem, input stream, and output sinks.
    ///
    /// The chunk file is read from `PATH` through `fs`, or from `input` when no
    /// path (or `-`) is given. The result goes to `--output` if set, otherwise back
    /// into `PATH`, otherwise to `out`. With `--dry-run` the diff of the chunks
    /// that would change goes to `out` instead and nothing is written. Status
    /// lines go to `err`, so stdout can carry the chunk format in the
    /// stdin-to-stdout case. `color` paints the `--dry-run` diff.
    ///
    /// Returns [`Exit::Nothing`] when no chunk was stale and the file was
    /// therefore not touched (in-place and `--dry-run`).
    pub fn run(
        self,
        fs: &dyn FileSystem,
        input: &mut dyn Read,
        out: &mut dyn Write,
        err: &mut dyn Write,
        color: bool,
    ) -> Result<Exit, super::Error> {
        let mut src = String::new();
        match self.source() {
            Some(path) => fs.read(path)?.read_to_string(&mut src)?,
            None => input.read_to_string(&mut src)?,
        };

        let result = crate::refresh::refresh(&src, fs)?;

        let described = self
            .source()
            .map_or_else(|| "the input".to_string(), |p| p.display().to_string());
        write_status(err, &result, &described, self.dry_run)?;

        if self.dry_run {
            write_diff(out, &result.refreshed, color)?;
            out.flush()?;
            return Ok(if result.refreshed.is_empty() {
                Exit::Nothing
            } else {
                Exit::Ok
            });
        }

        let in_place = self.output.is_none();
        match self.output.as_deref().or(self.source()) {
            // Rewriting the file we read: leave it untouched when nothing changed.
            Some(_) if in_place && result.refreshed.is_empty() => Ok(Exit::Nothing),
            Some(path) => {
                super::write_file_atomically(fs, path, |w| {
                    Ok(w.write_all(result.text.as_bytes())?)
                })?;
                Ok(Exit::Ok)
            }
            None => {
                out.write_all(result.text.as_bytes())?;
                out.flush()?;
                Ok(Exit::Ok)
            }
        }
    }

    pub fn handle(self, global: super::GlobalArgs) -> Result<Exit, super::Error> {
        if self.source().is_none() && io::stdin().is_terminal() {
            eprintln!(
                "bulked refresh: reading chunks from standard input; pass a .bk file to refresh it in place (Ctrl-D to finish)"
            );
        }
        // The diff only ever goes to stdout, so that is the terminal to check.
        let color = global
            .color
            .enabled(self.dry_run && io::stdout().is_terminal());
        self.run(
            &PhysicalFS,
            &mut io::stdin(),
            &mut io::stdout(),
            &mut io::stderr(),
            color,
        )
    }
}

/// One line per outcome.
fn describe(kind: RefreshKind) -> &'static str {
    match kind {
        RefreshKind::Reread => "reread from the file",
        RefreshKind::KeptEdit => "kept your edit; it overwrites lines that changed",
        RefreshKind::AlreadyApplied => "already applied",
    }
}

/// The summary and per-chunk lines for stderr.
fn write_status(
    err: &mut dyn Write,
    result: &Refresh,
    described: &str,
    dry_run: bool,
) -> io::Result<()> {
    if result.refreshed.is_empty() {
        if result.fingerprinted == 0 {
            writeln!(err, "bulked refresh: no fingerprints in {described}")?;
        } else {
            writeln!(
                err,
                "bulked refresh: all {} in {described} are current",
                plural(result.fingerprinted, "chunk")
            )?;
        }
        return Ok(());
    }

    let verb = if dry_run { "would update" } else { "updated" };
    writeln!(
        err,
        "bulked refresh: {verb} {} of {} in {described}",
        result.refreshed.len(),
        plural(result.chunks, "chunk")
    )?;
    for r in &result.refreshed {
        writeln!(
            err,
            "  {}:{}  #{} -> #{}  {}",
            r.path.display(),
            r.range,
            r.old_fingerprint,
            r.new_fingerprint,
            describe(r.kind)
        )?;
    }
    Ok(())
}

/// The `--dry-run` preview: for each chunk that would change, its old and new
/// header, then a line diff of the old content against the new when the content
/// itself changes.
fn write_diff(out: &mut dyn Write, refreshed: &[Refreshed], color: bool) -> io::Result<()> {
    for r in refreshed {
        let head = crate::refresh::header(&r.path, r.range);
        writeln!(out, "--- {head} #{}", r.old_fingerprint)?;
        writeln!(
            out,
            "+++ {head} #{}  ({})",
            r.new_fingerprint,
            describe(r.kind)
        )?;
        if r.kind == RefreshKind::Reread {
            write_line_diff(out, &r.old_content, &r.new_content, color)?;
        }
    }
    Ok(())
}
