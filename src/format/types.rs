use miette::{Diagnostic, SourceSpan};
use std::fmt;
use std::path::PathBuf;
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
    pub fn new(path: PathBuf, start_line: usize, num_lines: usize, content: String) -> Self {
        Self {
            path,
            start_line,
            num_lines,
            content,
        }
    }
}

impl fmt::Display for Format {
    /// Serializes the Format to the file format string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in &self.0 {
            // Start delimiter: @path:line:numlines
            writeln!(
                f,
                "@{}:{}:{}",
                chunk.path.display(),
                chunk.start_line,
                chunk.num_lines
            )?;

            // Escaped content
            writeln!(f, "{}", crate::format::escaping::escape_content(&chunk.content))?;

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
        let chunk = Chunk::new(
            PathBuf::from("test.txt"),
            42,
            1,
            "test content".to_string(),
        );
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
}
