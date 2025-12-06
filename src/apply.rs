use std::path::PathBuf;

/// Apply a format to a file
///
/// The core function in this module is `apply_format` which takes chunks and file content
/// and applies the chunk modifications to produce the final file content.
///
/// Algorithm:
/// 1. Check that all chunks are for the same path
/// 2. Sort chunks by line number
/// 3. Convert to segments (alternating between modified chunks and unmodified content)
/// 4. Reconstruct the final string from segments
use crate::{
    filesystem::FileSystem,
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

fn chunks_are_within_file_bounds(chunks: &[Chunk], content: &str) -> Result<(), ApplyError> {
    let file_lines = content.split_inclusive('\n').count();
    for chunk in chunks {
        let end_line = chunk.start_line + chunk.num_lines - 1;
        if end_line > file_lines + 1 {
            // TODO: fix this + 1 once we start counting from 1
            return Err(ApplyError::ChunkOutOfBounds {
                line: chunk.start_line,
                num_lines: chunk.num_lines,
                file_lines,
            });
        }
    }
    Ok(())
}

enum Segment<'a> {
    Chunk(&'a Chunk),
    Content(&'a str),
}

fn segments_from_chunks<'a>(
    mut chunks: &'a [Chunk],
    mut content: &'a str,
) -> impl Iterator<Item = Segment<'a>> + 'a {
    let mut line = 1;
    std::iter::from_fn(move || {
        if let Some((cur, next)) = chunks.split_first() {
            let line_diff = cur.start_line - line;
            if line_diff == 0 {
                // Skip the lines that the chunk is replacing
                let skip_bytes: usize = content
                    .split_inclusive('\n')
                    .take(cur.num_lines)
                    .map(str::len)
                    .sum();

                content = &content[skip_bytes..];
                line = cur.start_line + cur.num_lines;
                chunks = next;

                return Some(Segment::Chunk(cur));
            }
            let next = content
                .split_inclusive('\n')
                .take(line_diff)
                .map(str::len)
                .sum();
            let (to_yield, new_content) = content.split_at(next);

            line = cur.start_line;
            content = new_content;

            return Some(Segment::Content(to_yield));
        }

        if content.is_empty() {
            return None;
        }

        let next = content;
        content = "";
        Some(Segment::Content(next))
    })
}

/// Apply chunks to file content, producing the modified file content.
///
/// # Arguments
/// * `chunks` - The modifications to apply (must all be for the same file)
/// * `content` - The original file content
///
/// # Returns
/// The modified file content with chunks applied
///
/// # Errors
/// Returns an error if:
/// - Chunks have different paths
/// - Chunks overlap
/// - Chunks reference lines outside the file
pub fn apply_format(chunks: &[Chunk], content: &str) -> Result<String, Vec<ApplyError>> {
    if chunks.is_empty() {
        return Ok(content.to_string());
    }

    let mut errors = Vec::new();
    let mut handle_error = |result: Result<(), ApplyError>| {
        if let Err(err) = result {
            errors.push(err);
        }
    };

    handle_error(chunks_are_all_for_same_path(chunks));
    handle_error(chunks_are_within_file_bounds(chunks, content));
    // TODO: fix this
    // chunks are zero indexed so checking if it's greater than 1 is bad
    // chunks_have_valid_line_numbers(chunks)?;
    //

    // to check if something is overlapping we need to first ensure the chunks are sorted
    handle_error(
        chunks_are_sorted_by_line_number(chunks).and_then(|_| chunks_are_not_overlapping(chunks)),
    );
    handle_error(chunks_are_have_non_zero_length(chunks));

    if !errors.is_empty() {
        return Err(errors);
    }

    // Reconstruct final string
    let result: String = segments_from_chunks(chunks, content)
        .map(|seg| match seg {
            Segment::Chunk(chunk) => &chunk.content,
            Segment::Content(res) => res,
        })
        .collect();

    Ok(result)
}

// TODO: change this function so it validates everything first before modifying the file.
pub fn apply_format_to_fs(
    format: &mut Format,
    fs: &mut dyn FileSystem,
) -> Result<(), Vec<ApplyError>> {
    let mut errors = Vec::new();

    for (path, chunks) in format.file_chunks() {
        let write_result = fs
            .read_to_string(path)
            .map_err(|e| {
                vec![ApplyError::ModifyError {
                    path: path.to_path_buf(),
                    source: e,
                }]
            })
            .and_then(|content| {
                let modified_content = apply_format(chunks, &content)?;
                fs.write_string(path, &modified_content).map_err(|e| {
                    vec![ApplyError::ModifyError {
                        path: path.to_path_buf(),
                        source: e,
                    }]
                })?;
                Ok(())
            });

        if let Err(err) = write_result {
            errors.extend(err);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
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
    #[ignore]
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
}
