use miette::Diagnostic;
use thiserror::Error;

/// Root error type for CLI operations
///
/// The `Format`, `IngestParse`, `Apply` and `Refresh` variants carry miette
/// diagnostic metadata (source spans, labels, help); they are marked transparent
/// so the report rendered by `main.rs` shows the underlying diagnostic. All other
/// variants render with their plain `Display` text.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Filesystem(#[from] crate::filesystem::FilesystemError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Format(#[from] crate::format::types::FormatError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    IngestParse(#[from] super::ingest::IngestParseError),

    #[error(transparent)]
    Execute(#[from] crate::execute::ExecuteError),

    #[error(transparent)]
    Ingest(#[from] crate::ingest::IngestError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Apply(#[from] crate::apply::ApplyErrors),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Refresh(#[from] crate::refresh::RefreshError),
}

#[cfg(test)]
mod tests {
    use super::Error;
    use crate::cli::ingest::IngestParseError;
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
    fn test_cli_error_plain_variant_has_no_labels() {
        let err = Error::from(std::io::Error::other("boom"));
        assert!(err.labels().is_none());
        assert!(err.code().is_none());
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn test_cli_error_exposes_ingest_help() {
        let err = Error::from(IngestParseError::NoLocations {
            lines: 3,
            first: "x".to_string(),
            help: Some("add -n".to_string()),
        });
        assert_eq!(
            err.help().map(|h| h.to_string()),
            Some("add -n".to_string())
        );
        assert_eq!(err.code().unwrap().to_string(), "ingest::no_locations");
    }
}
