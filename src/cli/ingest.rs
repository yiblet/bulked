use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    str::FromStr,
};

use clap::{Args, ValueEnum};

#[derive(Debug, Clone, Default)]
enum Format {
    #[default]
    Jsonl,
    Json,
    Grep,
}

impl ValueEnum for Format {
    fn value_variants<'a>() -> &'a [Self] {
        &[Format::Jsonl, Format::Json, Format::Grep]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::Jsonl => clap::builder::PossibleValue::new("jsonl").help("parse as a jsonl"),
            Self::Json => clap::builder::PossibleValue::new("json").help("parse as a json"),
            Self::Grep => clap::builder::PossibleValue::new("grep").help("parse as a grep output"),
        })
    }
}

pub enum EitherIter<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> Iterator for EitherIter<L, R>
where
    L: Iterator,
    R: Iterator<Item = L::Item>,
{
    type Item = L::Item;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Left(l) => l.next(),
            Self::Right(r) => r.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Left(l) => l.size_hint(),
            Self::Right(r) => r.size_hint(),
        }
    }
}

impl Format {
    fn parse_jsonl(
        r: &mut dyn std::io::Read,
    ) -> impl Iterator<Item = Result<IngestRecord, String>> {
        BufReader::new(r).lines().map(string_result).map(|r| {
            let line = r?;
            let record: Result<IngestRecord, _> = string_result(serde_json::from_str(&line));
            record
        })
    }

    fn parse_json(r: &mut dyn std::io::Read) -> impl Iterator<Item = Result<IngestRecord, String>> {
        let mut content = String::new();
        let res: Result<Vec<IngestRecord>, _> = string_result(r.read_to_string(&mut content))
            .map(move |_| content)
            .and_then(|content| string_result(serde_json::from_str(&content)));

        match res {
            Ok(v) => EitherIter::Left(v.into_iter().map(Ok)),
            Err(v) => EitherIter::Right(std::iter::once(Err(v))),
        }
    }

    fn parse_grep(
        r: &mut dyn ::std::io::Read,
    ) -> impl Iterator<Item = Result<IngestRecord, String>> {
        BufReader::new(r)
            .lines()
            .map(string_result)
            .filter_map(|r| {
                r.map(|line| {
                    let line_no_split = line
                        .split_inclusive(':')
                        .scan(0usize, |bytes, c| {
                            let prev = *bytes;
                            *bytes += c.len();
                            Some((prev, c))
                        })
                        .find_map(|(pos, st)| match st.chars().nth(1)? {
                            '0'..='9' => Some(pos),
                            _ => None,
                        })?;

                    let file = &line[..line_no_split - 1];
                    let nums = line[line_no_split..]
                        .split_once(|c: char| !c.is_numeric())?
                        .0;

                    let line_no: usize = nums.parse().ok()?;
                    let file = PathBuf::from_str(file).ok()?;
                    Some(IngestRecord {
                        path: file,
                        line: line_no,
                    })
                })
                .transpose()
            })
    }

    pub fn parse(
        &self,
        r: &mut dyn ::std::io::Read,
    ) -> impl Iterator<Item = Result<IngestRecord, String>> {
        match self {
            Self::Json => EitherIter::Left(EitherIter::Left(Self::parse_json(r))),
            Self::Jsonl => EitherIter::Left(EitherIter::Right(Self::parse_jsonl(r))),
            Self::Grep => EitherIter::Right(Self::parse_grep(r)),
        }
    }
}

#[derive(Args, Debug)]
pub(super) struct IngestArgs {
    /// Directory or file to search (default: current directory)
    /// use '-' to read from stdin
    #[arg(default_value = None)]
    path: Option<PathBuf>,

    #[arg(short, long = "format", default_value = "jsonl")]
    format: Format,

    /// Lines of context before and after each match
    #[arg(short = 'C', long, default_value = "20")]
    context: usize,

    /// output as plain text (human-readable format)
    #[arg(long)]
    plain: bool,
}

#[derive(Debug, serde::Deserialize)]
struct IngestRecord {
    path: PathBuf,
    line: usize,
}

impl From<IngestRecord> for crate::types::IngestInput {
    fn from(value: IngestRecord) -> Self {
        Self {
            file_path: value.path,
            line_number: value.line,
        }
    }
}

fn string_result<S, E: std::error::Error>(res: Result<S, E>) -> Result<S, String> {
    res.map_err(|e| e.to_string())
}

impl IngestArgs {
    fn get_inputs(&self) -> Result<Vec<crate::types::IngestInput>, String> {
        let mut stdin;
        let mut file;

        let stream: &mut dyn std::io::Read = match &self.path {
            Some(buf) if buf.as_os_str() != "-" => {
                file = std::fs::File::open(buf).map_err(|e| e.to_string())?;
                &mut file
            }
            _ => {
                stdin = std::io::stdin();
                &mut stdin
            }
        };

        self.format
            .parse(stream)
            .map(|r| r.map(|i| i.into()))
            .collect()
    }

    pub fn handle(self) -> Result<(), String> {
        let is_tty = atty::is(atty::Stream::Stdout);
        let inputs = self.get_inputs()?;

        let result = string_result(crate::ingest::ingest(
            &crate::filesystem::physical::PhysicalFS,
            inputs,
            self.context,
        ))?;

        let format = crate::format::Format::from_matches(&result);
        print!("{}", format.display(self.plain, is_tty));

        Ok(())
    }
}
