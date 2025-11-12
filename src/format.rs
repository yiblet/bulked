use aho_corasick::AhoCorasick;
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

#[derive(Debug)]
pub struct Format {
    path: PathBuf,
    chunks: Vec<Chunk>,
}

/// Chunk represents a single code snippet with its line number and content.
#[derive(Debug)]
pub struct Chunk {
    line_number: usize,
    content: String,
}

impl Format {
    /// Creates a new Format with the given path and chunks.
    pub fn new(path: PathBuf, chunks: Vec<Chunk>) -> Self {
        Self { path, chunks }
    }
}

impl fmt::Display for Format {
    /// Serializes the Format to the file format string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in &self.chunks {
            let num_lines = chunk.content.lines().count();
            // Start delimiter: @path:line:numlines
            writeln!(
                f,
                "@{}:{}:{}",
                self.path.display(),
                chunk.line_number,
                num_lines
            )?;

            // Escaped content
            writeln!(f, "{}", escape_content(&chunk.content))?;

            // End delimiter
            writeln!(f, "@@@")?;
        }

        Ok(())
    }
}

impl FromStr for Format {
    type Err = FormatError;

    /// Parses a Format from the file format string.
    ///
    /// # Errors
    ///
    /// Returns a detailed error with source location if the format is invalid.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut chunks = Vec::new();
        let mut path: Option<PathBuf> = None;
        let src = s.to_string();

        let mut lines_iter = s.lines().enumerate().peekable();

        while let Some((line_idx, line)) = lines_iter.next() {
            // Calculate byte offset for this line
            let line_start = s.lines().take(line_idx).map(|l| l.len() + 1).sum::<usize>();

            // Skip empty lines and comments
            if line.trim().is_empty() || (!line.starts_with('@') && chunks.is_empty()) {
                continue;
            }

            // Check for start delimiter: @path:line:numlines
            if line.starts_with('@') && !line.starts_with("@@@") {
                // Parse the delimiter
                let delimiter = &line[1..]; // Remove leading @
                let parts: Vec<&str> = delimiter.split(':').collect();

                if parts.len() != 3 {
                    return Err(FormatError::InvalidDelimiter {
                        src,
                        span: (line_start, line.len()).into(),
                    });
                }

                let chunk_path = PathBuf::from(parts[0]);

                // Parse line number with position tracking
                let line_num_str = parts[1];
                let line_number = line_num_str.parse::<usize>().map_err(|_| {
                    let line_num_offset = line_start + 1 + parts[0].len() + 1; // @ + path + :
                    FormatError::InvalidLineNumber {
                        value: line_num_str.to_string(),
                        src: src.clone(),
                        span: (line_num_offset, line_num_str.len()).into(),
                    }
                })?;

                // Parse numlines with position tracking
                let numlines_str = parts[2];
                let _num_lines = numlines_str.parse::<usize>().map_err(|_| {
                    let numlines_offset = line_start + 1 + parts[0].len() + 1 + parts[1].len() + 1;
                    FormatError::InvalidNumLines {
                        value: numlines_str.to_string(),
                        src: src.clone(),
                        span: (numlines_offset, numlines_str.len()).into(),
                    }
                })?;

                // Set path if not already set
                if path.is_none() {
                    path = Some(chunk_path.clone());
                }

                let chunk_start = line_start;

                // Read content until @@@
                let mut content = String::new();
                let mut found_end = false;

                while let Some((_, content_line)) = lines_iter.peek() {
                    if content_line.trim() == "@@@" {
                        found_end = true;
                        lines_iter.next(); // Consume the @@@ line
                        break;
                    }

                    // Consume the content line
                    let (_, content_line) = lines_iter.next().unwrap();
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(content_line);
                }

                if !found_end {
                    return Err(FormatError::MissingEndDelimiter {
                        src: src.clone(),
                        start_span: (chunk_start, line.len()).into(),
                        eof_span: (src.len().saturating_sub(1), 1).into(),
                    });
                }

                // Unescape content
                let unescaped_content = unescape_content(&content);
                chunks.push(Chunk::new(line_number, unescaped_content));
            }
        }

        if chunks.is_empty() {
            return Err(FormatError::NoChunks { src });
        }

        let path = path.ok_or(FormatError::NoPath)?;
        Ok(Format::new(path, chunks))
    }
}

impl Chunk {
    /// Creates a new Chunk with the given line number and content.
    pub fn new(line_number: usize, content: String) -> Self {
        Self {
            line_number,
            content,
        }
    }

    /// Returns the line number of this chunk.
    pub fn line_number(&self) -> usize {
        self.line_number
    }

    /// Returns the content of this chunk.
    pub fn content(&self) -> &str {
        &self.content
    }
}

fn escape_content(content: &str) -> String {
    let patterns = &["\\", "@"];
    let replacements = &["\\\\", "\\@"];
    let ac = AhoCorasick::new(patterns).expect("Failed to create Aho-Corasick matcher");
    ac.replace_all(content, replacements)
}

fn unescape_content(content: &str) -> String {
    let patterns = &["\\@", "\\\\"];
    let replacements = &["@", "\\"];
    let ac = AhoCorasick::new(patterns).expect("Failed to create Aho-Corasick matcher");
    ac.replace_all(content, replacements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_content_empty_string() {
        assert_eq!(escape_content(""), "");
    }

    #[test]
    fn test_escape_content_no_special_chars() {
        assert_eq!(escape_content("hello world"), "hello world");
    }

    #[test]
    fn test_escape_content_backslash() {
        assert_eq!(escape_content("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_escape_content_at_symbol() {
        assert_eq!(escape_content("user@domain.com"), "user\\@domain.com");
    }

    #[test]
    fn test_escape_content_both_special_chars() {
        assert_eq!(escape_content("C:\\path@file"), "C:\\\\path\\@file");
    }

    #[test]
    fn test_escape_content_multiple_backslashes() {
        assert_eq!(escape_content("\\\\\\"), "\\\\\\\\\\\\");
    }

    #[test]
    fn test_escape_content_multiple_at_symbols() {
        assert_eq!(escape_content("@@test@@"), "\\@\\@test\\@\\@");
    }

    #[test]
    fn test_unescape_content_empty_string() {
        assert_eq!(unescape_content(""), "");
    }

    #[test]
    fn test_unescape_content_no_special_chars() {
        assert_eq!(unescape_content("hello world"), "hello world");
    }

    #[test]
    fn test_unescape_content_escaped_backslash() {
        assert_eq!(unescape_content("path\\\\to\\\\file"), "path\\to\\file");
    }

    #[test]
    fn test_unescape_content_escaped_at_symbol() {
        assert_eq!(unescape_content("user\\@domain.com"), "user@domain.com");
    }

    #[test]
    fn test_unescape_content_both_escaped_chars() {
        assert_eq!(unescape_content("C:\\\\path\\@file"), "C:\\path@file");
    }

    #[test]
    fn test_roundtrip_escape_unescape() {
        let original = "C:\\path\\to@file\\with@symbols";
        let escaped = escape_content(original);
        let unescaped = unescape_content(&escaped);
        assert_eq!(original, unescaped);
    }

    #[test]
    fn test_roundtrip_complex_string() {
        let original = "\\\\@@@\\\\test\\@value";
        let escaped = escape_content(original);
        let unescaped = unescape_content(&escaped);
        assert_eq!(original, unescaped);
    }

    #[test]
    fn test_escape_already_escaped() {
        // Escaping an already escaped string should double-escape
        assert_eq!(escape_content("\\\\"), "\\\\\\\\");
        assert_eq!(escape_content("\\@"), "\\\\\\@");
    }

    #[test]
    fn test_unescape_non_escaped() {
        // Unescaping a non-escaped string should leave it unchanged
        assert_eq!(unescape_content("regular text"), "regular text");
    }

    // Format and Chunk tests

    #[test]
    fn test_chunk_new() {
        let chunk = Chunk::new(42, "test content".to_string());
        assert_eq!(chunk.line_number(), 42);
        assert_eq!(chunk.content(), "test content");
    }

    #[test]
    fn test_format_new() {
        let path = PathBuf::from("src/main.rs");
        let chunks = vec![Chunk::new(1, "fn main() {}".to_string())];
        let format = Format::new(path.clone(), chunks);
        // Just verify it constructs without panic
        assert_eq!(format.path, path);
    }

    #[test]
    fn test_format_to_string_single_chunk() {
        let format = Format::new(
            PathBuf::from("src/main.rs"),
            vec![Chunk::new(
                10,
                "fn main() {\n    println!(\"Hello\");\n}".to_string(),
            )],
        );

        let output = format.to_string();
        assert!(output.contains("@src/main.rs:10:3"));
        assert!(output.contains("fn main()"));
        assert!(output.contains("@@@"));
    }

    #[test]
    fn test_format_to_string_multiple_chunks() {
        let format = Format::new(
            PathBuf::from("test.txt"),
            vec![
                Chunk::new(5, "line 5".to_string()),
                Chunk::new(10, "line 10".to_string()),
            ],
        );

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
        let format = Format::new(
            PathBuf::from("test.txt"),
            vec![Chunk::new(1, "user@domain.com\\path".to_string())],
        );

        let output = format.to_string();
        // Special characters should be escaped
        assert!(output.contains("user\\@domain.com\\\\path"));
    }

    #[test]
    fn test_format_from_str_single_chunk() {
        let input = "@src/main.rs:10:2\nfn main() {\n    println!(\"Hello\");\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.path, PathBuf::from("src/main.rs"));
        assert_eq!(format.chunks.len(), 1);
        assert_eq!(format.chunks[0].line_number(), 10);
        assert_eq!(
            format.chunks[0].content(),
            "fn main() {\n    println!(\"Hello\");"
        );
    }

    #[test]
    fn test_format_from_str_multiple_chunks() {
        let input = r#"@test.txt:5:1
line 5
@@@

@test.txt:10:1
line 10
@@@
"#;

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.chunks.len(), 2);
        assert_eq!(format.chunks[0].line_number(), 5);
        assert_eq!(format.chunks[0].content(), "line 5");
        assert_eq!(format.chunks[1].line_number(), 10);
        assert_eq!(format.chunks[1].content(), "line 10");
    }

    #[test]
    fn test_format_from_str_with_comments() {
        let input = r#"This is a comment at the start

@test.txt:1:1
content
@@@

This is a comment between chunks

@test.txt:5:1
more content
@@@
"#;

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.chunks.len(), 2);
        assert_eq!(format.chunks[0].content(), "content");
        assert_eq!(format.chunks[1].content(), "more content");
    }

    #[test]
    fn test_format_from_str_with_escaped_chars() {
        let input = "@test.txt:1:1\nuser\\@domain.com\\\\path\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.chunks[0].content(), "user@domain.com\\path");
    }

    #[test]
    fn test_format_from_str_invalid_delimiter() {
        let input = "@test.txt:invalid\ncontent\n@@@\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            FormatError::InvalidDelimiter { .. }
        ));
    }

    #[test]
    fn test_format_from_str_missing_end_delimiter() {
        let input = "@test.txt:1:1\ncontent without end delimiter\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            FormatError::MissingEndDelimiter { .. }
        ));
    }

    #[test]
    fn test_format_from_str_empty() {
        let input = "";
        let result = Format::from_str(input);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FormatError::NoChunks { .. }));
    }

    #[test]
    fn test_format_error_invalid_line_number() {
        let input = "@test.txt:not_a_number:5\ncontent\n@@@\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::InvalidLineNumber { value, .. } => {
                assert_eq!(value, "not_a_number");
            }
            _ => panic!("Expected InvalidLineNumber error"),
        }
    }

    #[test]
    fn test_format_error_invalid_numlines() {
        let input = "@test.txt:10:invalid\ncontent\n@@@\n";
        let result = Format::from_str(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::InvalidNumLines { value, .. } => {
                assert_eq!(value, "invalid");
            }
            _ => panic!("Expected InvalidNumLines error"),
        }
    }

    #[test]
    fn test_format_roundtrip() {
        let original = Format::new(
            PathBuf::from("src/lib.rs"),
            vec![
                Chunk::new(1, "pub fn test() {\n    // test\n}".to_string()),
                Chunk::new(20, "fn another() {}".to_string()),
            ],
        );

        let serialized = original.to_string();
        let deserialized = Format::from_str(&serialized).unwrap();

        assert_eq!(deserialized.path, original.path);
        assert_eq!(deserialized.chunks.len(), original.chunks.len());
        for (i, chunk) in deserialized.chunks.iter().enumerate() {
            assert_eq!(chunk.line_number(), original.chunks[i].line_number());
            assert_eq!(chunk.content(), original.chunks[i].content());
        }
    }

    #[test]
    fn test_format_roundtrip_with_special_chars() {
        let original = Format::new(
            PathBuf::from("test.txt"),
            vec![Chunk::new(
                1,
                "@ symbol and \\ backslash\nuser@email.com\\path\\to\\file".to_string(),
            )],
        );

        let serialized = original.to_string();
        let deserialized = Format::from_str(&serialized).unwrap();

        assert_eq!(
            deserialized.chunks[0].content(),
            original.chunks[0].content()
        );
    }

    /// This test demonstrates the beautiful error messages from miette.
    /// Run with: cargo test test_format_error_display -- --nocapture
    #[test]
    fn test_format_error_display() {
        use miette::Report;

        let test_cases = vec![
            (
                "Invalid line number",
                "@src/main.rs:not_a_number:10\nfn main() {}\n@@@\n",
            ),
            ("Invalid delimiter", "@src/main.rs:10\nfn main() {}\n@@@\n"),
            ("Missing end delimiter", "@src/main.rs:10:1\nfn main() {}\n"),
        ];

        for (name, input) in test_cases {
            println!("\n=== Test case: {} ===", name);
            match Format::from_str(input) {
                Ok(_) => println!("Unexpectedly succeeded!"),
                Err(e) => {
                    let report = Report::new(e);
                    println!("{:?}", report);
                }
            }
        }
    }
}
