use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, BufWriter, IsTerminal, Read, Write},
    path::PathBuf,
};

use clap::{Args, ValueEnum};

use crate::filesystem::physical::PhysicalFS;
use crate::filesystem::{FileSystem, ReadFs};
use crate::types::IngestInput;

/// Errors produced while decoding `(path, line)` records from ingest input.
///
/// These are specific to the CLI's input decoders (jsonl / json / csv / grep)
/// and are wrapped by a single transparent `cli::Error::IngestParse` variant.
#[derive(Debug, thiserror::Error)]
pub enum IngestParseError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Csv(#[from] csv::Error),

    #[error("csv does not contain the right headers. It must be at least path,line_number")]
    MissingHeaders,

    #[error("csv contains missing fields for {0}")]
    MissingFields(&'static str),

    #[error("csv could not parse {0}")]
    CouldNotParse(&'static str),
}

/// A stream of decoded ingest records borrowing the underlying reader.
type RecordIter<'r> = Box<dyn Iterator<Item = Result<IngestInput, IngestParseError>> + 'r>;

/// Normalized (see [`normalize_header`]) CSV header names accepted for the
/// file-path column.
const FILE_PATH_HEADERS: [&str; 3] = ["filepath", "file", "path"];

/// Normalized (see [`normalize_header`]) CSV header names accepted for the
/// line-number column.
const LINE_NUMBER_HEADERS: [&str; 5] = ["linenumber", "line", "linenum", "lineno", "ln"];

/// Lowercase ASCII letters and drop whitespace, `-` and `_`, so that
/// `File_Path`, `file path` and `FILE-PATH` all normalize to `filepath`.
fn normalize_header(h: &str) -> String {
    h.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '_'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Does a normalized header name match one of `names`, tolerating a plural `s`?
///
/// Uses `strip_suffix` rather than byte slicing so a header ending in a
/// multi-byte character can never split a UTF-8 sequence.
fn header_matches(norm: &str, names: &[&str]) -> bool {
    names.contains(&norm) || norm.strip_suffix('s').is_some_and(|s| names.contains(&s))
}

/// Split a `grep -n`-style line into `(path, line_number)`.
///
/// The path ends at the first `:` that is followed by a run of ASCII digits
/// terminated by another `:` or the end of the line. Colons not followed by
/// such a run (e.g. the drive letter in `C:\x:5:...`) are part of the path.
fn split_grep_line(line: &str) -> Option<(&str, usize)> {
    for (idx, _) in line.match_indices(':') {
        let rest = &line[idx + 1..];
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            continue;
        }
        let terminated = digits_end == rest.len() || rest.as_bytes()[digits_end] == b':';
        if !terminated {
            continue;
        }
        if let Ok(line_number) = rest[..digits_end].parse() {
            return Some((&line[..idx], line_number));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Format {
    Jsonl,
    Json,
    Csv,
    Grep,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum FormatOptions {
    #[default]
    Auto,
    Format(Format),
}

impl FormatOptions {
    fn parse<'r, R: Read + 'r>(self, mut r: R) -> RecordIter<'r> {
        if let Self::Format(format) = self {
            return format.parse(r);
        }

        let mut total = 0;
        let mut cur = [0u8; 1024];
        loop {
            match r.read(&mut cur[total..]) {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    total += n;
                    if total == cur.len() {
                        break;
                    }
                }
                Err(e) => match e.kind() {
                    std::io::ErrorKind::UnexpectedEof => break,
                    _ => return Box::new(std::iter::once(Err(IngestParseError::Io(e)))),
                },
            }
        }

        let guess = Format::guess(&cur[..total]);
        let buf: VecDeque<u8> = VecDeque::from(cur[..total].to_vec());
        guess.parse(buf.chain(r))
    }
}

impl ValueEnum for FormatOptions {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            FormatOptions::Format(Format::Jsonl),
            FormatOptions::Format(Format::Json),
            FormatOptions::Format(Format::Grep),
            FormatOptions::Format(Format::Csv),
            FormatOptions::Auto,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::Format(Format::Jsonl) => {
                clap::builder::PossibleValue::new("jsonl").help("parse as a jsonl")
            }
            Self::Format(Format::Json) => {
                clap::builder::PossibleValue::new("json").help("parse as a json")
            }
            Self::Format(Format::Grep) => {
                clap::builder::PossibleValue::new("grep").help("parse as a grep output")
            }
            Self::Format(Format::Csv) => {
                clap::builder::PossibleValue::new("csv").help("parse as a csv")
            }
            Self::Auto => clap::builder::PossibleValue::new("auto").help("auto-detect format"),
        })
    }
}

/// Column indices of the path and line-number fields in a CSV header row.
#[derive(Debug, Clone, Copy)]
struct HeaderLocs {
    file_path: usize,
    line_number: usize,
}

impl HeaderLocs {
    fn find(headers: &csv::StringRecord) -> Result<Self, IngestParseError> {
        let mut file_path = None;
        let mut line_number = None;

        for (i, h) in headers.iter().enumerate() {
            let norm = normalize_header(h);
            if header_matches(&norm, &FILE_PATH_HEADERS) {
                file_path = Some(i);
            } else if header_matches(&norm, &LINE_NUMBER_HEADERS) {
                line_number = Some(i);
            }
        }

        file_path
            .zip(line_number)
            .map(|(file_path, line_number)| Self {
                file_path,
                line_number,
            })
            .ok_or(IngestParseError::MissingHeaders)
    }
}

impl Format {
    fn parse_jsonl<'r, R: Read + 'r>(r: R) -> RecordIter<'r> {
        Box::new(BufReader::new(r).lines().map(|r| {
            let line = r?;
            Ok(serde_json::from_str(&line)?)
        }))
    }

    fn parse_json<'r, R: Read + 'r>(mut r: R) -> RecordIter<'r> {
        let mut content = String::new();
        let res: Result<Vec<IngestInput>, IngestParseError> = r
            .read_to_string(&mut content)
            .map_err(IngestParseError::from)
            .and_then(|_| serde_json::from_str(&content).map_err(IngestParseError::from));

        match res {
            Ok(v) => Box::new(v.into_iter().map(Ok)),
            Err(e) => Box::new(std::iter::once(Err(e))),
        }
    }

    fn parse_csv<'r, R: Read + 'r>(r: R) -> RecordIter<'r> {
        let mut rdr = csv::Reader::from_reader(r);

        let locs = match rdr
            .headers()
            .map_err(IngestParseError::from)
            .and_then(HeaderLocs::find)
        {
            Ok(locs) => locs,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };

        Box::new(rdr.into_records().map(move |r| {
            let r = r?;

            let file_path = r
                .get(locs.file_path)
                .ok_or(IngestParseError::MissingFields("file path"))?;
            let line_number = r
                .get(locs.line_number)
                .ok_or(IngestParseError::MissingFields("line number"))?
                .parse()
                .map_err(|_| IngestParseError::CouldNotParse("line number"))?;

            Ok(IngestInput {
                file_path: PathBuf::from(file_path),
                line_number,
            })
        }))
    }

    fn parse_grep<'r, R: Read + 'r>(r: R) -> RecordIter<'r> {
        Box::new(BufReader::new(r).lines().filter_map(|r| match r {
            Err(e) => Some(Err(IngestParseError::Io(e))),
            Ok(line) => match split_grep_line(&line) {
                Some((file, line_number)) => Some(Ok(IngestInput {
                    file_path: PathBuf::from(file),
                    line_number,
                })),
                None => {
                    tracing::warn!("skipping unparseable grep line: {line:?}");
                    None
                }
            },
        }))
    }

    pub fn parse<'r, R: Read + 'r>(self, r: R) -> RecordIter<'r> {
        match self {
            Self::Json => Self::parse_json(r),
            Self::Jsonl => Self::parse_jsonl(r),
            Self::Grep => Self::parse_grep(r),
            Self::Csv => Self::parse_csv(r),
        }
    }

    pub fn guess(line: &[u8]) -> Self {
        if line.starts_with(b"[{") {
            Self::Json
        } else if line.starts_with(b"{") {
            Self::Jsonl
        } else {
            line.iter()
                .find_map(|c| match c {
                    b',' => Some(Self::Csv),
                    b':' => Some(Self::Grep),
                    _ => None,
                })
                .unwrap_or(Self::Grep)
        }
    }
}

#[derive(Args, Debug)]
#[command(after_long_help = "\
`ingest` reads a list of file locations from stdin (or a file) and, for each one,
prints the surrounding lines as an editable `chunk`. This is how you get the
output of *any* tool into bulked: grep, ripgrep, compiler/linter errors, a CSV
of locations — anything that names a file and a line number.

The result is the same editable chunk format used everywhere in bulked: edit it
in your editor, then pipe it to `bulked apply` to write the changes back.

INPUT FORMATS (auto-detected, override with --format):
  jsonl  one JSON object per line, e.g. {\"path\":\"src/a.rs\",\"line\":12}
  json   a JSON array of those same objects
  csv    a header row naming a path column and a line column, then rows
  grep   classic `path:line:...` lines, e.g. `grep -n` / `rg -n` output

EXAMPLES:
  # plain `grep -n` style output straight into the editable format
  grep -rn 'TODO' src/ | bulked ingest > edits.bk

  # write to a file with -o (instead of redirecting)
  grep -rn 'TODO' src/ | bulked ingest -o edits.bk

  # a CSV exported from somewhere else, 5 lines of context
  bulked ingest locations.csv -C 5 -o edits.bk

  # a JSON array of {\"path\", \"line\"} objects
  bulked ingest --format json locations.json -o edits.bk

Now edit edits.bk and run `bulked apply --input edits.bk`.")]
pub(crate) struct IngestArgs {
    /// File of locations to read (default: stdin). Use '-' to force stdin.
    #[arg(default_value = None)]
    pub(crate) path: Option<PathBuf>,

    /// Input format. `auto` sniffs jsonl/json/csv/grep from the first bytes.
    #[arg(short, long = "format", default_value = "auto")]
    pub(crate) format: FormatOptions,

    /// Write the editable format to this file instead of stdout
    #[arg(short, long)]
    pub(crate) output: Option<PathBuf>,

    /// Lines of context to include before and after each location
    #[arg(short = 'C', long, default_value = "20")]
    pub(crate) context: usize,

    /// Print human-readable text instead of the editable chunk format
    #[arg(long)]
    pub(crate) plain: bool,
}

impl IngestArgs {
    /// Decode the `(path, line)` records from the locations file (via `fs`) or,
    /// when no path is given or it is `-`, from `input`.
    fn get_inputs(
        &self,
        fs: &dyn ReadFs,
        input: &mut dyn Read,
    ) -> Result<Vec<IngestInput>, super::Error> {
        let mut file;

        let stream: &mut dyn Read = match &self.path {
            Some(buf) if buf.as_os_str() != "-" => {
                file = fs.read(buf)?;
                &mut file
            }
            _ => input,
        };

        let inputs: Result<Vec<IngestInput>, IngestParseError> =
            self.format.clone().parse(stream).collect();
        Ok(inputs?)
    }

    /// Run `ingest` against an injected filesystem, input stream, and sinks.
    ///
    /// This owns all of the subcommand's behavior; [`IngestArgs::handle`] is
    /// the thin production wrapper. Locations are read from `fs` (or `input`
    /// when no path / `-` is given), context lines are read through `fs`, the
    /// chunk format is written to `--output` through `fs` or else to `out`, and
    /// the status line for `--output` goes to `err`. `color` says whether to
    /// ANSI-highlight the target lines.
    pub fn run(
        self,
        fs: &dyn FileSystem,
        input: &mut dyn Read,
        out: &mut dyn Write,
        err: &mut dyn Write,
        color: bool,
    ) -> Result<(), super::Error> {
        let inputs = self.get_inputs(fs, input)?;

        let result = crate::ingest::ingest(fs, inputs, self.context)?;

        let format = crate::format::Format::from_matches(&result);

        let mut file;
        let sink: &mut dyn Write = match &self.output {
            Some(path) => {
                file = BufWriter::new(fs.writer(path)?);
                &mut file
            }
            None => out,
        };

        write!(sink, "{}", format.display(self.plain, color))?;
        sink.flush()?;

        // When the output went to a file, report a status line.
        if let Some(path) = &self.output {
            let chunks = format.len();
            let plural = if chunks == 1 { "chunk" } else { "chunks" };
            writeln!(
                err,
                "bulked ingest wrote {} {} to {}",
                chunks,
                plural,
                path.display()
            )?;
        }

        Ok(())
    }

    pub fn handle(self) -> Result<(), super::Error> {
        // When writing to a file, never colorize (it's not a terminal).
        let color = self.output.is_none() && io::stdout().is_terminal();
        self.run(
            &PhysicalFS,
            &mut io::stdin(),
            &mut io::stdout(),
            &mut io::stderr(),
            color,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str, line_number: usize) -> IngestInput {
        IngestInput {
            file_path: PathBuf::from(path),
            line_number,
        }
    }

    fn collect(format: Format, input: &str) -> Result<Vec<IngestInput>, IngestParseError> {
        format.parse(input.as_bytes()).collect()
    }

    #[test]
    fn test_header_matches_tolerates_plural_and_separators() {
        let paths = normalize_header("File_Paths");
        assert!(header_matches(&paths, &FILE_PATH_HEADERS));
        assert!(!header_matches(&paths, &LINE_NUMBER_HEADERS));

        let lines = normalize_header("Line-Nos");
        assert!(header_matches(&lines, &LINE_NUMBER_HEADERS));
        assert!(!header_matches(&lines, &FILE_PATH_HEADERS));

        // Singular forms and mixed separators still match.
        assert!(header_matches(
            &normalize_header("file path"),
            &FILE_PATH_HEADERS
        ));
        assert!(header_matches(
            &normalize_header("LINE_NUMBER"),
            &LINE_NUMBER_HEADERS
        ));
    }

    #[test]
    fn test_header_matches_multibyte_suffix_does_not_panic() {
        let norm = normalize_header("pathé");
        assert!(!header_matches(&norm, &FILE_PATH_HEADERS));
        assert!(!header_matches(&norm, &LINE_NUMBER_HEADERS));
    }

    #[test]
    fn test_parse_csv_multibyte_header_is_missing_headers_not_panic() {
        // The header row ends in a multi-byte character; the old byte-slice
        // normalization panicked here.
        let err = collect(Format::Csv, "pathé,line\nsrc/a.rs,3\n").unwrap_err();
        assert!(matches!(err, IngestParseError::MissingHeaders), "{err:?}");

        // A plural header with a separator still resolves both columns.
        let records = collect(Format::Csv, "Line-Nos,File_Paths\n3,src/a.rs\n").unwrap();
        assert_eq!(records, vec![record("src/a.rs", 3)]);
    }

    #[test]
    fn test_parse_grep_skips_unparseable_lines() {
        let records = collect(Format::Grep, "src/a.rs:12:x\n--\nBinary file b matches\n").unwrap();
        assert_eq!(records, vec![record("src/a.rs", 12)]);
    }

    #[test]
    fn test_parse_grep_accepts_single_digit_line_numbers() {
        let records = collect(Format::Grep, "src/main.rs:6:fn main\nsrc/a.rs:7\n").unwrap();
        assert_eq!(
            records,
            vec![record("src/main.rs", 6), record("src/a.rs", 7)]
        );
    }

    #[test]
    fn test_split_grep_line_keeps_drive_letter_colon_in_path() {
        assert_eq!(split_grep_line("C:\\x:5:foo"), Some(("C:\\x", 5)));
        assert_eq!(split_grep_line("a:b:c"), None);
        assert_eq!(split_grep_line("a:12x:3"), Some(("a:12x", 3)));
    }

    #[test]
    fn test_guess_format() {
        assert_eq!(Format::guess(b"[{"), Format::Json);
        assert_eq!(Format::guess(b"{"), Format::Jsonl);
        assert_eq!(Format::guess(b"a,b"), Format::Csv);
        assert_eq!(Format::guess(b"a:1"), Format::Grep);
    }
}
