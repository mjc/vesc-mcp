//! Inspect `.vescpkg` artifacts via `vesc-domain` wire reader.

use std::path::Path;

use serde::Serialize;
use vesc_domain::{parse_lisp_imports, read_vescpkg_fields};

use crate::error::AdapterError;

/// JSON-friendly inspection summary for MCP tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageInspection {
    pub name: String,
    pub description_md: String,
    pub qml_file: String,
    pub qml_is_fullscreen: bool,
    pub lisp_import_count: usize,
    pub lisp_editor_path: String,
}

/// Read and summarize a `.vescpkg` file.
///
/// # Errors
///
/// Returns [`AdapterError::Domain`] when the wire format is invalid.
pub fn inspect_vescpkg(path: &Path) -> Result<PackageInspection, AdapterError> {
    let fields = read_vescpkg_fields(path)?;
    let (_, imports) = parse_lisp_imports(&fields.lisp_data)?;
    let lisp_editor_path = imports
        .first()
        .map(|import| import.tag.clone())
        .unwrap_or_default();

    Ok(PackageInspection {
        name: fields.name,
        description_md: fields.description_md,
        qml_file: fields.qml_file,
        qml_is_fullscreen: fields.qml_is_fullscreen,
        lisp_import_count: imports.len(),
        lisp_editor_path,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn golden_vescpkg() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/golden/poc-minimal.vescpkg")
    }

    #[test]
    fn adapter_inspect_extracts_package_name() {
        let inspection = inspect_vescpkg(&golden_vescpkg()).expect("inspect");
        assert_eq!(inspection.name, "POC native-lib minimal fixture");
    }

    #[test]
    fn adapter_inspect_reports_lisp_import_count() {
        let inspection = inspect_vescpkg(&golden_vescpkg()).expect("inspect");
        assert!(inspection.lisp_import_count >= 1);
    }
}
