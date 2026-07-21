//! Cross-check descriptor fields against the filesystem.

use std::path::{Path, PathBuf};

use crate::error::DomainError;
use crate::pkgdesc::{ParsedPkgDesc, PkgDescVescTool, RelativeAssetPath};

/// One layout validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutIssue {
    MissingAsset { asset: PathBuf, root: PathBuf },
}

impl LayoutIssue {
    #[must_use]
    pub fn to_domain_error(&self) -> DomainError {
        match self {
            Self::MissingAsset { asset, root } => DomainError::MissingAsset {
                asset: asset.clone(),
                root: root.clone(),
            },
        }
    }
}

/// Aggregated layout validation result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayoutValidationReport {
    pub issues: Vec<LayoutIssue>,
}

impl LayoutValidationReport {
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn errors(&self) -> impl Iterator<Item = DomainError> + '_ {
        self.issues.iter().map(LayoutIssue::to_domain_error)
    }
}

/// Verify that all assets referenced by `desc` exist under `root`.
#[must_use]
pub fn validate_package_layout(root: &Path, desc: &ParsedPkgDesc) -> LayoutValidationReport {
    let mut issues = Vec::new();
    match desc {
        ParsedPkgDesc::VescTool(vesc_tool) => {
            validate_vesc_tool_layout(root, vesc_tool, &mut issues);
        }
    }
    LayoutValidationReport { issues }
}

fn validate_vesc_tool_layout(root: &Path, desc: &PkgDescVescTool, issues: &mut Vec<LayoutIssue>) {
    check_asset(root, &desc.description_md_path, issues);
    check_asset(root, &desc.lisp_path, issues);
    if !desc.qml_path.as_path().as_os_str().is_empty() {
        check_asset(root, &desc.qml_path, issues);
    }
}

fn check_asset(root: &Path, asset: &RelativeAssetPath, issues: &mut Vec<LayoutIssue>) {
    let path = root.join(asset.as_path());
    if !path.is_file() {
        issues.push(LayoutIssue::MissingAsset {
            asset: asset.as_path().to_path_buf(),
            root: root.to_path_buf(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::pkgdesc::parse_pkgdesc_qml;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    #[test]
    fn validate_refloat_layout_ok() {
        let root = fixtures_root().join("refloat-minimal");
        let content = std::fs::read_to_string(root.join("pkgdesc.qml")).expect("read pkgdesc");
        let desc = parse_pkgdesc_qml(&content, root.join("pkgdesc.qml")).expect("parse");
        let report = validate_package_layout(&root, &desc);
        assert!(report.is_ok(), "issues: {:?}", report.issues);
    }

    #[test]
    fn validate_missing_lisp_fails() {
        let root = fixtures_root().join("broken-missing-lisp");
        let content = std::fs::read_to_string(root.join("pkgdesc.qml")).expect("read pkgdesc");
        let desc = parse_pkgdesc_qml(&content, root.join("pkgdesc.qml")).expect("parse");
        let report = validate_package_layout(&root, &desc);
        assert!(!report.is_ok());
        assert_eq!(report.issues.len(), 1);
        assert!(matches!(report.issues[0], LayoutIssue::MissingAsset { .. }));
        let err = report.errors().next().expect("error");
        assert!(matches!(err, DomainError::MissingAsset { .. }));
    }
}
