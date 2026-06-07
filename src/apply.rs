use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Apply a format to a file
///
/// The reconstruction core is `apply_format_streaming`, which reads the original
/// file, interleaves the chunk replacements, and writes the result — all with
/// bounded memory (a fixed read buffer; never a whole line or whole file). The
/// `apply_format` helper is a convenience wrapper over it for string-in/string-out
/// callers (mostly tests).
///
/// Algorithm:
/// 1. Validate the chunks (same path, sorted, non-overlapping, non-zero length).
/// 2. Stream the original file, copying unchanged lines through and substituting
///    chunk content for the lines each chunk replaces.
/// 3. Detect out-of-bounds chunks when the stream reaches EOF.
use crate::{
    filesystem::{FileSystem, staging::StagingFs},
    format::{Chunk, Format},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("Chunks must all have the same path")]
    MixedPaths,

    #[error("Overlapping chunks at lines {0}-{1} and {2}-{3}")]
    OverlappingChunks(usize, usize, usize, usize),

    #[error(
        "Chunk at line {line} with {num_lines} lines exceeds file length of {file_lines} lines"
    )]
    ChunkOutOfBounds {
        line: usize,
        num_lines: usize,
        file_lines: usize,
    },

    #[error("Invalid line number: line numbers must be >= 1")]
    InvalidLineNumber,

    #[error("Invalid number of lines: number of lines must be > 0")]
    InvalidNumberOfLines,

    #[error("Chunks are not sorted by line number")]
    UnsortedChunks,

    #[error("Failed to modify file {path}: {source}")]
    ModifyError {
        path: PathBuf,
        #[source]
        source: crate::filesystem::FilesystemError,
    },

    #[error("I/O error while applying changes: {0}")]
    Io(#[source] std::io::Error),
}

fn modify_err(path: &Path, source: crate::filesystem::FilesystemError) -> ApplyError {
    ApplyError::ModifyError {
        path: path.to_path_buf(),
        source,
    }
}

fn chunks_are_all_for_same_path(chunks: &[Chunk]) -> Result<(), ApplyError> {
    let Some(first) = chunks.first() else {
        return Ok(());
    };

    if !chunks.iter().all(|c| c.path == first.path) {
        return Err(ApplyError::MixedPaths);
    }
    Ok(())
}

fn chunks_are_have_non_zero_length(chunks: &[Chunk]) -> Result<(), ApplyError> {
    if !chunks.iter().all(|c| c.num_lines > 0) {
        return Err(ApplyError::InvalidNumberOfLines);
    }
    Ok(())
}

// TODO: use this
#[allow(dead_code)]
fn chunks_have_valid_line_numbers(chunks: &[Chunk]) -> Result<(), ApplyError> {
    if !chunks.iter().all(|c| c.start_line >= 1) {
        return Err(ApplyError::InvalidLineNumber);
    }
    Ok(())
}

fn chunks_are_sorted_by_line_number(chunks: &[Chunk]) -> Result<(), ApplyError> {
    if !chunks.windows(2).all(|w| match w {
        [c1, c2] => c1.start_line < c2.start_line,
        _ => false,
    }) {
        return Err(ApplyError::UnsortedChunks);
    }
    Ok(())
}

fn chunks_are_not_overlapping(chunks: &[Chunk]) -> Result<(), ApplyError> {
    for window in chunks.windows(2) {
        if let [c1, c2] = window {
            let c1_end = c1.start_line + c1.num_lines - 1;

            if c1_end > c2.start_line {
                return Err(ApplyError::OverlappingChunks(
                    c1.start_line,
                    c1_end - 1,
                    c2.start_line,
                    c2.start_line + c2.num_lines - 1,
                ));
            }
        }
    }
    Ok(())
}

// Used by the `apply_format` string-in/string-out wrapper (mostly tests); the
// production streaming path detects out-of-bounds chunks at EOF instead.
#[allow(dead_code)]
fn chunks_are_within_file_bounds(chunks: &[Chunk], content: &str) -> Result<(), ApplyError> {
    let file_lines = content.split_inclusive('\n').count();
    for chunk in chunks {
        let end_line = chunk.start_line + chunk.num_lines - 1;
        if end_line > file_lines {
            return Err(ApplyError::ChunkOutOfBounds {
                line: chunk.start_line,
                num_lines: chunk.num_lines,
                file_lines,
            });
        }
    }
    Ok(())
}

/// Size of the fixed read buffer used by [`apply_format_streaming`]. Reconstruction
/// memory is bounded by this regardless of file size or line length.
const STREAM_BUF_SIZE: usize = 64 * 1024;

/// Stream a file's reconstruction: read the original from `reader`, interleave the
/// chunk replacements, and write the result to `writer`.
///
/// This is the reconstruction core. It uses a single fixed-size read buffer and a
/// byte-level state machine (copy original lines through, or skip the lines a chunk
/// replaces), so its memory use is approximately constant per call — independent of
/// the file size *and* of how long any individual line is. Chunk content (already
/// resident in the `Chunk`) is written verbatim, preserving exact bytes including
/// trailing-newline / no-trailing-newline semantics.
///
/// # Errors
/// Returns the accumulated validation errors if the chunks are structurally invalid
/// (mixed paths, unsorted, overlapping, zero-length), [`ApplyError::ChunkOutOfBounds`]
/// for any chunk that references lines past EOF, or [`ApplyError::Io`] on a read/write
/// failure.
pub fn apply_format_streaming(
    chunks: &[Chunk],
    mut reader: impl Read,
    writer: &mut dyn Write,
) -> Result<(), Vec<ApplyError>> {
    // Content-independent validation first; bail before touching the streams.
    let mut errors = Vec::new();
    {
        let mut handle_error = |result: Result<(), ApplyError>| {
            if let Err(err) = result {
                errors.push(err);
            }
        };
        handle_error(chunks_are_all_for_same_path(chunks));
        handle_error(
            chunks_are_sorted_by_line_number(chunks)
                .and_then(|()| chunks_are_not_overlapping(chunks)),
        );
        handle_error(chunks_are_have_non_zero_length(chunks));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut buf = [0u8; STREAM_BUF_SIZE];
    let mut cur_line: usize = 1; // line number of the byte at the read cursor
    let mut idx = 0usize; // index of the next chunk to emit
    let mut skip_until: usize = 1; // we are skipping original lines while cur_line < skip_until
    let mut at_line_start = true;
    let mut any_bytes = false;

    let to_io = |e: std::io::Error| vec![ApplyError::Io(e)];

    // A chunk may start on line 1, before we have read anything.
    if idx < chunks.len() && chunks[idx].start_line == cur_line {
        let chunk = &chunks[idx];
        writer.write_all(chunk.content.as_bytes()).map_err(to_io)?;
        skip_until = cur_line + chunk.num_lines;
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
                        && chunks[idx].start_line == cur_line
                        && cur_line >= skip_until
                    {
                        let chunk = &chunks[idx];
                        writer.write_all(chunk.content.as_bytes()).map_err(to_io)?;
                        skip_until = cur_line + chunk.num_lines;
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
    for chunk in chunks {
        let end_line = chunk.start_line + chunk.num_lines - 1;
        if end_line > file_lines {
            errors.push(ApplyError::ChunkOutOfBounds {
                line: chunk.start_line,
                num_lines: chunk.num_lines,
                file_lines,
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Apply chunks to in-memory file content, producing the modified content.
///
/// Convenience wrapper over [`apply_format_streaming`] for string-in/string-out
/// callers (primarily tests). It is not on the production apply path, so the
/// whole-file `String` it allocates never appears during a real `apply`.
///
/// # Errors
/// Returns an error if chunks have different paths, are unsorted, overlap, are
/// zero-length, or reference lines outside the file.
#[allow(dead_code)]
pub fn apply_format(chunks: &[Chunk], content: &str) -> Result<String, Vec<ApplyError>> {
    if chunks.is_empty() {
        return Ok(content.to_string());
    }

    // Preserve the historical accumulation behavior (bounds error reported alongside
    // structural errors), which `apply_format_streaming` cannot do because it only
    // learns the file length while streaming.
    let mut errors = Vec::new();
    {
        let mut handle_error = |result: Result<(), ApplyError>| {
            if let Err(err) = result {
                errors.push(err);
            }
        };
        handle_error(chunks_are_all_for_same_path(chunks));
        handle_error(chunks_are_within_file_bounds(chunks, content));
        handle_error(
            chunks_are_sorted_by_line_number(chunks)
                .and_then(|()| chunks_are_not_overlapping(chunks)),
        );
        handle_error(chunks_are_have_non_zero_length(chunks));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut out = Vec::with_capacity(content.len());
    apply_format_streaming(chunks, content.as_bytes(), &mut out)?;
    Ok(String::from_utf8(out).expect("reconstruction of UTF-8 input stays UTF-8"))
}

/// Verify that a parsed format can be applied cleanly, without writing anything.
///
/// This is exactly phase 1 of an atomic apply (and the entire `--dry-run` path): it
/// streams every file through [`apply_format_streaming`] into a sink, accumulating
/// all errors across all files. Nothing is read into memory whole and nothing is
/// written.
///
/// # Errors
/// Returns every validation/bounds/IO error found across all files.
pub fn verify_format_to_fs(
    format: &mut Format,
    fs: &dyn FileSystem,
) -> Result<(), Vec<ApplyError>> {
    let mut errors = Vec::new();
    for (path, chunks) in format.file_chunks() {
        let result = fs
            .read(path)
            .map_err(|e| vec![modify_err(path, e)])
            .and_then(|reader| apply_format_streaming(chunks, reader, &mut std::io::sink()));
        if let Err(errs) = result {
            errors.extend(errs);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Apply a parsed format to the filesystem atomically.
///
/// Two phases:
/// 1. **Verify** every file ([`verify_format_to_fs`]). If anything fails, nothing is
///    written, anywhere.
/// 2. **Commit**: stage each file's reconstruction into a temp file via a
///    [`StagingFs`], then move every temp into place. Staging streams with bounded
///    memory; an error during staging drops the `StagingFs`, deleting all temp files
///    and leaving every target untouched.
///
/// # Errors
/// Returns the accumulated errors from verification, or any I/O errors encountered
/// while staging or committing.
pub fn apply_format_to_fs(format: &mut Format, fs: &dyn FileSystem) -> Result<(), Vec<ApplyError>> {
    // Phase 1: validate everything up front.
    verify_format_to_fs(format, fs)?;

    // Phase 2: stage every file into a tracked temp file, then commit.
    let staging = StagingFs::new(fs);
    let mut errors = Vec::new();
    for (path, chunks) in format.file_chunks() {
        let result = stage_file(&staging, path, chunks);
        if let Err(errs) = result {
            errors.extend(errs);
        }
    }
    if !errors.is_empty() {
        // `staging` drops here: every staged temp file is removed, targets untouched.
        return Err(errors);
    }

    staging.commit().map_err(|failures| {
        failures
            .into_iter()
            .map(|(path, source)| ApplyError::ModifyError { path, source })
            .collect()
    })
}

/// Stream one file's reconstruction into the staging filesystem.
fn stage_file(
    staging: &StagingFs<'_>,
    path: &Path,
    chunks: &[Chunk],
) -> Result<(), Vec<ApplyError>> {
    let reader = staging.read(path).map_err(|e| vec![modify_err(path, e)])?;
    let mut writer = staging
        .writer(path)
        .map_err(|e| vec![modify_err(path, e)])?;
    apply_format_streaming(chunks, reader, &mut *writer)?;
    writer.flush().map_err(|e| vec![ApplyError::Io(e)])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_apply_empty_chunks() {
        let content = "line1\nline2\nline3";
        let chunks = vec![];
        let result = apply_format(&chunks, content).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_apply_single_chunk_replace() {
        let content = "line1\nline2\nline3\nline4";
        let chunks = vec![Chunk::new(
            PathBuf::from("test.txt"),
            2,
            2,
            "modified2\nmodified3\n".to_string(),
        )];
        let result = apply_format(&chunks, content).unwrap();
        assert_eq!(result, "line1\nmodified2\nmodified3\nline4");
    }

    #[test]
    fn test_apply_multiple_chunks() {
        let content = "line1\nline2\nline3\nline4\nline5";
        let chunks = vec![
            Chunk::new(PathBuf::from("test.txt"), 1, 1, "mod1\n".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 4, 2, "mod4\nmod5\n".to_string()),
        ];
        let result = apply_format(&chunks, content).unwrap();
        assert_eq!(result, "mod1\nline2\nline3\nmod4\nmod5\n");
    }

    #[test]
    fn test_apply_chunk_at_start() {
        let content = "line1\nline2\nline3";
        let chunks = vec![Chunk::new(
            PathBuf::from("test.txt"),
            1,
            1,
            "modified1\n".to_string(),
        )];
        let result = apply_format(&chunks, content).unwrap();
        assert_eq!(result, "modified1\nline2\nline3");
    }

    #[test]
    fn test_apply_chunk_at_end() {
        let content = "line1\nline2\nline3\n";
        let chunks = vec![Chunk::new(
            PathBuf::from("test.txt"),
            3,
            1,
            "modified3\n".to_string(),
        )];
        let result = apply_format(&chunks, content).unwrap();
        assert_eq!(result, "line1\nline2\nmodified3\n");
    }

    #[test]
    fn test_apply_mixed_paths_error() {
        let content = "line1\nline2";
        let chunks = vec![
            Chunk::new(PathBuf::from("test1.txt"), 1, 1, "mod1\n".to_string()),
            Chunk::new(PathBuf::from("test2.txt"), 2, 1, "mod2\n".to_string()),
        ];
        let result = apply_format(&chunks, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.as_slice()),
            Err([ApplyError::MixedPaths])
        ));
    }

    #[test]
    fn test_apply_overlapping_chunks_error() {
        let content = "line1\nline2\nline3\nline4";
        let chunks = vec![
            Chunk::new(PathBuf::from("test.txt"), 1, 3, "mod1".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 2, 2, "mod2".to_string()),
        ];
        let result = apply_format(&chunks, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.as_slice()),
            Err([ApplyError::OverlappingChunks(_, _, _, _)])
        ));
    }

    #[test]
    fn test_apply_chunk_out_of_bounds() {
        let content = "line1\nline2\nline3";
        let chunks = vec![Chunk::new(
            PathBuf::from("test.txt"),
            3,
            2,
            "mod".to_string(),
        )];
        let result = apply_format(&chunks, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.as_slice()),
            Err([ApplyError::ChunkOutOfBounds { .. }])
        ));
    }

    #[test]
    fn test_apply_replace_entire_file() {
        let content = "line1\nline2\nline3";
        let chunks = vec![Chunk::new(
            PathBuf::from("test.txt"),
            1,
            3,
            "new1\nnew2\nnew3".to_string(),
        )];
        let result = apply_format(&chunks, content).unwrap();
        assert_eq!(result, "new1\nnew2\nnew3");
    }

    #[test]
    fn test_apply_unsorted_chunks() {
        let content = "line1\nline2\nline3\nline4\nline5";
        let chunks = vec![
            Chunk::new(PathBuf::from("test.txt"), 3, 1, "mod4".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 1, 1, "mod1".to_string()),
        ];
        let result = apply_format(&chunks, content);
        assert!(matches!(
            result.as_ref().map_err(|r| r.as_slice()),
            Err([ApplyError::UnsortedChunks])
        ));
    }

    // ---- streaming-core tests --------------------------------------------------

    fn stream(chunks: &[Chunk], input: &str) -> Result<String, Vec<ApplyError>> {
        let mut out = Vec::new();
        apply_format_streaming(chunks, input.as_bytes(), &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn test_stream_preserves_missing_final_newline() {
        // Replace line 2 of a file whose final line has no trailing newline.
        let chunks = vec![Chunk::new(PathBuf::from("f"), 2, 1, "B".to_string())];
        assert_eq!(stream(&chunks, "a\nb").unwrap(), "a\nB");
    }

    #[test]
    fn test_stream_replace_last_line_keeps_preceding() {
        let chunks = vec![Chunk::new(PathBuf::from("f"), 3, 1, "C\n".to_string())];
        assert_eq!(stream(&chunks, "a\nb\nc\n").unwrap(), "a\nb\nC\n");
    }

    #[test]
    fn test_stream_multi_chunk_interleave() {
        let chunks = vec![
            Chunk::new(PathBuf::from("f"), 1, 1, "A\n".to_string()),
            Chunk::new(PathBuf::from("f"), 4, 2, "D\nE\n".to_string()),
        ];
        assert_eq!(
            stream(&chunks, "a\nb\nc\nd\ne\n").unwrap(),
            "A\nb\nc\nD\nE\n"
        );
    }

    #[test]
    fn test_stream_handles_very_long_single_line() {
        // A file with no newline at all, much larger than the read buffer. The
        // state machine must never buffer the whole line — it just has to produce
        // the right bytes.
        let big = "x".repeat(STREAM_BUF_SIZE * 3 + 7);
        let chunks = vec![Chunk::new(PathBuf::from("f"), 1, 1, "Y".to_string())];
        assert_eq!(stream(&chunks, &big).unwrap(), "Y");
    }

    #[test]
    fn test_stream_eof_mid_chunk_is_out_of_bounds() {
        // File has 3 lines; chunk wants to replace lines 3..=4.
        let chunks = vec![Chunk::new(PathBuf::from("f"), 3, 2, "X\n".to_string())];
        let err = stream(&chunks, "a\nb\nc").unwrap_err();
        assert!(matches!(
            err.as_slice(),
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

        let mut format = Format(vec![
            Chunk::new(a.clone(), 1, 1, "A1\n".to_string()),
            Chunk::new(b.clone(), 2, 1, "B2\n".to_string()),
        ]);

        apply_format_to_fs(&mut format, &fs).unwrap();

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
        let mut format = Format(vec![
            Chunk::new(a.clone(), 1, 1, "A1\n".to_string()),
            Chunk::new(b.clone(), 5, 1, "B\n".to_string()),
        ]);

        let result = apply_format_to_fs(&mut format, &fs);
        assert!(result.is_err());

        // Neither file was modified, and nothing was staged.
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\na3\n");
        assert_eq!(fs.read_to_string(&b).unwrap(), "b1\nb2\n");
        assert_eq!(fs.file_count(), 2);
    }

    #[test]
    fn test_verify_writes_nothing() {
        let fs = MemoryFS::new();
        let a = PathBuf::from("/a.txt");
        fs.add_file(&a, "a1\na2\n").unwrap();

        let mut ok = Format(vec![Chunk::new(a.clone(), 1, 1, "A1\n".to_string())]);
        verify_format_to_fs(&mut ok, &fs).unwrap();
        // Verification is read-only.
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\n");
        assert_eq!(fs.file_count(), 1);

        let mut bad = Format(vec![Chunk::new(a.clone(), 9, 1, "X\n".to_string())]);
        assert!(verify_format_to_fs(&mut bad, &fs).is_err());
        assert_eq!(fs.read_to_string(&a).unwrap(), "a1\na2\n");
    }
}
