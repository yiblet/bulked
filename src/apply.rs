use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

/// Apply a format to the filesystem.
///
/// "Parse, don't validate": every content-independent check lives in exactly one
/// place, [`Format::validate`], which turns a `Format` into a [`Plan`] — a proof
/// that the chunks are grouped by file, sorted, and non-overlapping. Everything
/// downstream takes a [`FileEdits`] (one file's validated chunks) and can only
/// fail on what it cannot know up front: the file's real length
/// ([`ApplyError::ChunkOutOfBounds`]), whether the lines a chunk replaces still
/// match the fingerprint recorded when it was generated
/// ([`ApplyError::ContentChanged`]), and I/O.
///
/// The reconstruction core is [`apply_format_streaming`], which reads the original
/// file, interleaves the chunk replacements, and writes the result — all with
/// bounded memory (a fixed read buffer; never a whole line or whole file).
///
/// Algorithm:
/// 1. `Format::validate`: group by path (a `Format` is sorted by construction) and
///    reject any consecutive pair in a file whose [`LineRange`]s overlap. Errors are
///    accumulated across all files.
/// 2. `verify_plan`: stream every file to a sink, accumulating bounds, fingerprint
///    and IO errors across all files. Nothing is written.
/// 3. `apply_plan`: `verify_plan`, then stage each reconstruction via a
///    [`StagingFs`] and commit all of them at once. Staging streams the original
///    again and re-checks every fingerprint against the very bytes it replaces, so
///    the commit only happens if nothing changed between the two passes.
use crate::{
    filesystem::{FileSystem, ReadFs, staging::StagingFs},
    format::{Chunk, Fingerprint, FingerprintHasher, Format, LineRange},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("{path}: overlapping chunks at lines {first} and {second}")]
    OverlappingChunks {
        path: PathBuf,
        first: LineRange,
        second: LineRange,
    },

    #[error("{path}: chunk at lines {range} exceeds file length of {file_lines} lines")]
    ChunkOutOfBounds {
        path: PathBuf,
        range: LineRange,
        file_lines: usize,
    },

    /// The lines a chunk replaces no longer match the fingerprint in its header:
    /// the file was edited after the chunk was generated, or the chunk was already
    /// applied. Either way the header's line numbers can no longer be trusted.
    #[error(
        "{path}: lines {range} changed since this chunk was generated (header fingerprint {expected}, file has {actual}); the file was edited, or this chunk was already applied"
    )]
    ContentChanged {
        path: PathBuf,
        range: LineRange,
        expected: Fingerprint,
        actual: Fingerprint,
    },

    #[error("Failed to modify file {path}: {source}")]
    ModifyError {
        path: PathBuf,
        #[source]
        source: crate::filesystem::FilesystemError,
    },

    #[error("I/O error while applying changes: {0}")]
    Io(#[source] std::io::Error),
}

/// Every [`ApplyError`] found while validating or applying a format.
///
/// Apply accumulates errors instead of failing fast, so the unit of failure is a
/// non-empty list. `Display` renders a header followed by one `  - <error>` line
/// per entry, which is exactly what the CLI shows the user.
#[derive(Debug, Error)]
#[error("Failed to apply changes:\n{}", .0.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n"))]
pub struct ApplyErrors(pub Vec<ApplyError>);

/// Shown under the error list when at least one chunk failed its fingerprint check.
pub const STALE_FINGERPRINT_HELP: &str = "if the files are right and the chunks are stale, \
`bulked refresh FILE` rereads those chunks from the files as they are now (keeping the ones you \
edited; `--dry-run` shows the difference); `bulked apply --force` overwrites the lines without checking";

impl miette::Diagnostic for ApplyErrors {
    /// A fingerprint mismatch is the one failure with a built-in next step, so it
    /// is the only one that gets a `help:` line.
    fn help(&self) -> Option<Box<dyn std::fmt::Display + '_>> {
        self.0
            .iter()
            .any(|e| matches!(e, ApplyError::ContentChanged { .. }))
            .then(|| Box::new(STALE_FINGERPRINT_HELP) as Box<dyn std::fmt::Display>)
    }
}

impl From<Vec<ApplyError>> for ApplyErrors {
    fn from(errors: Vec<ApplyError>) -> Self {
        Self(errors)
    }
}

impl From<ApplyError> for ApplyErrors {
    fn from(error: ApplyError) -> Self {
        Self(vec![error])
    }
}

impl IntoIterator for ApplyErrors {
    type Item = ApplyError;
    type IntoIter = std::vec::IntoIter<ApplyError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// One file's validated edits: every chunk is for `path`, the slice is sorted by
/// line, and no two chunks overlap.
///
/// The fields are private and this module never exposes a constructor, so the only
/// way to obtain a `FileEdits` is through [`Format::validate`]. Holding one is
/// proof that the structural checks passed.
#[derive(Debug)]
pub struct FileEdits<'a> {
    path: &'a Path,
    chunks: &'a [Chunk],
}

impl<'a> FileEdits<'a> {
    /// The file these edits apply to.
    pub fn path(&self) -> &'a Path {
        self.path
    }

    /// The chunks to apply, sorted by line and non-overlapping. Never empty.
    pub fn chunks(&self) -> &'a [Chunk] {
        self.chunks
    }
}

/// A validated apply plan: one [`FileEdits`] per distinct file, in path order.
///
/// Produced only by [`Format::validate`].
#[derive(Debug)]
pub struct Plan<'a>(Vec<FileEdits<'a>>);

impl<'a> Plan<'a> {
    /// The per-file edits, in path order.
    pub fn files(&self) -> &[FileEdits<'a>] {
        &self.0
    }

    /// Total number of chunks across all files.
    pub fn chunk_count(&self) -> usize {
        self.0.iter().map(|f| f.chunks.len()).sum()
    }
}

impl Format {
    /// Validates the format into a [`Plan`].
    ///
    /// Groups the (already sorted) chunks by path and rejects any pair of
    /// consecutive chunks in a file whose ranges share a line. Mixed paths and
    /// unsorted input are unrepresentable here: grouping makes every
    /// [`FileEdits`] single-path, and a `Format` is sorted by construction.
    ///
    /// This is the *only* place content-independent validation happens, and it
    /// accumulates every [`ApplyError::OverlappingChunks`] across all files
    /// instead of stopping at the first.
    ///
    /// # Errors
    /// Returns every overlap found, in (path, line) order.
    pub fn validate(&self) -> Result<Plan<'_>, ApplyErrors> {
        let mut errors = Vec::new();
        let mut files = Vec::new();

        for (path, chunks) in self.file_chunks() {
            for pair in chunks.windows(2) {
                let (first, second) = (pair[0].range(), pair[1].range());
                if first.overlaps(second) {
                    errors.push(ApplyError::OverlappingChunks {
                        path: path.to_path_buf(),
                        first,
                        second,
                    });
                }
            }
            files.push(FileEdits { path, chunks });
        }

        if errors.is_empty() {
            Ok(Plan(files))
        } else {
            Err(errors.into())
        }
    }
}

fn modify_err(path: &Path, source: crate::filesystem::FilesystemError) -> ApplyError {
    ApplyError::ModifyError {
        path: path.to_path_buf(),
        source,
    }
}

/// The bounds error for `range` against a file of `file_lines` lines, if any.
fn out_of_bounds(path: &Path, range: LineRange, file_lines: usize) -> Option<ApplyError> {
    (range.end_inclusive() > file_lines).then(|| ApplyError::ChunkOutOfBounds {
        path: path.to_path_buf(),
        range,
        file_lines,
    })
}

/// Compare the fingerprint of the original bytes a chunk replaced (`hasher`) with
/// the one recorded in its header, if any, and record a mismatch.
fn check_fingerprint(
    path: &Path,
    chunk: &Chunk,
    hasher: FingerprintHasher,
    errors: &mut Vec<ApplyError>,
) {
    let actual = hasher.finish();
    if let Some(expected) = chunk.fingerprint()
        && expected != actual
    {
        errors.push(ApplyError::ContentChanged {
            path: path.to_path_buf(),
            range: chunk.range(),
            expected,
            actual,
        });
    }
}

/// Size of the fixed read buffer used by [`apply_format_streaming`]. Reconstruction
/// memory is bounded by this regardless of file size or line length.
const STREAM_BUF_SIZE: usize = 64 * 1024;

/// Stream one file's reconstruction: read the original from `reader`, interleave
/// the chunk replacements from `edits`, and write the result to `writer`.
///
/// This is the reconstruction core and it only streams — `edits` is already proven
/// sorted, single-path, and non-overlapping by [`Format::validate`], so there is
/// nothing to check here except what the stream itself reveals. It uses a single
/// fixed-size read buffer and a byte-level state machine (copy original lines
/// through, or skip the lines a chunk replaces), so its memory use is approximately
/// constant per call — independent of the file size *and* of how long any
/// individual line is. Chunk content (already resident in the `Chunk`) is written
/// verbatim, preserving exact bytes including trailing-newline / no-trailing-newline
/// semantics.
///
/// The bytes skipped for each chunk are fed to a [`FingerprintHasher`] as they go
/// by; when the chunk's range ends, the result is compared with the fingerprint in
/// the chunk header (if it has one). Because this runs on the same read that
/// produces the output, a chunk whose original lines changed can never be written.
///
/// # Errors
/// Returns one [`ApplyError::ChunkOutOfBounds`] per chunk that references lines
/// past EOF and one [`ApplyError::ContentChanged`] per chunk whose original lines
/// no longer match their fingerprint (all of them, accumulated), or
/// [`ApplyError::Io`] on a read/write failure.
pub fn apply_format_streaming(
    edits: &FileEdits<'_>,
    mut reader: impl Read,
    writer: &mut dyn Write,
) -> Result<(), ApplyErrors> {
    let path = edits.path();
    let chunks = edits.chunks();
    let range_at = |i: usize| chunks[i].range();

    let mut buf = [0u8; STREAM_BUF_SIZE];
    let mut cur_line: usize = 1; // line number of the byte at the read cursor
    let mut idx = 0usize; // index of the next chunk to emit
    let mut skip_until: usize = 1; // we are skipping original lines while cur_line < skip_until
    let mut at_line_start = true;
    let mut any_bytes = false;
    // The chunk whose original lines are being skipped right now, with the running
    // fingerprint of the bytes skipped so far.
    let mut active: Option<(usize, FingerprintHasher)> = None;
    let mut errors: Vec<ApplyError> = Vec::new();

    let to_io = |e: std::io::Error| ApplyErrors::from(ApplyError::Io(e));

    // A chunk may start on line 1, before we have read anything.
    if idx < chunks.len() && range_at(idx).start() == cur_line {
        writer
            .write_all(chunks[idx].content().as_bytes())
            .map_err(to_io)?;
        skip_until = range_at(idx).end_exclusive();
        active = Some((idx, FingerprintHasher::new()));
        idx += 1;
    }

    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(to_io(e)),
        };
        any_bytes = true;
        let mut block = &buf[..n];
        while !block.is_empty() {
            let skipping = cur_line < skip_until;
            match block.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    if skipping {
                        if let Some((_, hasher)) = active.as_mut() {
                            hasher.update(&block[..=pos]);
                        }
                    } else {
                        writer.write_all(&block[..=pos]).map_err(to_io)?;
                    }
                    block = &block[pos + 1..];
                    cur_line += 1;
                    at_line_start = true;
                    if cur_line >= skip_until {
                        // The active chunk's original lines are all behind us.
                        if let Some((i, hasher)) = active.take() {
                            check_fingerprint(path, &chunks[i], hasher, &mut errors);
                        }
                        // Emit a chunk that begins at this new line.
                        if idx < chunks.len() && range_at(idx).start() == cur_line {
                            writer
                                .write_all(chunks[idx].content().as_bytes())
                                .map_err(to_io)?;
                            skip_until = range_at(idx).end_exclusive();
                            active = Some((idx, FingerprintHasher::new()));
                            idx += 1;
                        }
                    }
                }
                None => {
                    // No newline in the remaining block: it is all part of `cur_line`.
                    if skipping {
                        if let Some((_, hasher)) = active.as_mut() {
                            hasher.update(block);
                        }
                    } else {
                        writer.write_all(block).map_err(to_io)?;
                    }
                    at_line_start = false;
                    block = &[];
                }
            }
        }
    }

    // At EOF, count the file's lines the same way `split_inclusive('\n')` does.
    let file_lines = if !any_bytes {
        0
    } else if at_line_start {
        cur_line - 1
    } else {
        cur_line
    };

    // A chunk still active here ends on the file's last line, which has no
    // trailing newline (checked now), or runs past EOF (reported as out of bounds
    // below instead; its fingerprint is meaningless).
    if let Some((i, hasher)) = active.take()
        && range_at(i).end_inclusive() <= file_lines
    {
        check_fingerprint(path, &chunks[i], hasher, &mut errors);
    }

    errors.extend(
        chunks
            .iter()
            .filter_map(|c| out_of_bounds(path, c.range(), file_lines)),
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.into())
    }
}

/// Verify that a plan can be applied cleanly, without writing anything.
///
/// This is exactly phase 1 of an atomic apply (and the entire `--dry-run` path): it
/// streams every file through [`apply_format_streaming`] into a sink, accumulating
/// all bounds, fingerprint and IO errors across all files. Nothing is read into
/// memory whole and nothing is written.
///
/// # Errors
/// Returns every bounds/fingerprint/IO error found across all files.
pub fn verify_plan(plan: &Plan<'_>, fs: &dyn ReadFs) -> Result<(), ApplyErrors> {
    let mut errors = Vec::new();
    for edits in plan.files() {
        let path = edits.path();
        let result = fs
            .read(path)
            .map_err(|e| ApplyErrors::from(modify_err(path, e)))
            .and_then(|reader| apply_format_streaming(edits, reader, &mut std::io::sink()));
        if let Err(errs) = result {
            errors.extend(errs);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.into())
    }
}

/// How many of one file's chunks a `--dry-run` preview found changed vs. identical
/// to the lines they replace.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PreviewCounts {
    pub changed: usize,
    pub unchanged: usize,
}

/// Write a unified-diff-style preview of `plan` to `out`: for every chunk whose
/// content differs from the lines it replaces, a `@@ -start,len +start,len @@`
/// hunk holding a line diff of the original lines against the replacement
/// ([`crate::diff::write_line_diff`]: unchanged lines as context, `-`/`+` for the
/// rest, red and green when `color` is set). Chunks that would write back exactly
/// what is there are counted, not printed.
///
/// Meant to run after [`verify_plan`] has succeeded, so every range is known to be
/// in bounds and to match its fingerprint; it reads each file once more, line by
/// line, and never holds more than one chunk's original text.
///
/// # Errors
/// Returns the I/O errors encountered while reading the files or writing `out`.
pub fn write_preview<'a>(
    plan: &Plan<'a>,
    fs: &dyn ReadFs,
    out: &mut dyn Write,
    color: bool,
) -> Result<Vec<(&'a Path, PreviewCounts)>, ApplyErrors> {
    let io = |e: std::io::Error| ApplyErrors::from(ApplyError::Io(e));
    let mut summary = Vec::with_capacity(plan.files().len());

    for edits in plan.files() {
        let path = edits.path();
        let mut reader = BufReader::new(
            fs.read(path)
                .map_err(|e| ApplyErrors::from(modify_err(path, e)))?,
        );
        let mut counts = PreviewCounts::default();
        let mut line_no = 1usize; // 1-indexed line at the read cursor
        let mut delta = 0isize; // new-side minus old-side line count so far
        let mut buf = Vec::new();

        for chunk in edits.chunks() {
            // Skip forward to the chunk, then collect the lines it replaces.
            while line_no < chunk.start_line() {
                buf.clear();
                if reader.read_until(b'\n', &mut buf).map_err(io)? == 0 {
                    break;
                }
                line_no += 1;
            }
            let mut original = Vec::new();
            for _ in 0..chunk.num_lines() {
                buf.clear();
                if reader.read_until(b'\n', &mut buf).map_err(io)? == 0 {
                    break;
                }
                original.extend_from_slice(&buf);
                line_no += 1;
            }

            let new_lines = chunk.content().split_inclusive('\n').count();
            if original == chunk.content().as_bytes() {
                counts.unchanged += 1;
            } else {
                if counts.changed == 0 {
                    writeln!(out, "--- {}", path.display()).map_err(io)?;
                }
                counts.changed += 1;
                let new_start = chunk.start_line().saturating_add_signed(delta);
                writeln!(
                    out,
                    "@@ -{},{} +{},{} @@",
                    chunk.start_line(),
                    chunk.num_lines(),
                    new_start,
                    new_lines
                )
                .map_err(io)?;
                crate::diff::write_line_diff(
                    out,
                    &String::from_utf8_lossy(&original),
                    chunk.content(),
                    color,
                )
                .map_err(io)?;
            }
            delta += new_lines as isize - chunk.num_lines() as isize;
        }
        summary.push((path, counts));
    }

    Ok(summary)
}

/// Apply a plan to the filesystem atomically.
///
/// Two phases:
/// 1. **Verify** every file ([`verify_plan`]). If anything fails, nothing is
///    written, anywhere.
/// 2. **Commit**: stage each file's reconstruction into a temp file under
///    `temp_dir` via a [`StagingFs`], then move every temp into place. Staging streams with bounded
///    memory; an error during staging drops the `StagingFs`, deleting all temp files
///    and leaving every target untouched.
///
/// # Errors
/// Returns the accumulated errors from verification, or any I/O errors encountered
/// while staging or committing.
pub fn apply_plan(
    plan: &Plan<'_>,
    fs: &dyn FileSystem,
    temp_dir: &Path,
) -> Result<(), ApplyErrors> {
    // Phase 1: verify everything up front.
    verify_plan(plan, fs)?;

    // Phase 2: stage every file into a tracked temp file, then commit.
    let staging = StagingFs::new(fs, temp_dir);
    let mut errors = Vec::new();
    for edits in plan.files() {
        if let Err(errs) = stage_file(&staging, edits) {
            errors.extend(errs);
        }
    }
    if !errors.is_empty() {
        // `staging` drops here: every staged temp file is removed, targets untouched.
        return Err(errors.into());
    }

    staging.commit().map_err(|failures| {
        failures
            .into_iter()
            .map(|(path, source)| ApplyError::ModifyError { path, source })
            .collect::<Vec<_>>()
            .into()
    })
}

/// Stream one file's reconstruction into the staging filesystem.
fn stage_file(staging: &dyn FileSystem, edits: &FileEdits<'_>) -> Result<(), ApplyErrors> {
    let path = edits.path();
    let reader = staging
        .read(path)
        .map_err(|e| ApplyErrors::from(modify_err(path, e)))?;
    let mut writer = staging
        .writer(path)
        .map_err(|e| ApplyErrors::from(modify_err(path, e)))?;
    apply_format_streaming(edits, reader, &mut *writer)?;
    writer
        .flush()
        .map_err(|e| ApplyErrors::from(ApplyError::Io(e)))?;
    Ok(())
}

/// [`Format::validate`] then [`verify_plan`].
///
/// The CLI works from the `Plan` directly (it prints per-file counts), so this
/// wrapper only has test callers.
#[cfg(test)]
pub fn verify_format_to_fs(format: &Format, fs: &dyn FileSystem) -> Result<(), ApplyErrors> {
    verify_plan(&format.validate()?, fs)
}

/// [`Format::validate`] then [`apply_plan`].
///
/// The CLI works from the `Plan` directly (it prints the plan's chunk count), so
/// this wrapper only has test callers.
#[cfg(test)]
pub fn apply_format_to_fs(format: &Format, fs: &dyn FileSystem) -> Result<(), ApplyErrors> {
    apply_plan(&format.validate()?, fs, Path::new("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Fingerprint;
    use std::path::PathBuf;

    fn lr(start: usize, len: usize) -> LineRange {
        LineRange::from_usize(start, len).expect("test ranges are non-zero")
    }

    /// Apply a single-file format to in-memory content, producing the modified
    /// content. Goes through `Format::validate` like production does, so there is
    /// no second copy of the validation rules here; the streamer only ever sees a
    /// `FileEdits`.
    ///
    /// # Panics
    /// Panics if `format` spans more than one file — a single string of content
    /// can only stand in for one file.
    fn apply_format(format: &Format, content: &str) -> Result<String, ApplyErrors> {
        let plan = format.validate()?;
        let edits = match plan.files() {
            [] => return Ok(content.to_string()),
            [edits] => edits,
            files => panic!("apply_format expects one file, got {}", files.len()),
        };
        let mut out = Vec::with_capacity(content.len());
        apply_format_streaming(edits, content.as_bytes(), &mut out)?;
        Ok(String::from_utf8(out).expect("reconstruction of UTF-8 input stays UTF-8"))
    }

    #[test]
    fn test_apply_empty_chunks() {
        let content = "line1\nline2\nline3";
        let format = Format::new(vec![]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_apply_single_chunk_replace() {
        let content = "line1\nline2\nline3\nline4";
        let format = Format::new(vec![Chunk::from_parts(
            "test.txt",
            2,
            2,
            "modified2\nmodified3\n",
        )]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, "line1\nmodified2\nmodified3\nline4");
    }

    #[test]
    fn test_apply_multiple_chunks() {
        let content = "line1\nline2\nline3\nline4\nline5";
        let format = Format::new(vec![
            Chunk::from_parts("test.txt", 1, 1, "mod1\n"),
            Chunk::from_parts("test.txt", 4, 2, "mod4\nmod5\n"),
        ]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, "mod1\nline2\nline3\nmod4\nmod5\n");
    }

    #[test]
    fn test_apply_chunk_at_start() {
        let content = "line1\nline2\nline3";
        let format = Format::new(vec![Chunk::from_parts("test.txt", 1, 1, "modified1\n")]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, "modified1\nline2\nline3");
    }

    #[test]
    fn test_apply_chunk_at_end() {
        let content = "line1\nline2\nline3\n";
        let format = Format::new(vec![Chunk::from_parts("test.txt", 3, 1, "modified3\n")]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, "line1\nline2\nmodified3\n");
    }

    #[test]
    fn test_apply_overlapping_chunks_error() {
        let content = "line1\nline2\nline3\nline4";
        let format = Format::new(vec![
            Chunk::from_parts("test.txt", 1, 3, "mod1"),
            Chunk::from_parts("test.txt", 2, 2, "mod2"),
        ]);
        let result = apply_format(&format, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.0.as_slice()),
            Err([ApplyError::OverlappingChunks { .. }])
        ));
    }

    #[test]
    fn test_apply_rejects_chunks_sharing_a_line() {
        // (1,2) covers lines 1-2 and (2,1) covers line 2: they share line 2.
        let content = "a\nb\nc\nd\n";
        let format = Format::new(vec![
            Chunk::from_parts("f", 1, 2, "X\nY\n"),
            Chunk::from_parts("f", 2, 1, "Z\n"),
        ]);
        let result = apply_format(&format, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.0.as_slice()),
            Err([ApplyError::OverlappingChunks { first, second, .. }])
                if *first == lr(1, 2) && *second == lr(2, 1)
        ));
    }

    #[test]
    fn test_apply_accepts_adjacent_chunks() {
        // (1,2) covers lines 1-2 and (3,1) covers line 3: adjacent, not overlapping.
        let content = "a\nb\nc\nd\n";
        let format = Format::new(vec![
            Chunk::from_parts("f", 1, 2, "X\nY\n"),
            Chunk::from_parts("f", 3, 1, "Z\n"),
        ]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, "X\nY\nZ\nd\n");
    }

    #[test]
    fn test_overlap_error_reports_inclusive_ends() {
        // (5,3) covers lines 5-7 and (7,2) covers lines 7-8; the error must
        // report both ranges with inclusive ends, not off by one.
        let content = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let format = Format::new(vec![
            Chunk::from_parts("f", 5, 3, "A\n"),
            Chunk::from_parts("f", 7, 2, "B\n"),
        ]);
        let errors = apply_format(&format, content).unwrap_err();
        assert!(matches!(
            errors.0.as_slice(),
            [ApplyError::OverlappingChunks { first, second, .. }]
                if *first == lr(5, 3) && *second == lr(7, 2)
        ));
        assert_eq!(
            errors.0[0].to_string(),
            "f: overlapping chunks at lines 5-7 and 7-8"
        );
    }

    #[test]
    fn test_apply_chunk_out_of_bounds() {
        let content = "line1\nline2\nline3";
        let format = Format::new(vec![Chunk::from_parts("test.txt", 3, 2, "mod")]);
        let result = apply_format(&format, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.0.as_slice()),
            Err([ApplyError::ChunkOutOfBounds { range, file_lines: 3, .. }])
                if *range == lr(3, 2)
        ));
    }

    #[test]
    fn test_apply_replace_entire_file() {
        let content = "line1\nline2\nline3";
        let format = Format::new(vec![Chunk::from_parts(
            "test.txt",
            1,
            3,
            "new1\nnew2\nnew3",
        )]);
        let result = apply_format(&format, content).unwrap();
        assert_eq!(result, "new1\nnew2\nnew3");
    }

    #[test]
    fn test_apply_errors_display_lists_each_error() {
        let errors = ApplyErrors(vec![
            ApplyError::OverlappingChunks {
                path: PathBuf::from("a.rs"),
                first: lr(1, 2),
                second: lr(2, 1),
            },
            ApplyError::ChunkOutOfBounds {
                path: PathBuf::from("b.rs"),
                range: lr(3, 1),
                file_lines: 2,
            },
            ApplyError::ContentChanged {
                path: PathBuf::from("c.rs"),
                range: lr(4, 2),
                expected: Fingerprint::of(b"x"),
                actual: Fingerprint::of(b"y"),
            },
        ]);

        let rendered = errors.to_string();
        assert_eq!(
            rendered,
            format!(
                "Failed to apply changes:\n  - a.rs: overlapping chunks at lines 1-2 and 2\n  - b.rs: chunk at lines 3 exceeds file length of 2 lines\n  - c.rs: lines 4-5 changed since this chunk was generated (header fingerprint {}, file has {}); the file was edited, or this chunk was already applied",
                Fingerprint::of(b"x"),
                Fingerprint::of(b"y")
            )
        );
        assert_eq!(rendered.matches("\n  - ").count(), 3);

        // A single error wraps into a one-element list and iterates back out.
        let single = ApplyErrors::from(ApplyError::ChunkOutOfBounds {
            path: PathBuf::from("b.rs"),
            range: lr(3, 1),
            file_lines: 2,
        });
        assert_eq!(single.0.len(), 1);
        assert!(matches!(
            single.into_iter().collect::<Vec<_>>().as_slice(),
            [ApplyError::ChunkOutOfBounds { file_lines: 2, .. }]
        ));
    }

    // ---- validate / Plan tests -------------------------------------------------

    #[test]
    fn test_validate_groups_by_path_and_accumulates_overlaps() {
        // a:(1,2) and a:(2,1) share line 2; b:(1,1) and b:(1,1) are identical.
        // Both files must be reported — one overlap each — not just the first.
        let format = Format::new(vec![
            Chunk::from_parts("a", 1, 2, "A1\nA2\n"),
            Chunk::from_parts("a", 2, 1, "A2'\n"),
            Chunk::from_parts("b", 1, 1, "B1\n"),
            Chunk::from_parts("b", 1, 1, "B1'\n"),
        ]);

        let errors = format.validate().unwrap_err();
        assert!(matches!(
            errors.0.as_slice(),
            [
                ApplyError::OverlappingChunks { first: a1, second: a2, .. },
                ApplyError::OverlappingChunks { first: b1, second: b2, .. },
            ] if *a1 == lr(1, 2) && *a2 == lr(2, 1) && *b1 == lr(1, 1) && *b2 == lr(1, 1)
        ));
    }

    #[test]
    fn test_validate_ok_yields_plan_with_file_groups() {
        let format = Format::new(vec![
            Chunk::from_parts("a", 1, 1, "A1\n"),
            Chunk::from_parts("a", 5, 1, "A5\n"),
            Chunk::from_parts("b", 1, 1, "B1\n"),
        ]);

        let plan = format.validate().unwrap();
        assert_eq!(plan.files().len(), 2);
        assert_eq!(plan.chunk_count(), 3);

        let a = &plan.files()[0];
        assert_eq!(a.path(), Path::new("a"));
        assert_eq!(a.chunks().len(), 2);
        assert_eq!(a.chunks()[0].start_line(), 1);
        assert_eq!(a.chunks()[1].start_line(), 5);

        let b = &plan.files()[1];
        assert_eq!(b.path(), Path::new("b"));
        assert_eq!(b.chunks().len(), 1);
        assert_eq!(b.chunks()[0].start_line(), 1);

        // An empty format validates to an empty plan.
        let empty = Format::new(vec![]);
        let plan = empty.validate().unwrap();
        assert!(plan.files().is_empty());
        assert_eq!(plan.chunk_count(), 0);
    }

    #[test]
    fn test_streaming_reports_only_bounds_errors() {
        // A 2-line file; the chunk wants line 3. The streamer's only possible
        // complaint is the bounds error — everything structural was settled by
        // `validate`.
        let format = Format::new(vec![Chunk::from_parts("f", 3, 1, "X\n")]);
        let plan = format.validate().unwrap();
        let edits = &plan.files()[0];

        let mut out = Vec::new();
        let errors = apply_format_streaming(edits, "a\nb\n".as_bytes(), &mut out).unwrap_err();
        assert!(matches!(
            errors.0.as_slice(),
            [ApplyError::ChunkOutOfBounds { range, file_lines: 2, .. }] if *range == lr(3, 1)
        ));
    }

    // ---- streaming-core tests --------------------------------------------------

    fn stream(chunks: Vec<Chunk>, input: &str) -> Result<String, ApplyErrors> {
        apply_format(&Format::new(chunks), input)
    }

    #[test]
    fn test_stream_preserves_missing_final_newline() {
        // Replace line 2 of a file whose final line has no trailing newline.
        let chunks = vec![Chunk::from_parts("f", 2, 1, "B")];
        assert_eq!(stream(chunks, "a\nb").unwrap(), "a\nB");
    }

    #[test]
    fn test_stream_replace_last_line_keeps_preceding() {
        let chunks = vec![Chunk::from_parts("f", 3, 1, "C\n")];
        assert_eq!(stream(chunks, "a\nb\nc\n").unwrap(), "a\nb\nC\n");
    }

    #[test]
    fn test_stream_multi_chunk_interleave() {
        let chunks = vec![
            Chunk::from_parts("f", 1, 1, "A\n"),
            Chunk::from_parts("f", 4, 2, "D\nE\n"),
        ];
        assert_eq!(
            stream(chunks, "a\nb\nc\nd\ne\n").unwrap(),
            "A\nb\nc\nD\nE\n"
        );
    }

    #[test]
    fn test_stream_handles_very_long_single_line() {
        // A file with no newline at all, much larger than the read buffer. The
        // state machine must never buffer the whole line — it just has to produce
        // the right bytes.
        let big = "x".repeat(STREAM_BUF_SIZE * 3 + 7);
        let chunks = vec![Chunk::from_parts("f", 1, 1, "Y")];
        assert_eq!(stream(chunks, &big).unwrap(), "Y");
    }

    #[test]
    fn test_stream_eof_mid_chunk_is_out_of_bounds() {
        // File has 3 lines; chunk wants to replace lines 3..=4.
        let chunks = vec![Chunk::from_parts("f", 3, 2, "X\n")];
        let err = stream(chunks, "a\nb\nc").unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [ApplyError::ChunkOutOfBounds { .. }]
        ));
    }

    // ---- fingerprint tests ----------------------------------------------------

    fn fp(bytes: &[u8]) -> Option<Fingerprint> {
        Some(Fingerprint::of(bytes))
    }

    #[test]
    fn test_stream_fingerprint_match_applies() {
        let chunks = vec![Chunk::from_parts("f", 2, 1, "B\n").with_fingerprint(fp(b"b\n"))];
        assert_eq!(stream(chunks, "a\nb\nc\n").unwrap(), "a\nB\nc\n");

        // Multi-line range, chunk at the start, chunk at the end with newline.
        let chunks = vec![
            Chunk::from_parts("f", 1, 2, "X\n").with_fingerprint(fp(b"a\nb\n")),
            Chunk::from_parts("f", 4, 1, "Y\n").with_fingerprint(fp(b"d\n")),
        ];
        assert_eq!(stream(chunks, "a\nb\nc\nd\n").unwrap(), "X\nc\nY\n");
    }

    #[test]
    fn test_stream_fingerprint_mismatch_is_content_changed() {
        let chunks = vec![Chunk::from_parts("f", 2, 1, "B\n").with_fingerprint(fp(b"b\n"))];
        let err = stream(chunks, "a\nX\nc\n").unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [ApplyError::ContentChanged { path, range, expected, actual }]
                if path == Path::new("f")
                    && *range == lr(2, 1)
                    && Some(*expected) == fp(b"b\n")
                    && Some(*actual) == fp(b"X\n")
        ));

        // Every mismatching chunk is reported, not just the first.
        let chunks = vec![
            Chunk::from_parts("f", 1, 1, "A\n").with_fingerprint(fp(b"nope\n")),
            Chunk::from_parts("f", 3, 1, "C\n").with_fingerprint(fp(b"nope\n")),
        ];
        let err = stream(chunks, "a\nb\nc\n").unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [
                ApplyError::ContentChanged { range: r1, .. },
                ApplyError::ContentChanged { range: r3, .. },
            ] if *r1 == lr(1, 1) && *r3 == lr(3, 1)
        ));
    }

    #[test]
    fn test_stream_fingerprint_without_tag_is_unchecked() {
        let chunks = vec![Chunk::from_parts("f", 2, 1, "B\n")];
        assert_eq!(stream(chunks, "a\nanything\nc\n").unwrap(), "a\nB\nc\n");
    }

    #[test]
    fn test_stream_fingerprint_last_line_without_newline() {
        // The original last line has no trailing newline: the fingerprint covers
        // exactly the bytes present.
        let chunks = vec![Chunk::from_parts("f", 2, 1, "B").with_fingerprint(fp(b"b"))];
        assert_eq!(stream(chunks, "a\nb").unwrap(), "a\nB");

        let chunks = vec![Chunk::from_parts("f", 2, 1, "B").with_fingerprint(fp(b"b\n"))];
        assert!(matches!(
            stream(chunks, "a\nb").unwrap_err().0.as_slice(),
            [ApplyError::ContentChanged { .. }]
        ));
    }

    #[test]
    fn test_stream_fingerprint_across_read_buffer_boundaries() {
        // The replaced lines straddle several read buffers; the incremental hash
        // must equal the one-shot hash of the same bytes.
        let long = "x".repeat(STREAM_BUF_SIZE + 17);
        let content = format!("head\n{long}\n{long}\ntail\n");
        let original = format!("{long}\n{long}\n");
        let chunks =
            vec![Chunk::from_parts("f", 2, 2, "Y\n").with_fingerprint(fp(original.as_bytes()))];
        assert_eq!(stream(chunks, &content).unwrap(), "head\nY\ntail\n");
    }

    #[test]
    fn test_stream_out_of_bounds_chunk_reports_bounds_not_fingerprint() {
        let chunks = vec![Chunk::from_parts("f", 2, 3, "B\n").with_fingerprint(fp(b"zzz"))];
        let err = stream(chunks, "a\nb\n").unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [ApplyError::ChunkOutOfBounds { range, file_lines: 2, .. }] if *range == lr(2, 3)
        ));
    }

    #[test]
    fn test_apply_to_fs_refuses_double_apply() {
        use std::str::FromStr;
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        fs.add_file(&a, "a\nb\nc\n").unwrap();

        // What `ingest` would have produced for line 2, then edited to two lines.
        let bk = format!("@/a.txt:2:1 #{}\nb1\nb2\n@@@\n", Fingerprint::of(b"b\n"));
        let format = Format::from_str(&bk).unwrap();

        apply_format_to_fs(&format, &fs).unwrap();
        assert_eq!(fs.read_to_string(&a).unwrap(), "a\nb1\nb2\nc\n");

        // Applying the same file again must not insert at the shifted position.
        let err = apply_format_to_fs(&format, &fs).unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [ApplyError::ContentChanged { range, .. }] if *range == lr(2, 1)
        ));
        assert_eq!(fs.read_to_string(&a).unwrap(), "a\nb1\nb2\nc\n");
        assert_eq!(fs.file_count(), 1);
    }

    #[test]
    fn test_apply_to_fs_refuses_stale_file_and_stays_atomic() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        let b = PathBuf::from("/b.txt");
        fs.add_file(&a, "a1\na2\n").unwrap();
        fs.add_file(&b, "b1\nb2\n").unwrap();

        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 1, 1, "A1\n").with_fingerprint(fp(b"a1\n")),
            Chunk::from_parts(b.clone(), 2, 1, "B2\n").with_fingerprint(fp(b"b2\n")),
        ]);

        // Someone inserts a line at the top of b between ingest and apply.
        fs.add_file(&b, "new\nb1\nb2\n").unwrap();

        let err = apply_format_to_fs(&format, &fs).unwrap_err();
        assert!(matches!(
            err.0.as_slice(),
            [ApplyError::ContentChanged { path, range, .. }] if path == &b && *range == lr(2, 1)
        ));
        // a's chunk was fine, but nothing at all was written.
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\n");
        assert_eq!(fs.read_to_string(&b).unwrap(), "new\nb1\nb2\n");
        assert_eq!(fs.file_count(), 2);
    }

    // ---- preview tests --------------------------------------------------------

    fn preview(format: &Format, fs: &MemoryFS) -> (String, Vec<PreviewCounts>) {
        let plan = format.validate().unwrap();
        let mut out = Vec::new();
        let summary = write_preview(&plan, fs, &mut out, false).unwrap();
        (
            String::from_utf8(out).unwrap(),
            summary.into_iter().map(|(_, c)| c).collect(),
        )
    }

    #[test]
    fn test_preview_prints_hunks_for_changed_chunks_only() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        fs.add_file(&a, "a\nb\nc\nd\n").unwrap();

        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 2, 1, "B\n"),
            Chunk::from_parts(a.clone(), 3, 1, "c\n"), // identical: not printed
            Chunk::from_parts(a.clone(), 4, 1, "D1\nD2\n"),
        ]);
        let (out, counts) = preview(&format, &fs);
        assert_eq!(
            out,
            "--- /a.txt\n@@ -2,1 +2,1 @@\n-b\n+B\n@@ -4,1 +4,2 @@\n-d\n+D1\n+D2\n"
        );
        assert_eq!(
            counts,
            vec![PreviewCounts {
                changed: 2,
                unchanged: 1
            }]
        );
        // Preview is read-only.
        assert_eq!(fs.read_to_string(&a).unwrap(), "a\nb\nc\nd\n");
    }

    #[test]
    fn test_preview_shows_unchanged_lines_inside_a_chunk_as_context() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        fs.add_file(&a, "a\nb\nc\nd\n").unwrap();

        // Only the middle line of the chunk changes.
        let format = Format::new(vec![Chunk::from_parts(a.clone(), 1, 3, "a\nB\nc\n")]);
        let (out, _) = preview(&format, &fs);
        assert_eq!(out, "--- /a.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n");
    }

    #[test]
    fn test_preview_shifts_new_side_line_numbers_by_earlier_deltas() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        fs.add_file(&a, "a\nb\nc\n").unwrap();

        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 1, 1, "A1\nA2\nA3\n"), // +2 lines
            Chunk::from_parts(a.clone(), 3, 1, ""),             // deletion
        ]);
        let (out, _) = preview(&format, &fs);
        assert_eq!(
            out,
            "--- /a.txt\n@@ -1,1 +1,3 @@\n-a\n+A1\n+A2\n+A3\n@@ -3,1 +5,0 @@\n-c\n"
        );
    }

    #[test]
    fn test_preview_marks_missing_trailing_newline_and_all_unchanged() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        let b = PathBuf::from("/b.txt");
        fs.add_file(&a, "a\nb").unwrap();
        fs.add_file(&b, "x\n").unwrap();

        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 2, 1, "B"),
            Chunk::from_parts(b.clone(), 1, 1, "x\n"),
        ]);
        let (out, counts) = preview(&format, &fs);
        assert_eq!(
            out,
            "--- /a.txt\n@@ -2,1 +2,1 @@\n-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n"
        );
        assert_eq!(
            counts,
            vec![
                PreviewCounts {
                    changed: 1,
                    unchanged: 0
                },
                PreviewCounts {
                    changed: 0,
                    unchanged: 1
                },
            ]
        );
    }

    // ---- filesystem orchestration tests ---------------------------------------

    use crate::filesystem::memory::MemoryFS;

    #[test]
    fn test_apply_to_fs_multi_file_happy_path() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        let b = PathBuf::from("/b.txt");
        fs.add_file(&a, "a1\na2\na3\n").unwrap();
        fs.add_file(&b, "b1\nb2\n").unwrap();

        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 1, 1, "A1\n"),
            Chunk::from_parts(b.clone(), 2, 1, "B2\n"),
        ]);

        apply_format_to_fs(&format, &fs).unwrap();

        assert_eq!(fs.read_to_string(&a).unwrap(), "A1\na2\na3\n");
        assert_eq!(fs.read_to_string(&b).unwrap(), "b1\nB2\n");
        // Both targets only; no staged temp files left behind.
        assert_eq!(fs.file_count(), 2);
    }

    #[test]
    fn test_apply_to_fs_is_atomic_across_files() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        let b = PathBuf::from("/b.txt");
        fs.add_file(&a, "a1\na2\na3\n").unwrap();
        fs.add_file(&b, "b1\nb2\n").unwrap();

        // a's chunk is valid; b's chunk is out of bounds (line 5 of a 2-line file).
        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 1, 1, "A1\n"),
            Chunk::from_parts(b.clone(), 5, 1, "B\n"),
        ]);

        let result = apply_format_to_fs(&format, &fs);
        assert!(result.is_err());

        // Neither file was modified, and nothing was staged.
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\na3\n");
        assert_eq!(fs.read_to_string(&b).unwrap(), "b1\nb2\n");
        assert_eq!(fs.file_count(), 2);
    }

    #[test]
    fn test_verify_plan_accumulates_bounds_errors_across_files() {
        // Two files, each with one out-of-bounds chunk: both must be reported.
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        let b = PathBuf::from("/b.txt");
        fs.add_file(&a, "a1\n").unwrap();
        fs.add_file(&b, "b1\nb2\n").unwrap();

        let format = Format::new(vec![
            Chunk::from_parts(a.clone(), 4, 1, "A\n"),
            Chunk::from_parts(b.clone(), 9, 1, "B\n"),
        ]);
        let plan = format.validate().unwrap();

        let errors = verify_plan(&plan, &fs).unwrap_err();
        assert!(matches!(
            errors.0.as_slice(),
            [
                ApplyError::ChunkOutOfBounds { range: ra, file_lines: 1, .. },
                ApplyError::ChunkOutOfBounds { range: rb, file_lines: 2, .. },
            ] if *ra == lr(4, 1) && *rb == lr(9, 1)
        ));
    }

    #[test]
    fn test_verify_writes_nothing() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        fs.add_file(&a, "a1\na2\n").unwrap();

        let ok = Format::new(vec![Chunk::from_parts(a.clone(), 1, 1, "A1\n")]);
        verify_format_to_fs(&ok, &fs).unwrap();
        // Verification is read-only.
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\n");
        assert_eq!(fs.file_count(), 1);

        let bad = Format::new(vec![Chunk::from_parts(a.clone(), 9, 1, "X\n")]);
        assert!(verify_format_to_fs(&bad, &fs).is_err());
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\n");
    }
}
