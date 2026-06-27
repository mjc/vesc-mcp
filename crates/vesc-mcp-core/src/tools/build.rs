//! `build_vescpkg` — build `.vescpkg` wire artifacts from on-disk package roots.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vesc_mcp_adapters::build_package_from_root;

/// Default build timeout in seconds (applied when subprocess modes land).
pub const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuildVescpkgParams {
    /// Package root directory containing `pkgdesc.qml` (or `package/pkgdesc.qml`).
    pub root: String,
    /// Build backend: `rust` uses the in-tree adapter; `vesc_tool` is not yet implemented.
    pub mode: String,
    /// Maximum seconds to allow for the build (default 120).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

const fn default_timeout_secs() -> u64 {
    DEFAULT_BUILD_TIMEOUT_SECS
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct BuildVescpkgResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[must_use]
pub fn build_vescpkg_tool(params: &BuildVescpkgParams) -> BuildVescpkgResponse {
    if params.mode != "rust" {
        return BuildVescpkgResponse {
            ok: false,
            artifact_path: None,
            sha256: None,
            size_bytes: None,
            error: Some(format!(
                "unsupported build mode {:?}; only \"rust\" is implemented (vesc_tool pending)",
                params.mode
            )),
        };
    }

    let root = PathBuf::from(&params.root);

    match build_package_from_root(&root) {
        Ok(built) => BuildVescpkgResponse {
            ok: true,
            artifact_path: Some(built.artifact_path.display().to_string()),
            sha256: Some(built.sha256),
            size_bytes: Some(built.bytes_len),
            error: None,
        },
        Err(err) => BuildVescpkgResponse {
            ok: false,
            artifact_path: None,
            sha256: None,
            size_bytes: None,
            error: Some(err.to_string()),
        },
    }
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn build_vescpkg_json(params: &BuildVescpkgParams) -> String {
    let response = build_vescpkg_tool(params);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempWorkspace, fixture_path};

    #[test]
    fn tool_build_rust_mode_creates_artifact() {
        let root = fixture_path("poc-native-lib-minimal");
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: root.display().to_string(),
            mode: "rust".into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        assert!(response.ok, "error: {:?}", response.error);
        let artifact_path = response.artifact_path.expect("artifact_path");
        assert!(artifact_path.ends_with("poc-native-lib-minimal.vescpkg"));
        assert!(std::path::Path::new(&artifact_path).is_file());
        assert!(
            response
                .sha256
                .as_ref()
                .is_some_and(|hash| hash.len() == 64)
        );
        assert!(response.size_bytes.is_some_and(|size| size > 0));
    }

    #[test]
    fn tool_build_rust_mode_missing_pkgdesc_fails() {
        let workspace = TempWorkspace::new();
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: workspace.root.display().to_string(),
            mode: "rust".into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_ref()
                .is_some_and(|err| err.contains("pkgdesc"))
        );
    }

    #[test]
    fn tool_build_rust_mode_invalid_layout_fails() {
        let root = fixture_path("broken-missing-lisp");
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: root.display().to_string(),
            mode: "rust".into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        assert!(!response.ok);
        assert!(response.error.is_some());
    }

    #[test]
    fn tool_build_unsupported_mode_fails() {
        let root = fixture_path("poc-native-lib-minimal");
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: root.display().to_string(),
            mode: "vesc_tool".into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_ref()
                .is_some_and(|err| err.contains("vesc_tool"))
        );
    }
}
