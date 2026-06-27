//! Adapter error types.

use std::path::PathBuf;

use thiserror::Error;
use vesc_domain::DomainError;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AdapterError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("package layout invalid under {root}")]
    LayoutInvalid { root: PathBuf },

    #[error("{message}")]
    Message { message: String },
}

impl AdapterError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
        }
    }
}
