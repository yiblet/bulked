use miette::{Diagnostic, SourceSpan};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

/// Errors that can occur while parsing the format.
#[derive(Debug, Error, Diagnostic)]
pub enum FormatError {
    #[error("Invalid start delimiter")]
    #[diagnostic(
        code(format::invalid_delimiter),
        help("Expected format: @<path>:<line>:<numlines>")
    )]
    InvalidDelimiter {
        #[source_code]
        src: String,
        #[label("Invalid delimiter here")]
        span: SourceSpan,
    },

    #[error("Invalid line number: {value}")]
    #[diagnostic(
        code(format::invalid_line_number),
        help("Line number must be a positive integer")
    )]
    InvalidLineNumber {
        value: String,
        #[source_code]
        src: String,
        #[label("Expected a number here")]
        span: SourceSpan,
    },

    #[error("Invalid numlines: {value}")]
    #[diagnostic(
        code(format::invalid_numlines),
        help("numlines must be a positive integer")
    )]
    InvalidNumLines {
        value: String,
        #[source_code]
        src: String,
        #[label("Expected a number here")]
        span: SourceSpan,
    },

    #[error("Missing end delimiter @@@")]
    #[diagnostic(
        code(format::missing_end_delimiter),
        help("Each chunk must be terminated with @@@")
    )]
    MissingEndDelimiter {
        #[source_code]
        src: String,
        #[label("Chunk started here")]
        start_span: SourceSpan,
        #[label("Expected @@@ before end of file")]
        eof_span: SourceSpan,
    },

    #[error("No chunks found in input")]
    #[diagnostic(
        code(format::no_chunks),
        help("File must contain at least one chunk starting with @<path>:<line>:<numlines>")
    )]
    NoChunks {
        #[source_code]
        src: String,
    },

    #[error("No path found in input")]
    #[diagnostic(code(format::no_path))]
    NoPath,
}

/// Format represents a structured file format for storing code chunks with metadata.
///
/// # File Format Specification
///
/// The format uses a simple text-based structure:
///
/// ```text
/// @/path/to/file.txt:line:numlines
/// <chunk content>
/// @@@
///
/// @/path/to/file.txt:line:numlines
/// <chunk content>
/// @@@
/// ```
///
/// ## Format Rules
///
/// - **Start delimiter**: `@<path>:<line>:<numlines>` marks the beginning of a chunk
///   - `<path>`: Absolute or relative file path
///   - `<line>`: Starting line number (1-indexed)
///   - `<numlines>`: Number of lines in the chunk
///
/// - **End delimiter**: `@@@` marks the end of a chunk
///
/// - **Comments**: Text between chunks (outside delimiters) is ignored and can be used for comments
///
/// - **Escape sequences**: Inside chunk content:
///   - `\\` represents a literal backslash
///   - `\@` represents a literal at symbol
///
/// ## Example
///
/// ```text
/// @src/main.rs:10:3
/// fn main() {
///     println!("Hello, world!");
/// }
/// @@@
///
/// This is a comment - it will be ignored
///
/// @src/lib.rs:5:2
/// pub fn greet() \{
///     println!("Hi from lib");
/// \}
/// @@@
/// ```
/// Format is a collection of chunks
#[derive(Debug)]
pub struct Format(pub Vec<Chunk>);

impl Format {
    /// Returns the number of chunks in the format.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Converts a slice of match results into a Format.
    /// Each match result is converted to a chunk containing the match line
    /// along with its before and after context lines.
    pub fn from_matches(matches: &[crate::types::MatchResult]) -> Self {
        let chunks: Vec<Chunk> = matches
            .iter()
            .map(|match_result| {
                // Calculate the starting line number (accounting for context before)
                let start_line = match match_result.context_before.as_slice() {
                    [] => match_result.line_number,
                    [first, ..] => first.line_number,
                };

                // Build the content from context_before + match line + context_after
                let mut content_lines = Vec::new();

                // Add context before
                for ctx in &match_result.context_before {
                    content_lines.push(ctx.content.as_str());
                }

                // Add the match line itself
                content_lines.push(&match_result.line_content);

                // Add context after
                for ctx in &match_result.context_after {
                    content_lines.push(ctx.content.as_str());
                }

                let content = content_lines.join("\n");
                let num_lines = content_lines.len();

                Chunk::new(
                    match_result.file_path.clone(),
                    start_line,
                    num_lines,
                    content,
                )
            })
            .collect();

        Self(chunks)
    }

    fn sort(&mut self) {
        self.0.sort_by(|c1, c2| c1.as_ref().cmp(&c2.as_ref()));
    }

    /// Merges all overlapping or adjacent chunks in the format.
    /// Chunks are first sorted by path and position, then consecutive mergeable chunks
    /// are combined into single chunks.
    pub fn merge(&mut self) {
        self.sort();
        if self.0.len() < 2 {
            return;
        }

        let mut result = Vec::new();
        let mut chunks = std::mem::take(&mut self.0);
        let mut current = chunks.remove(0); // Take first chunk

        for chunk in chunks {
            match current.merge(chunk) {
                Ok(()) => {
                    // Merge succeeded, current is now updated in-place
                }
                Err(chunk) => {
                    // Merge failed, push current and start new one
                    result.push(current);
                    current = chunk;
                }
            }
        }

        // Don't forget the last chunk
        result.push(current);
        self.0 = result;
    }

    pub fn file_chunks(&mut self) -> Vec<(&Path, &[Chunk])> {
        self.sort();

        let mut res = Vec::new();
        let mut cur_file = None;
        for (idx, chunk) in self.0.iter().enumerate() {
            match cur_file {
                None => {
                    cur_file = Some((0, chunk));
                }
                Some((start, start_chunk)) if chunk.path != start_chunk.path => {
                    res.push((start_chunk.path.as_ref(), &self.0[start..idx]));
                    cur_file = Some((idx, chunk));
                }
                _ => {}
            }
        }

        if let Some((start, chunk)) = cur_file {
            res.push((chunk.path.as_path(), &self.0[start..]));
        }

        res
    }

    #[must_use]
    pub fn chunks(&self) -> BTreeMap<ChunkRef<'_>, &Chunk> {
        self.0
            .iter()
            .map(|chunk| {
                (
                    ChunkRef {
                        path: chunk.path.as_path(),
                        start_line: chunk.start_line,
                        num_lines: chunk.num_lines,
                    },
                    chunk,
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkRef<'a> {
    pub path: &'a Path,
    pub start_line: usize,
    pub num_lines: usize,
}

/// Chunk represents a single code snippet with its metadata and content.
#[derive(Debug)]
pub struct Chunk {
    pub path: PathBuf,
    pub start_line: usize,
    pub num_lines: usize,
    pub content: String,
}

impl Chunk {
    /// Creates a new Chunk with the given path, start line, number of lines, and content.
    #[must_use]
    pub fn new(path: PathBuf, start_line: usize, num_lines: usize, content: String) -> Self {
        Self {
            path,
            start_line,
            num_lines,
            content,
        }
    }

    #[must_use]
    pub fn as_ref(&self) -> ChunkRef<'_> {
        ChunkRef {
            path: self.path.as_path(),
            start_line: self.start_line,
            num_lines: self.num_lines,
        }
    }

    /// Determines if this chunk can be merged with another chunk.
    /// Two chunks can be merged if they have the same path and are either:
    /// - Sequential (no gaps between them)
    /// - Overlapping
    #[must_use]
    pub fn can_merge(&self, other: &Chunk) -> bool {
        if self.path != other.path {
            return false;
        }

        let self_end = self.start_line + self.num_lines;
        let other_end = other.start_line + other.num_lines;

        // Check if chunks are sequential or overlapping
        // Sequential: one chunk ends where the other begins
        // Overlapping: chunks share some lines
        self_end >= other.start_line && other_end >= self.start_line
    }

    /// Merges another chunk into this chunk, updating this chunk in place.
    /// The chunks must be mergeable (use `can_merge` to check first).
    ///
    /// For adjacent chunks (no gap), the content is concatenated with a newline.
    /// For overlapping chunks, the merge keeps all unique lines from both chunks,
    /// with the earlier chunk's content taking precedence for the overlapping region.
    ///
    /// # Errors
    ///
    /// Returns `Err(other)` if the chunks cannot be merged (different paths or non-overlapping/non-adjacent).
    pub fn merge(&mut self, other: Chunk) -> Result<(), Chunk> {
        if !self.can_merge(&other) {
            return Err(other);
        }

        let self_end = self.start_line + self.num_lines;
        let other_end = other.start_line + other.num_lines;

        // Calculate the merged boundaries
        let merged_start = self.start_line.min(other.start_line);
        let merged_end = self_end.max(other_end);
        let merged_num_lines = merged_end - merged_start;

        // Merge content based on chunk positions
        let merged_content = if self.start_line <= other.start_line {
            // self comes first or they start at the same line
            if self_end >= other.start_line + other.num_lines {
                // other is completely contained in self, keep self's content
                self.content.clone()
            } else if self_end == other.start_line {
                // Chunks are adjacent, concatenate
                format!("{}\n{}", self.content, other.content)
            } else {
                // Overlapping: self comes first, other extends beyond
                // Keep self's content and append the non-overlapping part of other
                let overlap_lines = self_end - other.start_line;
                let other_lines: Vec<&str> = other.content.lines().collect();
                let non_overlapping: Vec<&str> =
                    other_lines.iter().skip(overlap_lines).copied().collect();

                if non_overlapping.is_empty() {
                    self.content.clone()
                } else {
                    format!("{}\n{}", self.content, non_overlapping.join("\n"))
                }
            }
        } else {
            // other comes first
            if other_end >= self.start_line + self.num_lines {
                // self is completely contained in other, use other's content
                other.content
            } else if other_end == self.start_line {
                // Chunks are adjacent, concatenate
                format!("{}\n{}", other.content, self.content)
            } else {
                // Overlapping: other comes first, self extends beyond
                let overlap_lines = other_end - self.start_line;
                let self_lines: Vec<&str> = self.content.lines().collect();
                let non_overlapping: Vec<&str> =
                    self_lines.iter().skip(overlap_lines).copied().collect();

                if non_overlapping.is_empty() {
                    other.content
                } else {
                    format!("{}\n{}", other.content, non_overlapping.join("\n"))
                }
            }
        };

        // Update self with merged values
        self.start_line = merged_start;
        self.num_lines = merged_num_lines;
        self.content = merged_content;

        Ok(())
    }
}

impl fmt::Display for Format {
    /// Serializes the Format to the file format string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (idx, chunk) in self.0.iter().enumerate() {
            if idx != 0 {
                f.write_str("\n")?;
            };

            // Start delimiter: @path:line:numlines
            writeln!(
                f,
                "@{}:{}:{}",
                chunk.path.display(),
                chunk.start_line,
                chunk.num_lines
            )?;

            // Escaped content
            writeln!(
                f,
                "{}",
                crate::format::escaping::escape_content(&chunk.content)
            )?;

            // End delimiter
            writeln!(f, "@@@")?;
        }

        Ok(())
    }
}

impl FromStr for Format {
    type Err = FormatError;

    /// Parses a Format from the file format string using nom parser combinators.
    ///
    /// # Errors
    ///
    /// Returns a detailed error with source location if the format is invalid.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::format::parse::parse_format(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_chunk_new() {
        let chunk = Chunk::new(PathBuf::from("test.txt"), 42, 1, "test content".to_string());
        assert_eq!(chunk.start_line, 42);
        assert_eq!(chunk.num_lines, 1);
        assert_eq!(chunk.content, "test content");
        assert_eq!(chunk.path, PathBuf::from("test.txt"));
    }

    #[test]
    fn test_format_to_string_single_chunk() {
        let format = Format(vec![Chunk::new(
            PathBuf::from("src/main.rs"),
            10,
            3,
            "fn main() {\n    println!(\"Hello\");\n}".to_string(),
        )]);

        let output = format.to_string();
        assert!(output.contains("@src/main.rs:10:3"));
        assert!(output.contains("fn main()"));
        assert!(output.contains("@@@"));
    }

    #[test]
    fn test_format_to_string_multiple_chunks() {
        let format = Format(vec![
            Chunk::new(PathBuf::from("test.txt"), 5, 1, "line 5".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 10, 1, "line 10".to_string()),
        ]);

        let output = format.to_string();
        assert!(output.contains("@test.txt:5:1"));
        assert!(output.contains("@test.txt:10:1"));
        assert!(output.contains("line 5"));
        assert!(output.contains("line 10"));
        // Should have two @@@ delimiters
        assert_eq!(output.matches("@@@").count(), 2);
    }

    #[test]
    fn test_format_to_string_with_special_chars() {
        let format = Format(vec![Chunk::new(
            PathBuf::from("test.txt"),
            1,
            1,
            "user@domain.com\\path".to_string(),
        )]);

        let output = format.to_string();
        // Special characters should be escaped
        assert!(output.contains("user\\@domain.com\\\\path"));
    }

    #[test]
    fn test_format_roundtrip() {
        let original = Format(vec![
            Chunk::new(
                PathBuf::from("src/lib.rs"),
                1,
                3,
                "pub fn test() {\n    // test\n}".to_string(),
            ),
            Chunk::new(
                PathBuf::from("src/lib.rs"),
                20,
                1,
                "fn another() {}".to_string(),
            ),
        ]);

        let serialized = original.to_string();
        let deserialized = Format::from_str(&serialized).unwrap();

        assert_eq!(deserialized.0.len(), original.0.len());
        for (i, chunk) in deserialized.0.iter().enumerate() {
            assert_eq!(chunk.path, original.0[i].path);
            assert_eq!(chunk.start_line, original.0[i].start_line);
            assert_eq!(chunk.num_lines, original.0[i].num_lines);
            assert_eq!(chunk.content, original.0[i].content);
        }
    }

    #[test]
    fn test_format_roundtrip_with_special_chars() {
        let original = Format(vec![Chunk::new(
            PathBuf::from("test.txt"),
            1,
            2,
            "@ symbol and \\ backslash\nuser@email.com\\path\\to\\file".to_string(),
        )]);

        let serialized = original.to_string();
        let deserialized = Format::from_str(&serialized).unwrap();

        assert_eq!(deserialized.0[0].content, original.0[0].content);
    }

    #[test]
    fn test_chunk_can_merge_same_path_adjacent() {
        let chunk1 = Chunk::new(
            PathBuf::from("test.txt"),
            1,
            3,
            "line1\nline2\nline3".to_string(),
        );
        let chunk2 = Chunk::new(PathBuf::from("test.txt"), 4, 2, "line4\nline5".to_string());
        assert!(chunk1.can_merge(&chunk2));
    }

    #[test]
    fn test_chunk_can_merge_overlapping() {
        let chunk1 = Chunk::new(
            PathBuf::from("test.txt"),
            1,
            4,
            "line1\nline2\nline3\nline4".to_string(),
        );
        let chunk2 = Chunk::new(
            PathBuf::from("test.txt"),
            3,
            3,
            "line3\nline4\nline5".to_string(),
        );
        assert!(chunk1.can_merge(&chunk2));
    }

    #[test]
    fn test_chunk_cannot_merge_different_paths() {
        let chunk1 = Chunk::new(PathBuf::from("test1.txt"), 1, 3, "content1".to_string());
        let chunk2 = Chunk::new(PathBuf::from("test2.txt"), 1, 3, "content2".to_string());
        assert!(!chunk1.can_merge(&chunk2));
    }

    #[test]
    fn test_chunk_cannot_merge_non_overlapping() {
        let chunk1 = Chunk::new(PathBuf::from("test.txt"), 1, 3, "content1".to_string());
        let chunk2 = Chunk::new(PathBuf::from("test.txt"), 5, 3, "content2".to_string());
        assert!(!chunk1.can_merge(&chunk2));
    }

    #[test]
    fn test_chunk_merge_adjacent() {
        let mut chunk1 = Chunk::new(
            PathBuf::from("test.txt"),
            1,
            3,
            "line1\nline2\nline3".to_string(),
        );
        let chunk2 = Chunk::new(PathBuf::from("test.txt"), 4, 2, "line4\nline5".to_string());

        chunk1.merge(chunk2).unwrap();

        assert_eq!(chunk1.start_line, 1);
        assert_eq!(chunk1.num_lines, 5);
        assert_eq!(chunk1.content, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn test_chunk_merge_overlapping() {
        let mut chunk1 = Chunk::new(
            PathBuf::from("test.txt"),
            1,
            4,
            "line1\nline2\nline3\nline4".to_string(),
        );
        let chunk2 = Chunk::new(
            PathBuf::from("test.txt"),
            3,
            3,
            "line3\nline4\nline5".to_string(),
        );

        chunk1.merge(chunk2).unwrap();

        assert_eq!(chunk1.start_line, 1);
        assert_eq!(chunk1.num_lines, 5);
        // The merge should keep chunk1's content for the overlap and append the non-overlapping part
        assert_eq!(chunk1.content, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn test_chunk_merge_contained() {
        let mut chunk1 = Chunk::new(
            PathBuf::from("test.txt"),
            1,
            5,
            "line1\nline2\nline3\nline4\nline5".to_string(),
        );
        let chunk2 = Chunk::new(PathBuf::from("test.txt"), 2, 2, "line2\nline3".to_string());

        chunk1.merge(chunk2).unwrap();

        assert_eq!(chunk1.start_line, 1);
        assert_eq!(chunk1.num_lines, 5);
        // chunk2 is completely contained in chunk1, so content stays the same
        assert_eq!(chunk1.content, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn test_chunk_merge_reverse_order() {
        let mut chunk1 = Chunk::new(PathBuf::from("test.txt"), 4, 2, "line4\nline5".to_string());
        let chunk2 = Chunk::new(
            PathBuf::from("test.txt"),
            1,
            3,
            "line1\nline2\nline3".to_string(),
        );

        chunk1.merge(chunk2).unwrap();

        assert_eq!(chunk1.start_line, 1);
        assert_eq!(chunk1.num_lines, 5);
        assert_eq!(chunk1.content, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn test_chunk_merge_returns_error_for_different_paths() {
        let mut chunk1 = Chunk::new(PathBuf::from("test1.txt"), 1, 3, "content1".to_string());
        let chunk2 = Chunk::new(PathBuf::from("test2.txt"), 1, 3, "content2".to_string());

        let result = chunk1.merge(chunk2);
        assert!(result.is_err());
    }

    // Tests for Format::merge

    #[test]
    fn test_format_merge_empty() {
        let mut format = Format(vec![]);
        format.merge();
        assert_eq!(format.len(), 0);
    }

    #[test]
    fn test_format_merge_single_chunk() {
        let mut format = Format(vec![Chunk::new(
            PathBuf::from("test.txt"),
            1,
            3,
            "line1\nline2\nline3".to_string(),
        )]);
        format.merge();
        assert_eq!(format.len(), 1);
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[0].num_lines, 3);
    }

    #[test]
    fn test_format_merge_two_adjacent_chunks() {
        let mut format = Format(vec![
            Chunk::new(
                PathBuf::from("test.txt"),
                1,
                3,
                "line1\nline2\nline3".to_string(),
            ),
            Chunk::new(PathBuf::from("test.txt"), 4, 2, "line4\nline5".to_string()),
        ]);
        format.merge();
        assert_eq!(format.len(), 1);
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[0].num_lines, 5);
        assert_eq!(format.0[0].content, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn test_format_merge_two_overlapping_chunks() {
        let mut format = Format(vec![
            Chunk::new(
                PathBuf::from("test.txt"),
                1,
                4,
                "line1\nline2\nline3\nline4".to_string(),
            ),
            Chunk::new(
                PathBuf::from("test.txt"),
                3,
                3,
                "line3\nline4\nline5".to_string(),
            ),
        ]);
        format.merge();
        assert_eq!(format.len(), 1);
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[0].num_lines, 5);
        assert_eq!(format.0[0].content, "line1\nline2\nline3\nline4\nline5");
    }

    #[test]
    fn test_format_merge_two_non_adjacent_chunks() {
        let mut format = Format(vec![
            Chunk::new(
                PathBuf::from("test.txt"),
                1,
                3,
                "line1\nline2\nline3".to_string(),
            ),
            Chunk::new(PathBuf::from("test.txt"), 7, 2, "line7\nline8".to_string()),
        ]);
        format.merge();
        // Should remain 2 chunks since they can't be merged
        assert_eq!(format.len(), 2);
        // Check they're in the correct order
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[1].start_line, 7);
    }

    #[test]
    fn test_format_merge_unsorted_chunks() {
        let mut format = Format(vec![
            Chunk::new(PathBuf::from("test.txt"), 7, 2, "line7\nline8".to_string()),
            Chunk::new(
                PathBuf::from("test.txt"),
                1,
                3,
                "line1\nline2\nline3".to_string(),
            ),
            Chunk::new(PathBuf::from("test.txt"), 4, 2, "line4\nline5".to_string()),
        ]);
        format.merge();
        // First and second should merge (1-3 and 4-5), third stays separate (7-8)
        assert_eq!(format.len(), 2);
        // Check the merged chunk
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[0].num_lines, 5);
        assert_eq!(format.0[1].start_line, 7);
    }

    #[test]
    fn test_format_merge_multiple_files() {
        let mut format = Format(vec![
            Chunk::new(
                PathBuf::from("file1.txt"),
                1,
                2,
                "file1 line1\nfile1 line2".to_string(),
            ),
            Chunk::new(
                PathBuf::from("file1.txt"),
                3,
                2,
                "file1 line3\nfile1 line4".to_string(),
            ),
            Chunk::new(
                PathBuf::from("file2.txt"),
                1,
                2,
                "file2 line1\nfile2 line2".to_string(),
            ),
            Chunk::new(
                PathBuf::from("file2.txt"),
                3,
                2,
                "file2 line3\nfile2 line4".to_string(),
            ),
        ]);
        format.merge();
        // Should merge to 2 chunks (one per file)
        assert_eq!(format.len(), 2);

        // Find chunks by path
        let file1_chunks: Vec<_> = format
            .0
            .iter()
            .filter(|c| c.path == PathBuf::from("file1.txt"))
            .collect();
        let file2_chunks: Vec<_> = format
            .0
            .iter()
            .filter(|c| c.path == PathBuf::from("file2.txt"))
            .collect();

        assert_eq!(file1_chunks.len(), 1);
        assert_eq!(file2_chunks.len(), 1);
        assert_eq!(file1_chunks[0].num_lines, 4);
        assert_eq!(file2_chunks[0].num_lines, 4);
    }

    #[test]
    fn test_format_merge_all_mergeable() {
        let mut format = Format(vec![
            Chunk::new(PathBuf::from("test.txt"), 1, 2, "line1\nline2".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 3, 2, "line3\nline4".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 5, 2, "line5\nline6".to_string()),
            Chunk::new(PathBuf::from("test.txt"), 7, 2, "line7\nline8".to_string()),
        ]);
        format.merge();
        // All should merge into one chunk
        assert_eq!(format.len(), 1);
        assert_eq!(format.0[0].start_line, 1);
        assert_eq!(format.0[0].num_lines, 8);
    }

    #[test]
    fn test_file_chunks_multiple_files() {
        // This test verifies the fix for the bug where file_chunks would panic
        // when grouping chunks from multiple files. The bug was on line 232 where
        // it used chunk.path instead of start_chunk.path when transitioning between files.
        let mut format = Format(vec![
            Chunk::new(PathBuf::from("src/main.rs"), 1, 2, "fn main() {\n    println!(\"Hello\");".to_string()),
            Chunk::new(PathBuf::from("src/main.rs"), 10, 1, "// comment".to_string()),
            Chunk::new(PathBuf::from("src/lib.rs"), 5, 3, "pub fn test() {\n    // test\n}".to_string()),
            Chunk::new(PathBuf::from("src/lib.rs"), 20, 2, "pub fn another() {\n}".to_string()),
            Chunk::new(PathBuf::from("tests/integration.rs"), 1, 1, "#[test]".to_string()),
        ]);

        let file_chunks = format.file_chunks();

        // Should have 3 different files
        assert_eq!(file_chunks.len(), 3);

        // Verify first file (src/lib.rs comes first alphabetically after sorting)
        assert_eq!(file_chunks[0].0, Path::new("src/lib.rs"));
        assert_eq!(file_chunks[0].1.len(), 2);
        assert_eq!(file_chunks[0].1[0].start_line, 5);
        assert_eq!(file_chunks[0].1[1].start_line, 20);

        // Verify second file (src/main.rs)
        assert_eq!(file_chunks[1].0, Path::new("src/main.rs"));
        assert_eq!(file_chunks[1].1.len(), 2);
        assert_eq!(file_chunks[1].1[0].start_line, 1);
        assert_eq!(file_chunks[1].1[1].start_line, 10);

        // Verify third file (tests/integration.rs)
        assert_eq!(file_chunks[2].0, Path::new("tests/integration.rs"));
        assert_eq!(file_chunks[2].1.len(), 1);
        assert_eq!(file_chunks[2].1[0].start_line, 1);

        // Verify that all chunks in each group have the correct path
        for (path, chunks) in file_chunks {
            for chunk in chunks {
                assert_eq!(chunk.path.as_path(), path,
                    "Chunk path mismatch: expected {:?}, got {:?}", path, chunk.path);
            }
        }
    }
}
