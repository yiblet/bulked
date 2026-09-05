use miette::Diagnostic;
use thiserror::Error;

/// Root error type for CLI operations
///
/// Only the `Format` variant carries miette diagnostic metadata (source spans,
/// labels, help); it is marked transparent so the report rendered by `main.rs`
/// shows the underlying `FormatError` diagnostic. All other variants render
/// with their plain `Display` text.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Format(#[from] crate::format::types::FormatError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Csv(#[from] csv::Error),

    #[error("csv does not contain the right headers. It must be at least path,line_number")]
    CsvMissingHeaders,

    #[error("csv contains missing fields for {0}")]
    CsvMissingFields(&'static str),

    #[error("csv coould not parse {0}")]
    CsvCouldNotParse(&'static str),

    #[error(transparent)]
    Execute(#[from] crate::execute::ExecuteError),

    #[error(transparent)]
    Ingest(#[from] crate::ingest::IngestError),

    #[error(transparent)]
    Apply(#[from] crate::apply::ApplyErrors),
}

#[cfg(test)]
mod tests {
    use super::Error;
    use crate::format::types::Format;
    use miette::Diagnostic;

    #[test]
    fn test_cli_error_exposes_format_diagnostic_labels() {
        let input = "@f.txt:abc:1\nZ\n@@@\n";
        let format_err = input
            .parse::<Format>()
            .expect_err("non-numeric line number must fail to parse");
        let err = Error::from(format_err);

        let labels: Vec<_> = err
            .labels()
            .expect("Format variant must expose the underlying labels")
            .collect();
        assert_eq!(labels.len(), 1, "expected exactly one label");
        let expected_offset = input.find("abc").unwrap();
        assert_eq!(labels[0].offset(), expected_offset);
        assert_eq!(labels[0].len(), "abc".len());
        assert_eq!(labels[0].label(), Some("Expected a number here"));

        let code = err
            .code()
            .expect("Format variant must expose the underlying code");
        assert_eq!(code.to_string(), "format::invalid_line_number");
    }

    #[test]
    fn test_cli_error_non_format_variant_has_no_labels() {
        let err = Error::CsvMissingHeaders;
        assert!(err.labels().is_none());
        assert!(err.code().is_none());
    }
}
