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

/// Allowed sandbox roots covering all in-repo fixtures (unit tests).
#[must_use]
pub fn fixture_sandbox_roots() -> Vec<PathBuf> {
    vec![fixtures_root()]
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

    /// Call a registered MCP tool and return the JSON text payload.
    ///
    /// Dispatches through the same tool handlers registered on [`crate::VescMcpService`].
    ///
    /// # Panics
    ///
    /// Panics when the tool name is unknown or arguments fail to deserialize.
    #[must_use]
    pub fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        use crate::server::{PingParams, PingResponse, decide_ping_echo};
        use crate::tools::build::{BuildVescpkgParams, build_vescpkg_json};
        use crate::tools::check::{RunPackageChecksParams, run_package_checks_json};
        use crate::tools::inspect::{
            InspectPkgdescParams, InspectVescpkgParams, inspect_pkgdesc_with_sandbox,
            inspect_vescpkg_with_sandbox,
        };
        use crate::tools::list_packages::{ListPackagesParams, list_vesc_packages_json};
        use crate::tools::search_knowledge::{
            SearchVescKnowledgeParams, search_vesc_knowledge_json,
        };
        use crate::tools::validate::{
            ValidatePackageLayoutParams, validate_package_layout_tool_with_sandbox,
        };

        let sandbox = fixture_sandbox_roots();

        assert!(
            self.list_tool_names().iter().any(|tool| tool == name),
            "tool {name} is not registered; have {:?}",
            self.list_tool_names()
        );

        match name {
            "ping" => {
                let params: PingParams = serde_json::from_value(arguments).unwrap_or_default();
                let payload = PingResponse {
                    ok: true,
                    echo: decide_ping_echo(params.message),
                    server: "vesc-mcp".into(),
                };
                serde_json::to_string(&payload).expect("ping response json")
            }
            "list_vesc_packages" => {
                let params: ListPackagesParams =
                    serde_json::from_value(arguments).unwrap_or_default();
                list_vesc_packages_json(&params)
            }
            "inspect_pkgdesc" => {
                let params: InspectPkgdescParams = serde_json::from_value(arguments)
                    .expect("inspect_pkgdesc requires { \"path\": \"...\" }");
                let response = inspect_pkgdesc_with_sandbox(&params.path, Some(&sandbox));
                serde_json::to_string(&response).expect("inspect_pkgdesc response json")
            }
            "inspect_vescpkg" => {
                let params: InspectVescpkgParams = serde_json::from_value(arguments)
                    .expect("inspect_vescpkg requires { \"path\": \"...\" }");
                let response = inspect_vescpkg_with_sandbox(&params.path, Some(&sandbox));
                serde_json::to_string(&response).expect("inspect_vescpkg response json")
            }
            "validate_package_layout" => {
                let params: ValidatePackageLayoutParams = serde_json::from_value(arguments)
                    .expect("validate_package_layout requires { \"root\": \"...\" }");
                let response =
                    validate_package_layout_tool_with_sandbox(&params.root, Some(&sandbox));
                serde_json::to_string(&response).expect("validate_package_layout response json")
            }
            "build_vescpkg" => {
                let params: BuildVescpkgParams = serde_json::from_value(arguments)
                    .expect("build_vescpkg requires { \"root\": \"...\", \"mode\": \"rust\" | \"vesc_tool\" }");
                build_vescpkg_json(&params)
            }
            "run_package_checks" => {
                let params: RunPackageChecksParams = serde_json::from_value(arguments)
                    .expect("run_package_checks requires { \"root\": \"...\" }");
                run_package_checks_json(&params)
            }
            "search_vesc_knowledge" => {
                let params: SearchVescKnowledgeParams = serde_json::from_value(arguments)
                    .expect("search_vesc_knowledge requires { \"query\": \"...\" }");
                search_vesc_knowledge_json(&params)
            }
            other => panic!("missing harness dispatch for registered tool: {other}"),
        }
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
