use super::escaping::unescape_content;
use super::types::{Chunk, Format, FormatError};
use nom::{
    IResult,
    branch::alt,
    bytes::complete::{tag, take_till1},
    character::complete::{char, line_ending, not_line_ending, space0},
    combinator::recognize,
    error::{ErrorKind, ParseError as NomParseError},
    multi::many0,
    sequence::{preceded, tuple},
};
use std::path::PathBuf;

/// Custom nom error type that carries context for generating FormatError
#[derive(Debug, Clone)]
pub(super) struct ParserError {
    pub(super) kind: ParserErrorKind,
}

#[derive(Debug, Clone)]
pub(super) enum ParserErrorKind {
    InvalidDelimiter {
        offset: usize,
    },
    InvalidLineNumber {
        value: String,
        offset: usize,
        len: usize,
    },
    InvalidNumLines {
        value: String,
        offset: usize,
        len: usize,
    },
    MissingEndDelimiter {
        start_offset: usize,
        start_len: usize,
    },
    Nom,
}

impl<'a> NomParseError<&'a str> for ParserError {
    fn from_error_kind(_input: &'a str, _kind: ErrorKind) -> Self {
        ParserError {
            kind: ParserErrorKind::Nom,
        }
    }

    fn append(_input: &'a str, _kind: ErrorKind, other: Self) -> Self {
        other
    }
}

impl ParserError {
    pub(super) fn new(kind: ParserErrorKind) -> Self {
        ParserError { kind }
    }

    pub(super) fn into_format_error(self, source: &str) -> FormatError {
        let src = source.to_string();
        match self.kind {
            ParserErrorKind::InvalidDelimiter { offset } => {
                let end = source[offset..]
                    .find('\n')
                    .map(|i| offset + i)
                    .unwrap_or(source.len());
                FormatError::InvalidDelimiter {
                    src,
                    span: (offset, end - offset).into(),
                }
            }
            ParserErrorKind::InvalidLineNumber { value, offset, len } => {
                FormatError::InvalidLineNumber {
                    value,
                    src,
                    span: (offset, len).into(),
                }
            }
            ParserErrorKind::InvalidNumLines { value, offset, len } => {
                FormatError::InvalidNumLines {
                    value,
                    src,
                    span: (offset, len).into(),
                }
            }
            ParserErrorKind::MissingEndDelimiter {
                start_offset,
                start_len,
            } => FormatError::MissingEndDelimiter {
                src: src.clone(),
                start_span: (start_offset, start_len).into(),
                eof_span: (src.len().saturating_sub(1), 1).into(),
            },
            ParserErrorKind::Nom => FormatError::NoChunks { src },
        }
    }
}

type ParseResult<'a, T> = IResult<&'a str, T, ParserError>;

/// Calculate byte offset based on the remaining slice length
fn offset_from_len(full_len: usize, remaining: &str) -> usize {
    full_len - remaining.len()
}

fn invalid_delimiter_error(offset: usize) -> ParserError {
    ParserError::new(ParserErrorKind::InvalidDelimiter { offset })
}

fn parse_usize_segment<F>(
    segment: &str,
    offset: usize,
    err_builder: F,
) -> Result<usize, nom::Err<ParserError>>
where
    F: FnOnce(String, usize, usize) -> ParserErrorKind,
{
    segment.parse::<usize>().map_err(|_| {
        nom::Err::Failure(ParserError::new(err_builder(
            segment.to_string(),
            offset,
            segment.len(),
        )))
    })
}

/// Main entry point - parses the entire format
pub fn parse_format(src: &str) -> Result<Format, FormatError> {
    let full_len = src.len();
    // Skip leading whitespace/comments
    let (input, _) = skip_whitespace_and_comments(src).map_err(|e| match e {
        nom::Err::Error(e) | nom::Err::Failure(e) => e.into_format_error(src),
        nom::Err::Incomplete(_) => FormatError::NoChunks {
            src: src.to_string(),
        },
    })?;

    // Parse all chunks
    let (_, chunks) = many0(preceded(
        skip_whitespace_and_comments,
        chunk_parser(full_len),
    ))(input)
    .map_err(|e| match e {
        nom::Err::Error(e) | nom::Err::Failure(e) => e.into_format_error(src),
        nom::Err::Incomplete(_) => FormatError::NoChunks {
            src: src.to_string(),
        },
    })?;

    if chunks.is_empty() {
        return Err(FormatError::NoChunks {
            src: src.to_string(),
        });
    }

    Ok(Format(chunks))
}

/// Returns a parser that consumes a chunk with context for better diagnostics.
fn chunk_parser<'a>(full_len: usize) -> impl Fn(&'a str) -> ParseResult<'a, Chunk> {
    move |input| {
        let start_offset = offset_from_len(full_len, input);
        let header_len = input.lines().next().map(|l| l.len()).unwrap_or(0);

        let (input, (path, line_number, numlines)) =
            start_delimiter(full_len, start_offset)(input)?;

        let (input, content) = chunk_content(start_offset, header_len)(input)?;

        let (input, _) = parse_end_delimiter_nom(input)?;

        let unescaped_content = unescape_content(&content);
        Ok((
            input,
            Chunk::new(path, line_number, numlines, unescaped_content),
        ))
    }
}

/// Parser factory for the start delimiter: @path:line:numlines
fn start_delimiter<'a>(
    full_len: usize,
    start_offset: usize,
) -> impl Fn(&'a str) -> ParseResult<'a, (PathBuf, usize, usize)> {
    move |input| {
        // FIXME: is there a cleaner way to do this? 
        let invalid_failure = || nom::Err::Failure(invalid_delimiter_error(start_offset));
        let invalid_error = || nom::Err::Error(invalid_delimiter_error(start_offset));

        let (input, _) = char('@')(input).map_err(|_: nom::Err<ParserError>| invalid_error())?;
        let (input, path_str) = take_till1(|c| c == ':' || c == '\n')(input)
            .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
        let (input, _) = char(':')(input).map_err(|_: nom::Err<ParserError>| invalid_failure())?;

        let line_num_offset = offset_from_len(full_len, input);
        let (input, line_str) = take_till1(|c| c == ':' || c == '\n')(input)
            .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
        if input.starts_with('\n') {
            return Err(invalid_failure());
        }
        let line_number = parse_usize_segment(line_str, line_num_offset, |value, offset, len| {
            ParserErrorKind::InvalidLineNumber { value, offset, len }
        })?;

        let (input, _) = char(':')(input).map_err(|_: nom::Err<ParserError>| invalid_failure())?;

        let numlines_offset = offset_from_len(full_len, input);
        let (input, numlines_str) = take_till1(|c| c == '\n')(input)
            .map_err(|_: nom::Err<ParserError>| invalid_failure())?;
        let numlines = parse_usize_segment(numlines_str, numlines_offset, |value, offset, len| {
            ParserErrorKind::InvalidNumLines { value, offset, len }
        })?;
        
        let (input, _) = space0(input)?;
        let (input, _) = line_ending(input)?;

        Ok((input, (PathBuf::from(path_str), line_number, numlines)))
    }
}

/// Parser factory for chunk content until the @@@ end delimiter.
fn chunk_content<'a>(
    chunk_start_offset: usize,
    header_len: usize,
) -> impl Fn(&'a str) -> ParseResult<'a, String> {
    move |mut current| {
        let mut content = String::new();

        loop {
            if current.starts_with("@@@") {
                return Ok((current, content));
            }

            if current.is_empty() {
                return Err(nom::Err::Failure(ParserError::new(
                    ParserErrorKind::MissingEndDelimiter {
                        start_offset: chunk_start_offset,
                        start_len: header_len,
                    },
                )));
            }

            let (rest, line) = not_line_ending(current)?;

            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(line);

            current = match line_ending::<&str, ParserError>(rest) {
                Ok((rest, _)) => rest,
                Err(_) => rest,
            };
        }
    }
}

/// Parse end delimiter: @@@
fn parse_end_delimiter_nom<'a>(input: &'a str) -> ParseResult<'a, ()> {
    // FIXME: parse end delimiter should be permissive and allow characters after @@@
    // until the end of the line. 
    //
    // Add a test case for this as well.
    let (input, _) = tag("@@@")(input)?;
    let (input, _) = alt((line_ending, recognize(nom::combinator::eof)))(input)?;
    Ok((input, ()))
}

/// Skip whitespace and comment lines
fn skip_whitespace_and_comments<'a>(input: &'a str) -> ParseResult<'a, ()> {
    // FIXME: we can make this more efficient by using better combinators like 
    // multispace and sharing the line_ending combinator.
    let (input, _) = many0(alt((
        // Skip whitespace lines (line with only whitespace)
        recognize(tuple((many0(alt((char(' '), char('\t')))), line_ending))),
        // Skip comment lines (non-@ lines)
        recognize(tuple((
            nom::combinator::peek(nom::combinator::not(char('@'))),
            not_line_ending,
            line_ending,
        ))),
    )))(input)?;

    Ok((input, ()))
}

#[cfg(test)]
mod tests {
    use super::super::types::{Format, FormatError};
    use std::path::PathBuf;
    use std::str::FromStr;

    #[test]
    fn test_format_from_str_single_chunk() {
        let input = "@src/main.rs:10:2\nfn main() {\n    println!(\"Hello\");\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.0.len(), 1);
        assert_eq!(format.0[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(format.0[0].start_line, 10);
        assert_eq!(format.0[0].num_lines, 2);
        assert_eq!(
            format.0[0].content,
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
        assert_eq!(format.0.len(), 2);
        assert_eq!(format.0[0].path, PathBuf::from("test.txt"));
        assert_eq!(format.0[0].start_line, 5);
        assert_eq!(format.0[0].num_lines, 1);
        assert_eq!(format.0[0].content, "line 5");
        assert_eq!(format.0[1].path, PathBuf::from("test.txt"));
        assert_eq!(format.0[1].start_line, 10);
        assert_eq!(format.0[1].num_lines, 1);
        assert_eq!(format.0[1].content, "line 10");
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
        assert_eq!(format.0.len(), 2);
        assert_eq!(format.0[0].content, "content");
        assert_eq!(format.0[1].content, "more content");
    }

    #[test]
    fn test_format_from_str_with_escaped_chars() {
        let input = "@test.txt:1:1\nuser\\@domain.com\\\\path\n@@@\n";

        let format = Format::from_str(input).unwrap();
        assert_eq!(format.0[0].content, "user@domain.com\\path");
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
