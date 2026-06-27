//! Shared helpers for fixture-driven integration tests.

use std::path::{Path, PathBuf};

/// Temporary workspace directory that is removed when dropped.
pub struct TempWorkspace {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl TempWorkspace {
    #[must_use]
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().to_path_buf();
        Self { _temp: temp, root }
    }
}

/// Workspace-root `tests/fixtures/` directory.
#[must_use]
pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Resolve a named fixture directory under [`fixtures_root`].
#[must_use]
pub fn fixture_path(name: &str) -> PathBuf {
    fixtures_root().join(name)
}

/// Read a fixture file relative to a named fixture directory.
///
/// # Panics
///
/// Panics if the fixture file cannot be read.
#[must_use]
pub fn read_fixture_file(fixture: &str, relative: impl AsRef<Path>) -> String {
    let path = fixture_path(fixture).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read fixture file {}: {err}", path.display());
    })
}

/// Return true when a referenced asset path is missing under a package root.
#[must_use]
pub fn asset_missing(root: &Path, relative: &Path) -> bool {
    !root.join(relative).is_file()
}

/// In-process MCP server harness for integration tests.
#[derive(Debug, Clone)]
pub struct McpTestHarness {
    service: crate::VescMcpService,
}

impl McpTestHarness {
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: crate::VescMcpService::new(),
        }
    }

    #[must_use]
    pub fn list_tool_names(&self) -> Vec<String> {
        self.service.list_tool_names()
    }
}

impl Default for McpTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_workspace_creates_empty_directory() {
        let workspace = TempWorkspace::new();
        assert!(workspace.root.is_dir());
        assert!(
            std::fs::read_dir(&workspace.root)
                .expect("read dir")
                .next()
                .is_none()
        );
    }

    #[test]
    fn fixture_path_resolves_refloat_minimal() {
        let path = fixture_path("refloat-minimal");
        assert!(path.join("pkgdesc.qml").is_file(), "{}", path.display());
    }

    #[test]
    fn read_fixture_file_loads_pkgdesc() {
        let content = read_fixture_file("refloat-minimal", "pkgdesc.qml");
        assert!(content.contains("Refloat Minimal"));
    }
}
