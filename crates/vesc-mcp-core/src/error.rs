//! Structured errors for vesc-mcp-core.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("invalid repository root: {path}")]
    InvalidRepoRoot { path: PathBuf },

    #[error("configuration error: {message}")]
    Config { message: String },

    #[error("tool error: {tool}: {message}")]
    Tool { tool: String, message: String },

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type CoreResult<T> = Result<T, CoreError>;

#[must_use]
#[allow(clippy::missing_const_for_fn)] // PathBuf parameter is not const-constructible
pub fn invalid_repo_root(path: PathBuf) -> CoreError {
    CoreError::InvalidRepoRoot { path }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_repo_root_formats_path_in_message() {
        let err = invalid_repo_root(PathBuf::from("/tmp/missing"));
        let message = err.to_string();
        assert!(message.contains("/tmp/missing"));
        assert!(message.contains("invalid repository root"));
    }

    #[test]
    fn tool_error_includes_tool_name() {
        let err = CoreError::Tool {
            tool: "ping".into(),
            message: "unexpected failure".into(),
        };
        assert_eq!(err.to_string(), "tool error: ping: unexpected failure");
    }
}
