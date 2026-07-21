//! `list_vesc_packages` — discover pkgdesc.qml under configured roots.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vesc_domain::{PkgDescDialect, parse_pkgdesc_qml};

use crate::config::{McpConfig, resolve_package_roots};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ListPackagesParams {
    /// Package roots to scan. Defaults to client roots plus `VESC_PACKAGE_ROOTS`.
    #[serde(default)]
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct PackageEntry {
    /// Package directory containing the descriptor and assets.
    pub root: String,
    pub pkgdesc_path: String,
    pub dialect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct ListPackagesResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<PackageEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Resolve search roots from explicit params or loaded MCP config.
#[must_use]
pub fn resolve_roots(roots: &[String]) -> Vec<PathBuf> {
    resolve_package_roots(roots, McpConfig::load())
}

/// Resolve explicit roots, or configured plus client-provided project roots.
#[must_use]
pub fn resolve_roots_with_client_roots(roots: &[String], client_roots: &[PathBuf]) -> Vec<PathBuf> {
    if !roots.is_empty() {
        return roots.iter().map(PathBuf::from).collect();
    }
    let mut resolved = resolve_roots(roots);
    resolved.extend(client_roots.iter().cloned());
    resolved
}

#[must_use]
pub const fn dialect_label(dialect: PkgDescDialect) -> &'static str {
    match dialect {
        PkgDescDialect::VescTool => "vesc_tool",
    }
}

/// Walk roots and collect pkgdesc entries.
#[must_use]
pub fn list_vesc_packages(roots: &[String]) -> ListPackagesResponse {
    list_vesc_packages_from_roots(&resolve_roots(roots))
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn list_vesc_packages_json(params: &ListPackagesParams) -> String {
    let response = list_vesc_packages(&params.roots);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

/// Serialize package discovery using MCP client roots as an additional default.
#[must_use]
pub fn list_vesc_packages_json_with_client_roots(
    params: &ListPackagesParams,
    client_roots: &[PathBuf],
) -> String {
    let response = list_vesc_packages_from_roots(&resolve_roots_with_client_roots(
        &params.roots,
        client_roots,
    ));
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

fn list_vesc_packages_from_roots(search_roots: &[PathBuf]) -> ListPackagesResponse {
    let mut packages = Vec::new();
    for root in search_roots {
        if root.is_dir() {
            walk_for_pkgdesc(root, &mut packages);
        }
    }
    packages.sort_by(|left, right| left.pkgdesc_path.cmp(&right.pkgdesc_path));
    ListPackagesResponse {
        ok: true,
        packages,
        error: None,
    }
}

fn walk_for_pkgdesc(dir: &Path, out: &mut Vec<PackageEntry>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            walk_for_pkgdesc(&path, out);
        } else if path.file_name().is_some_and(|name| name == "pkgdesc.qml")
            && let Some(package) = package_entry_from_pkgdesc(&path)
        {
            out.push(package);
        }
    }
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.starts_with('.') || name == "target" || name == "node_modules"
    })
}

fn package_entry_from_pkgdesc(pkgdesc_path: &Path) -> Option<PackageEntry> {
    let content = fs::read_to_string(pkgdesc_path).ok()?;
    let parsed = parse_pkgdesc_qml(&content, pkgdesc_path).ok()?;
    let dialect = parsed.dialect();
    let package_root = package_root_for_pkgdesc(pkgdesc_path)?;

    Some(PackageEntry {
        root: package_root.display().to_string(),
        pkgdesc_path: pkgdesc_path.display().to_string(),
        dialect: dialect_label(dialect).into(),
    })
}

fn package_root_for_pkgdesc(pkgdesc_path: &Path) -> Option<PathBuf> {
    let parent = pkgdesc_path.parent()?;
    if parent.file_name().is_some_and(|name| name == "package") {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_path;

    #[test]
    fn resolve_roots_prefers_explicit_paths() {
        let roots = resolve_roots(&["/tmp/a".into(), "/tmp/b".into()]);
        assert_eq!(
            roots,
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
    }

    #[test]
    fn list_finds_refloat_minimal_fixture() {
        let root = fixture_path("refloat-minimal");
        let response = list_vesc_packages(&[root.display().to_string()]);

        assert!(response.ok);
        assert_eq!(response.packages.len(), 1);
        let entry = &response.packages[0];
        assert_eq!(entry.dialect, "vesc_tool");
        assert!(entry.pkgdesc_path.ends_with("pkgdesc.qml"));
        assert!(entry.root.ends_with("refloat-minimal"));
    }
}
