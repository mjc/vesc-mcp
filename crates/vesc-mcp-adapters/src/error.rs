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

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use vesc_domain::DomainError;

    use super::*;

    #[test]
    fn display_message_error() {
        let err = AdapterError::message("no pkgdesc");
        assert_eq!(err.to_string(), "no pkgdesc");
    }

    #[test]
    fn display_layout_invalid() {
        let root = PathBuf::from("/tmp/pkg");
        let display_root = root.display().to_string();
        let err = AdapterError::LayoutInvalid { root };
        assert!(err.to_string().contains(&display_root));
    }

    #[test]
    fn display_io_error_includes_path() {
        let err = AdapterError::Io {
            path: PathBuf::from("/x/y"),
            source: io::Error::new(io::ErrorKind::NotFound, "nope"),
        };
        assert!(err.to_string().contains("/x/y"));
    }

    #[test]
    fn from_domain_error() {
        let domain = DomainError::InvalidWireFormat {
            message: "bad".into(),
        };
        let err: AdapterError = domain.into();
        assert!(matches!(err, AdapterError::Domain(_)));
    }
}
