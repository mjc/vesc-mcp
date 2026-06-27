//! `validate_package_layout` — verify descriptor assets exist on disk.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vesc_domain::{LayoutIssue, parse_pkgdesc_qml, validate_package_layout};

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

fn locate_pkgdesc(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    const CANDIDATES: [&str; 2] = ["pkgdesc.qml", "package/pkgdesc.qml"];
    for relative in CANDIDATES {
        let path = root.join(relative);
        if path.is_file() {
            let package_root = path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("pkgdesc path has no parent: {}", path.display()))?;
            return Ok((path, package_root));
        }
    }
    Err(format!("no pkgdesc.qml under {}", root.display()))
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
    let root_path = PathBuf::from(root);

    let (pkgdesc_path, package_root) = match locate_pkgdesc(&root_path) {
        Ok(found) => found,
        Err(err) => {
            return ValidatePackageLayoutResponse {
                ok: false,
                issues: Vec::new(),
                error: Some(err),
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
