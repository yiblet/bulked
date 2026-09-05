use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Apply a format to the filesystem.
///
/// "Parse, don't validate": every content-independent check lives in exactly one
/// place, [`Format::validate`], which turns a `Format` into a [`Plan`] — a proof
/// that the chunks are grouped by file, sorted, and non-overlapping. Everything
/// downstream takes a [`FileEdits`] (one file's validated chunks) and can only
/// fail on what it cannot know up front: the file's real length
/// ([`ApplyError::ChunkOutOfBounds`]) and I/O.
///
/// The reconstruction core is [`apply_format_streaming`], which reads the original
/// file, interleaves the chunk replacements, and writes the result — all with
/// bounded memory (a fixed read buffer; never a whole line or whole file).
///
/// Algorithm:
/// 1. `Format::validate`: group by path (a `Format` is sorted by construction) and
///    reject any consecutive pair in a file whose [`LineRange`]s overlap. Errors are
///    accumulated across all files.
/// 2. `verify_plan`: stream every file to a sink, accumulating bounds/IO errors
///    across all files. Nothing is written.
/// 3. `apply_plan`: `verify_plan`, then stage each reconstruction via a
///    [`StagingFs`] and commit all of them at once.
use crate::{
    filesystem::{FileSystem, ReadFs, staging::StagingFs},
    format::{Chunk, Format, LineRange},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("Overlapping chunks at lines {first} and {second}")]
    OverlappingChunks { first: LineRange, second: LineRange },

    #[error("Chunk at lines {range} exceeds file length of {file_lines} lines")]
    ChunkOutOfBounds { range: LineRange, file_lines: usize },

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
                    errors.push(ApplyError::OverlappingChunks { first, second });
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
fn out_of_bounds(range: LineRange, file_lines: usize) -> Option<ApplyError> {
    (range.end_inclusive() > file_lines)
        .then_some(ApplyError::ChunkOutOfBounds { range, file_lines })
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
/// # Errors
/// Returns one [`ApplyError::ChunkOutOfBounds`] per chunk that references lines
/// past EOF (all of them, accumulated), or [`ApplyError::Io`] on a read/write
/// failure.
pub fn apply_format_streaming(
    edits: &FileEdits<'_>,
    mut reader: impl Read,
    writer: &mut dyn Write,
) -> Result<(), ApplyErrors> {
    let chunks = edits.chunks();
    let range_at = |i: usize| chunks[i].range();

    let mut buf = [0u8; STREAM_BUF_SIZE];
    let mut cur_line: usize = 1; // line number of the byte at the read cursor
    let mut idx = 0usize; // index of the next chunk to emit
    let mut skip_until: usize = 1; // we are skipping original lines while cur_line < skip_until
    let mut at_line_start = true;
    let mut any_bytes = false;

    let to_io = |e: std::io::Error| ApplyErrors::from(ApplyError::Io(e));

    // A chunk may start on line 1, before we have read anything.
    if idx < chunks.len() && range_at(idx).start() == cur_line {
        writer
            .write_all(chunks[idx].content().as_bytes())
            .map_err(to_io)?;
        skip_until = range_at(idx).end_exclusive();
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
                    if !skipping {
                        writer.write_all(&block[..=pos]).map_err(to_io)?;
                    }
                    block = &block[pos + 1..];
                    cur_line += 1;
                    at_line_start = true;
                    // Emit a chunk that begins at this new line (once we are past any
                    // active skip region).
                    if idx < chunks.len()
                        && range_at(idx).start() == cur_line
                        && cur_line >= skip_until
                    {
                        writer
                            .write_all(chunks[idx].content().as_bytes())
                            .map_err(to_io)?;
                        skip_until = range_at(idx).end_exclusive();
                        idx += 1;
                    }
                }
                None => {
                    // No newline in the remaining block: it is all part of `cur_line`.
                    if !skipping {
                        writer.write_all(block).map_err(to_io)?;
                    }
                    at_line_start = false;
                    block = &[];
                }
            }
        }
    }

    // At EOF, count the file's lines the same way `split_inclusive('\n')` does, then
    // flag any chunk whose range extends past the end of the file.
    let file_lines = if !any_bytes {
        0
    } else if at_line_start {
        cur_line - 1
    } else {
        cur_line
    };
    let errors: Vec<ApplyError> = chunks
        .iter()
        .filter_map(|c| out_of_bounds(c.range(), file_lines))
        .collect();

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
/// all bounds/IO errors across all files. Nothing is read into memory whole and
/// nothing is written.
///
/// # Errors
/// Returns every bounds/IO error found across all files.
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

/// Apply a plan to the filesystem atomically.
///
/// Two phases:
/// 1. **Verify** every file ([`verify_plan`]). If anything fails, nothing is
///    written, anywhere.
/// 2. **Commit**: stage each file's reconstruction into a temp file via a
///    [`StagingFs`], then move every temp into place. Staging streams with bounded
///    memory; an error during staging drops the `StagingFs`, deleting all temp files
///    and leaving every target untouched.
///
/// # Errors
/// Returns the accumulated errors from verification, or any I/O errors encountered
/// while staging or committing.
pub fn apply_plan(plan: &Plan<'_>, fs: &dyn FileSystem) -> Result<(), ApplyErrors> {
    // Phase 1: verify everything up front.
    verify_plan(plan, fs)?;

    // Phase 2: stage every file into a tracked temp file, then commit.
    let staging = StagingFs::new(fs);
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
    apply_plan(&format.validate()?, fs)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            Err([ApplyError::OverlappingChunks { first, second }])
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
            [ApplyError::OverlappingChunks { first, second }]
                if *first == lr(5, 3) && *second == lr(7, 2)
        ));
        assert_eq!(
            errors.0[0].to_string(),
            "Overlapping chunks at lines 5-7 and 7-8"
        );
    }

    #[test]
    fn test_apply_chunk_out_of_bounds() {
        let content = "line1\nline2\nline3";
        let format = Format::new(vec![Chunk::from_parts("test.txt", 3, 2, "mod")]);
        let result = apply_format(&format, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.0.as_slice()),
            Err([ApplyError::ChunkOutOfBounds { range, file_lines: 3 }])
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
                first: lr(1, 2),
                second: lr(2, 1),
            },
            ApplyError::ChunkOutOfBounds {
                range: lr(3, 1),
                file_lines: 2,
            },
        ]);

        let rendered = errors.to_string();
        assert_eq!(
            rendered,
            "Failed to apply changes:\n  - Overlapping chunks at lines 1-2 and 2\n  - Chunk at lines 3 exceeds file length of 2 lines"
        );
        assert_eq!(rendered.matches("\n  - ").count(), 2);

        // A single error wraps into a one-element list and iterates back out.
        let single = ApplyErrors::from(ApplyError::ChunkOutOfBounds {
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
                ApplyError::OverlappingChunks { first: a1, second: a2 },
                ApplyError::OverlappingChunks { first: b1, second: b2 },
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
            [ApplyError::ChunkOutOfBounds { range, file_lines: 2 }] if *range == lr(3, 1)
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
                ApplyError::ChunkOutOfBounds { range: ra, file_lines: 1 },
                ApplyError::ChunkOutOfBounds { range: rb, file_lines: 2 },
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
