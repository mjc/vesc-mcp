//! `inspect_pkgdesc` / `inspect_vescpkg` — parse package descriptors and wire artifacts.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vesc_domain::{ParsedPkgDesc, parse_pkgdesc_qml};
use vesc_mcp_adapters::{PackageInspection, inspect_vescpkg as adapter_inspect_vescpkg};

use crate::config::{allowed_package_roots, validate_sandbox_file};

use super::list_packages::dialect_label;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InspectPkgdescParams {
    /// Absolute or relative path to a `pkgdesc.qml` file.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct ParsedPkgdescJson {
    pub pkg_name: String,
    pub description_md_path: String,
    pub lisp_path: String,
    pub qml_path: String,
    pub output_name: String,
    pub qml_is_fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct InspectPkgdescResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<ParsedPkgdescJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[must_use]
pub fn inspect_pkgdesc(path: &str) -> InspectPkgdescResponse {
    inspect_pkgdesc_with_sandbox(path, None)
}

#[must_use]
pub fn inspect_pkgdesc_with_sandbox(
    path: &str,
    allowed_roots_override: Option<&[PathBuf]>,
) -> InspectPkgdescResponse {
    let path_buf = PathBuf::from(path);
    let allowed_roots = allowed_package_roots(allowed_roots_override);
    if let Err(err) = validate_sandbox_file(&path_buf, &allowed_roots) {
        return InspectPkgdescResponse {
            ok: false,
            dialect: None,
            parsed: None,
            error: Some(err),
        };
    }

    inspect_pkgdesc_at_path(&path_buf)
}

fn inspect_pkgdesc_at_path(path_buf: &Path) -> InspectPkgdescResponse {
    let content = match fs::read_to_string(path_buf) {
        Ok(content) => content,
        Err(err) => {
            return InspectPkgdescResponse {
                ok: false,
                dialect: None,
                parsed: None,
                error: Some(format!("read {}: {err}", path_buf.display())),
            };
        }
    };

    match parse_pkgdesc_qml(&content, path_buf) {
        Ok(parsed) => parsed_to_response(&parsed),
        Err(err) => InspectPkgdescResponse {
            ok: false,
            dialect: None,
            parsed: None,
            error: Some(err.to_string()),
        },
    }
}

fn parsed_to_response(parsed: &ParsedPkgDesc) -> InspectPkgdescResponse {
    let (dialect, parsed_json) = pkgdesc_to_json(parsed);

    InspectPkgdescResponse {
        ok: true,
        dialect: Some(dialect),
        parsed: Some(parsed_json),
        error: None,
    }
}

/// Shared pkgdesc JSON projection for `inspect_pkgdesc` and manifest resources.
#[must_use]
pub fn pkgdesc_to_json(parsed: &ParsedPkgDesc) -> (String, ParsedPkgdescJson) {
    let dialect = dialect_label(parsed.dialect()).into();
    let parsed_json = match parsed {
        ParsedPkgDesc::VescTool(desc) => ParsedPkgdescJson {
            pkg_name: desc.pkg_name.as_str().into(),
            description_md_path: desc.description_md_path.as_path().display().to_string(),
            lisp_path: desc.lisp_path.as_path().display().to_string(),
            qml_path: desc.qml_path.as_path().display().to_string(),
            output_name: desc.output_name.as_str().into(),
            qml_is_fullscreen: desc.qml_is_fullscreen,
        },
    };
    (dialect, parsed_json)
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn inspect_pkgdesc_json(params: &InspectPkgdescParams) -> String {
    let response = inspect_pkgdesc(&params.path);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InspectVescpkgParams {
    /// Absolute or relative path to a `.vescpkg` file.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct VescpkgInspectionJson {
    pub name: String,
    pub description_md: String,
    pub qml_file: String,
    pub qml_is_fullscreen: bool,
    pub lisp_import_count: usize,
    pub lisp_editor_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct InspectVescpkgResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspection: Option<VescpkgInspectionJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[must_use]
pub fn inspect_vescpkg(path: &str) -> InspectVescpkgResponse {
    inspect_vescpkg_with_sandbox(path, None)
}

#[must_use]
pub fn inspect_vescpkg_with_sandbox(
    path: &str,
    allowed_roots_override: Option<&[PathBuf]>,
) -> InspectVescpkgResponse {
    let path_buf = PathBuf::from(path);
    let allowed_roots = allowed_package_roots(allowed_roots_override);
    if let Err(err) = validate_sandbox_file(&path_buf, &allowed_roots) {
        return InspectVescpkgResponse {
            ok: false,
            inspection: None,
            error: Some(err),
        };
    }

    match adapter_inspect_vescpkg(&path_buf) {
        Ok(inspection) => InspectVescpkgResponse {
            ok: true,
            inspection: Some(inspection.into()),
            error: None,
        },
        Err(err) => InspectVescpkgResponse {
            ok: false,
            inspection: None,
            error: Some(err.to_string()),
        },
    }
}

impl From<PackageInspection> for VescpkgInspectionJson {
    fn from(inspection: PackageInspection) -> Self {
        Self {
            name: inspection.name,
            description_md: inspection.description_md,
            qml_file: inspection.qml_file,
            qml_is_fullscreen: inspection.qml_is_fullscreen,
            lisp_import_count: inspection.lisp_import_count,
            lisp_editor_path: inspection.lisp_editor_path,
        }
    }
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn inspect_vescpkg_json(params: &InspectVescpkgParams) -> String {
    let response = inspect_vescpkg(&params.path);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture_path, fixture_sandbox_roots};

    #[test]
    fn pkgdesc_to_json_matches_refloat_fixture() {
        let path = fixture_path("refloat-minimal").join("pkgdesc.qml");
        let content = std::fs::read_to_string(&path).expect("read pkgdesc");
        let parsed = parse_pkgdesc_qml(&content, &path).expect("parse");
        let (dialect, json) = pkgdesc_to_json(&parsed);
        assert_eq!(dialect, "vesc_tool");
        assert_eq!(json.pkg_name, "Refloat Minimal");
        assert_eq!(json.output_name, "refloat-minimal.vescpkg");
    }

    #[test]
    fn inspect_refloat_minimal_fixture() {
        let path = fixture_path("refloat-minimal").join("pkgdesc.qml");
        let response = inspect_pkgdesc(&path.display().to_string());

        assert!(response.ok);
        assert_eq!(response.dialect.as_deref(), Some("vesc_tool"));
        let parsed = response.parsed.expect("parsed fields");
        assert_eq!(parsed.pkg_name, "Refloat Minimal");
        assert_eq!(parsed.output_name, "refloat-minimal.vescpkg");
    }

    #[test]
    fn inspect_poc_native_fixture() {
        let path = fixture_path("poc-native-lib-minimal/package/pkgdesc.qml");
        let response = inspect_pkgdesc(&path.display().to_string());

        assert!(response.ok);
        assert_eq!(response.dialect.as_deref(), Some("vesc_tool"));
        let parsed = response.parsed.expect("parsed fields");
        assert_eq!(parsed.pkg_name, "POC native-lib minimal fixture");
        assert_eq!(parsed.output_name, "poc-native-lib-minimal.vescpkg");
        assert_eq!(parsed.qml_path, "");
    }

    #[test]
    fn inspect_missing_file_returns_error() {
        let response = inspect_pkgdesc_with_sandbox(
            "/nonexistent/pkgdesc.qml",
            Some(&fixture_sandbox_roots()),
        );
        assert!(!response.ok);
        assert!(response.error.is_some());
    }

    #[test]
    fn inspect_vescpkg_golden_reads_name_and_imports() {
        let path = fixture_path("golden/poc-minimal.vescpkg");
        let response = inspect_vescpkg(&path.display().to_string());

        assert!(response.ok);
        let inspection = response.inspection.expect("inspection fields");
        assert_eq!(inspection.name, "POC native-lib minimal fixture");
        assert_eq!(inspection.lisp_import_count, 1);
        assert_eq!(inspection.lisp_editor_path, "package-lib");
    }

    #[test]
    fn inspect_vescpkg_missing_file_returns_error() {
        let response = inspect_vescpkg_with_sandbox(
            "/nonexistent/package.vescpkg",
            Some(&fixture_sandbox_roots()),
        );
        assert!(!response.ok);
        assert!(response.error.is_some());
    }
}
