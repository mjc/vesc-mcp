//! Domain-level errors with actionable messages for MCP tools.

use std::path::PathBuf;

/// Errors from parsing, validation, and inspection of VESC packages.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("missing pkgdesc property `{property}` in {path}")]
    MissingProperty { property: String, path: PathBuf },

    #[error("invalid pkgdesc property `{property}` in {path}: {message}")]
    InvalidProperty {
        property: String,
        path: PathBuf,
        message: String,
    },

    #[error(
        "unknown pkgdesc dialect in {path}: expected vesc_tool properties starting with `pkgName`"
    )]
    UnknownDialect { path: PathBuf },

    #[error(
        "legacy POC pkgdesc dialect in {path}: use vesc_tool properties (`pkgName`, `pkgLisp`, …) instead of POC-only fields (`packageName`, `nativeLibraryPath`, …)"
    )]
    LegacyPocDialect { path: PathBuf },

    #[error("missing asset `{asset}` relative to package root `{root}`")]
    MissingAsset { asset: PathBuf, root: PathBuf },

    #[error("invalid vescpkg wire format: {message}")]
    InvalidWireFormat { message: String },

    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("legacy buildPkg descriptor parse error: {message}")]
    LegacyDescriptor { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_error_messages_include_path_hints() {
        let err = DomainError::MissingProperty {
            property: "pkgName".to_string(),
            path: PathBuf::from("fixtures/refloat/pkgdesc.qml"),
        };
        let message = err.to_string();
        assert!(message.contains("pkgName"));
        assert!(message.contains("fixtures/refloat/pkgdesc.qml"));
    }

    #[test]
    fn legacy_poc_dialect_error_is_actionable() {
        let err = DomainError::LegacyPocDialect {
            path: PathBuf::from("fixtures/poc/package/pkgdesc.qml"),
        };
        let message = err.to_string();
        assert!(message.contains("legacy POC pkgdesc dialect"));
        assert!(message.contains("pkgName"));
        assert!(message.contains("packageName"));
    }

    #[test]
    fn wire_format_error_is_actionable() {
        let err = DomainError::InvalidWireFormat {
            message: "missing magic header".to_string(),
        };
        assert!(err.to_string().contains("missing magic header"));
    }
}
