//! `validate_package_layout` — verify descriptor assets exist on disk.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vesc_domain::{LayoutIssue, parse_pkgdesc_qml, validate_package_layout};
use vesc_mcp_adapters::locate_pkgdesc;

use crate::config::{allowed_package_roots, validate_sandbox_path};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ValidatePackageLayoutParams {
    /// Package root directory containing `pkgdesc.qml` (or `package/pkgdesc.qml`).
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct LayoutIssueJson {
    pub kind: String,
    pub asset: String,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct ValidatePackageLayoutResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<LayoutIssueJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn issue_to_json(issue: &LayoutIssue) -> LayoutIssueJson {
    match issue {
        LayoutIssue::MissingAsset { asset, root } => LayoutIssueJson {
            kind: "missing_asset".into(),
            asset: asset.display().to_string(),
            root: root.display().to_string(),
        },
    }
}

#[must_use]
pub fn validate_package_layout_tool(root: &str) -> ValidatePackageLayoutResponse {
    validate_package_layout_tool_with_sandbox(root, None)
}

#[must_use]
pub fn validate_package_layout_tool_with_sandbox(
    root: &str,
    allowed_roots_override: Option<&[PathBuf]>,
) -> ValidatePackageLayoutResponse {
    let root_path = PathBuf::from(root);
    let allowed_roots = allowed_package_roots(allowed_roots_override);
    if let Err(err) = validate_sandbox_path(&root_path, &allowed_roots) {
        return ValidatePackageLayoutResponse {
            ok: false,
            issues: Vec::new(),
            error: Some(err),
        };
    }

    validate_package_layout_at_root(&root_path)
}

fn validate_package_layout_at_root(root_path: &Path) -> ValidatePackageLayoutResponse {
    let (pkgdesc_path, package_root) = match locate_pkgdesc(root_path) {
        Ok(found) => found,
        Err(err) => {
            return ValidatePackageLayoutResponse {
                ok: false,
                issues: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let content = match fs::read_to_string(&pkgdesc_path) {
        Ok(content) => content,
        Err(err) => {
            return ValidatePackageLayoutResponse {
                ok: false,
                issues: Vec::new(),
                error: Some(format!("read {}: {err}", pkgdesc_path.display())),
            };
        }
    };

    let parsed = match parse_pkgdesc_qml(&content, &pkgdesc_path) {
        Ok(parsed) => parsed,
        Err(err) => {
            return ValidatePackageLayoutResponse {
                ok: false,
                issues: Vec::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let report = validate_package_layout(&package_root, &parsed);
    if report.is_ok() {
        return ValidatePackageLayoutResponse {
            ok: true,
            issues: Vec::new(),
            error: None,
        };
    }

    ValidatePackageLayoutResponse {
        ok: false,
        issues: report.issues.iter().map(issue_to_json).collect(),
        error: None,
    }
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn validate_package_layout_json(params: &ValidatePackageLayoutParams) -> String {
    let response = validate_package_layout_tool(&params.root);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_path;

    #[test]
    fn tool_validate_refloat_fixture_ok() {
        let root = fixture_path("refloat-minimal");
        let response = validate_package_layout_tool(&root.display().to_string());

        assert!(response.ok, "issues: {:?}", response.issues);
        assert!(response.issues.is_empty());
        assert!(response.error.is_none());
    }

    #[test]
    fn tool_validate_broken_fixture_fails() {
        let root = fixture_path("broken-missing-lisp");
        let response = validate_package_layout_tool(&root.display().to_string());

        assert!(!response.ok);
        assert!(response.error.is_none());
        assert_eq!(response.issues.len(), 1);
        assert_eq!(response.issues[0].kind, "missing_asset");
        assert!(response.issues[0].asset.contains("missing-package.lisp"));
    }
}
