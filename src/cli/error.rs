use thiserror::Error;

/// Root error type for CLI operations
#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Format(#[from] crate::format::types::FormatError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Execute(#[from] crate::execute::ExecuteError),

    #[error(transparent)]
    Ingest(#[from] crate::ingest::IngestError),

    #[error(transparent)]
    Apply(#[from] crate::apply::ApplyError),

    #[error("Failed to apply changes:\n{}", format_apply_errors(.0))]
    ApplyMultiple(Vec<crate::apply::ApplyError>),
}

fn format_apply_errors(errors: &[crate::apply::ApplyError]) -> String {
    errors
        .iter()
        .map(|e| format!("  - {}", e))
        .collect::<Vec<_>>()
        .join("\n")
}
