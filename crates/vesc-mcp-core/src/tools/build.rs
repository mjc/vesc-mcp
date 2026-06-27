//! `build_vescpkg` — build `.vescpkg` wire artifacts via `vesc_tool`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vesc_domain::{ParsedPkgDesc, parse_pkgdesc_qml, validate_package_layout};
use vesc_mcp_adapters::locate_pkgdesc;

use crate::config::{McpConfig, allowed_package_roots, validate_sandbox_path};
use crate::tools::tool_error::{
    ToolError, tool_error_from_adapter, tool_error_from_domain, tool_error_from_sandbox,
    tool_error_from_vesc_tool,
};

/// Default build timeout in seconds.
pub const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuildVescpkgParams {
    /// Package root directory containing `pkgdesc.qml` (or `package/pkgdesc.qml`).
    pub root: String,
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
    pub error: Option<ToolError>,
}

/// Subprocess backend for `vesc_tool --buildPkgFromDesc` (mockable in unit tests).
pub trait VescToolRunner {
    /// Run `vesc_tool --buildPkgFromDesc <pkgdesc_file_name>` with `package_root` as cwd.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error when spawn, timeout, or non-zero exit occurs.
    fn build_pkg_from_desc(
        &self,
        vesc_tool: &Path,
        package_root: &Path,
        pkgdesc_file_name: &str,
        timeout_secs: u64,
    ) -> Result<(), String>;
}

/// Production runner that spawns the real `vesc_tool` CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealVescToolRunner;

impl VescToolRunner for RealVescToolRunner {
    fn build_pkg_from_desc(
        &self,
        vesc_tool: &Path,
        package_root: &Path,
        pkgdesc_file_name: &str,
        timeout_secs: u64,
    ) -> Result<(), String> {
        let mut child = Command::new(vesc_tool)
            .current_dir(package_root)
            .arg("--buildPkgFromDesc")
            .arg(pkgdesc_file_name)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("spawn {}: {err}", vesc_tool.display()))?;

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            match child
                .try_wait()
                .map_err(|err| format!("wait on {}: {err}", vesc_tool.display()))?
            {
                Some(status) => {
                    if status.success() {
                        return Ok(());
                    }
                    let stderr = child
                        .stderr
                        .take()
                        .and_then(|mut pipe| {
                            let mut buf = Vec::new();
                            std::io::Read::read_to_end(&mut pipe, &mut buf).ok()?;
                            Some(String::from_utf8_lossy(&buf).into_owned())
                        })
                        .unwrap_or_default();
                    return Err(format!(
                        "vesc_tool exited with {status}{}",
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!(": {stderr}")
                        }
                    ));
                }
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "vesc_tool timed out after {timeout_secs}s (root {})",
                        package_root.display()
                    ));
                }
                None => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[must_use]
pub fn build_vescpkg_tool(params: &BuildVescpkgParams) -> BuildVescpkgResponse {
    build_vescpkg_tool_with_runner(params, &RealVescToolRunner, None, None)
}

#[must_use]
pub fn build_vescpkg_tool_with_runner(
    params: &BuildVescpkgParams,
    runner: &dyn VescToolRunner,
    vesc_tool_override: Option<&Path>,
    allowed_roots_override: Option<&[PathBuf]>,
) -> BuildVescpkgResponse {
    let root_path = PathBuf::from(&params.root);
    let allowed_roots = allowed_package_roots(allowed_roots_override);
    if let Err(err) = validate_sandbox_path(&root_path, &allowed_roots) {
        return build_error(tool_error_from_sandbox(err));
    }

    build_vescpkg_vesc_tool(params, runner, vesc_tool_override)
}

#[allow(clippy::missing_const_for_fn)]
fn build_error(error: ToolError) -> BuildVescpkgResponse {
    BuildVescpkgResponse {
        ok: false,
        artifact_path: None,
        sha256: None,
        size_bytes: None,
        error: Some(error),
    }
}

fn build_vescpkg_vesc_tool(
    params: &BuildVescpkgParams,
    runner: &dyn VescToolRunner,
    vesc_tool_override: Option<&Path>,
) -> BuildVescpkgResponse {
    let root = PathBuf::from(&params.root);
    let (pkgdesc_path, package_root) = match locate_pkgdesc(&root) {
        Ok(found) => found,
        Err(err) => return build_error(tool_error_from_adapter(err)),
    };
    let (output_name, pkgdesc_file_name) =
        match vesc_tool_pkgdesc_context(&pkgdesc_path, &package_root) {
            Ok(context) => context,
            Err(err) => return build_error(err),
        };
    let vesc_tool = vesc_tool_override.map_or_else(
        || McpConfig::load().vesc_tool_path.clone(),
        Path::to_path_buf,
    );
    if let Err(err) = runner.build_pkg_from_desc(
        &vesc_tool,
        &package_root,
        &pkgdesc_file_name,
        params.timeout_secs,
    ) {
        return build_error(tool_error_from_vesc_tool(err, &root));
    }

    let artifact_path = resolve_vesc_tool_artifact_path(&root, &package_root, &output_name);

    let Some(artifact_path) = artifact_path else {
        return build_error(tool_error_from_vesc_tool(
            format!(
                "vesc_tool finished but artifact {output_name} not found under {}",
                root.display()
            ),
            &root,
        ));
    };

    match artifact_metadata(&artifact_path) {
        Ok((sha256, size_bytes)) => BuildVescpkgResponse {
            ok: true,
            artifact_path: Some(artifact_path.display().to_string()),
            sha256: Some(sha256),
            size_bytes: Some(size_bytes),
            error: None,
        },
        Err(err) => build_error(err),
    }
}

/// Locate the `.vescpkg` written by `vesc_tool --buildPkgFromDesc`.
fn resolve_vesc_tool_artifact_path(
    root: &Path,
    package_root: &Path,
    output_name: &str,
) -> Option<PathBuf> {
    [root.join(output_name), package_root.join(output_name)]
        .into_iter()
        .find(|path| path.is_file())
}

fn vesc_tool_pkgdesc_context(
    pkgdesc_path: &Path,
    package_root: &Path,
) -> Result<(String, String), ToolError> {
    let pkgdesc_src = std::fs::read_to_string(pkgdesc_path).map_err(|source| {
        ToolError::new(
            "IO_ERROR",
            format!("read {}: {source}", pkgdesc_path.display()),
        )
        .with_path(pkgdesc_path.display().to_string())
        .with_hint("ensure pkgdesc.qml exists and is readable")
    })?;
    let parsed = parse_pkgdesc_qml(&pkgdesc_src, pkgdesc_path).map_err(tool_error_from_domain)?;
    let report = validate_package_layout(package_root, &parsed);
    if !report.is_ok() {
        return Err(tool_error_from_adapter(
            vesc_mcp_adapters::AdapterError::LayoutInvalid {
                root: package_root.to_path_buf(),
            },
        ));
    }
    let ParsedPkgDesc::VescTool(desc) = parsed;
    let pkgdesc_file_name = pkgdesc_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            ToolError::new(
                "BUILD_FAILED",
                format!("pkgdesc path has no file name: {}", pkgdesc_path.display()),
            )
            .with_path(pkgdesc_path.display().to_string())
        })?;
    Ok((desc.output_name.as_str().to_owned(), pkgdesc_file_name))
}

fn artifact_metadata(path: &Path) -> Result<(String, usize), ToolError> {
    let bytes = std::fs::read(path).map_err(|source| {
        ToolError::new("IO_ERROR", format!("read {}: {source}", path.display()))
            .with_path(path.display().to_string())
            .with_hint("ensure the build artifact exists and is readable")
    })?;
    Ok((sha256_hex(&bytes), bytes.len()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn build_vescpkg_json(params: &BuildVescpkgParams) -> String {
    let response = build_vescpkg_tool(params);
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"ok":false,"error":{"code":"SERIALIZATION_FAILED","message":"serialization failed"}}"#
            .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempWorkspace, fixture_path, fixture_sandbox_roots};

    struct MockVescToolRunner {
        artifact_bytes: Vec<u8>,
    }

    struct SpyVescToolRunner {
        called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl VescToolRunner for SpyVescToolRunner {
        fn build_pkg_from_desc(
            &self,
            _vesc_tool: &Path,
            _package_root: &Path,
            _pkgdesc_file_name: &str,
            _timeout_secs: u64,
        ) -> Result<(), String> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    impl VescToolRunner for MockVescToolRunner {
        fn build_pkg_from_desc(
            &self,
            _vesc_tool: &Path,
            package_root: &Path,
            _pkgdesc_file_name: &str,
            _timeout_secs: u64,
        ) -> Result<(), String> {
            let artifact_path = package_root.join("refloat-minimal.vescpkg");
            std::fs::write(&artifact_path, &self.artifact_bytes)
                .map_err(|err| format!("mock write {}: {err}", artifact_path.display()))
        }
    }

    #[test]
    fn tool_build_vesc_tool_mocked() {
        let root = fixture_path("refloat-minimal");
        let artifact_bytes = b"mock-vescpkg-bytes".to_vec();
        let expected_hash = sha256_hex(&artifact_bytes);
        let runner = MockVescToolRunner { artifact_bytes };

        let response = build_vescpkg_tool_with_runner(
            &BuildVescpkgParams {
                root: root.display().to_string(),
                timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
            },
            &runner,
            Some(Path::new("/mock/vesc_tool")),
            Some(&fixture_sandbox_roots()),
        );

        assert!(response.ok, "error: {:?}", response.error);
        let artifact_path = response.artifact_path.expect("artifact_path");
        assert!(artifact_path.ends_with("refloat-minimal.vescpkg"));
        assert_eq!(response.sha256.as_deref(), Some(expected_hash.as_str()));
        assert_eq!(response.size_bytes, Some(18));
    }

    #[test]
    fn resolve_vesc_tool_artifact_path_checks_root_then_package_root() {
        let workspace = TempWorkspace::new();
        let package_root = workspace.root.join("package");
        std::fs::create_dir_all(&package_root).expect("package dir");
        let artifact = package_root.join("demo.vescpkg");
        std::fs::write(&artifact, b"bytes").expect("artifact");

        assert_eq!(
            resolve_vesc_tool_artifact_path(&workspace.root, &package_root, "demo.vescpkg")
                .as_deref(),
            Some(artifact.as_path())
        );
    }

    #[test]
    fn tool_build_vesc_tool_nested_package_layout_mocked() {
        struct NestedPackageMockRunner {
            artifact_bytes: Vec<u8>,
        }

        impl VescToolRunner for NestedPackageMockRunner {
            fn build_pkg_from_desc(
                &self,
                _vesc_tool: &Path,
                package_root: &Path,
                _pkgdesc_file_name: &str,
                _timeout_secs: u64,
            ) -> Result<(), String> {
                let artifact_path = package_root.join("poc-native-lib-minimal.vescpkg");
                std::fs::write(&artifact_path, &self.artifact_bytes)
                    .map_err(|err| format!("mock write {}: {err}", artifact_path.display()))
            }
        }

        let root = fixture_path("poc-native-lib-minimal");
        let artifact_at_root = root.join("poc-native-lib-minimal.vescpkg");
        let _ = std::fs::remove_file(&artifact_at_root);
        let artifact_bytes = b"nested-package-artifact".to_vec();
        let expected_hash = sha256_hex(&artifact_bytes);
        let runner = NestedPackageMockRunner { artifact_bytes };

        let response = build_vescpkg_tool_with_runner(
            &BuildVescpkgParams {
                root: root.display().to_string(),
                timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
            },
            &runner,
            Some(Path::new("/mock/vesc_tool")),
            Some(&fixture_sandbox_roots()),
        );

        assert!(response.ok, "error: {:?}", response.error);
        let artifact_path = response.artifact_path.expect("artifact_path");
        assert!(
            artifact_path.ends_with("package/poc-native-lib-minimal.vescpkg"),
            "artifact_path: {artifact_path}"
        );
        assert_eq!(response.sha256.as_deref(), Some(expected_hash.as_str()));
    }

    #[test]
    fn tool_build_vesc_tool_invalid_layout_skips_subprocess() {
        let root = fixture_path("broken-missing-lisp");
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner = SpyVescToolRunner {
            called: std::sync::Arc::clone(&called),
        };

        let response = build_vescpkg_tool_with_runner(
            &BuildVescpkgParams {
                root: root.display().to_string(),
                timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
            },
            &runner,
            Some(Path::new("/mock/vesc_tool")),
            Some(&fixture_sandbox_roots()),
        );

        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("LAYOUT_INVALID"),
            "error: {:?}",
            response.error
        );
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "vesc_tool subprocess must not run when layout is invalid"
        );
    }

    #[test]
    fn tool_build_vesc_tool_missing_binary() {
        let root = fixture_path("refloat-minimal");
        let response = build_vescpkg_tool_with_runner(
            &BuildVescpkgParams {
                root: root.display().to_string(),
                timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
            },
            &RealVescToolRunner,
            Some(Path::new("/nonexistent/vesc_tool_for_test")),
            Some(&fixture_sandbox_roots()),
        );

        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("VESC_TOOL_SPAWN_FAILED"),
            "error: {:?}",
            response.error
        );
    }

    #[test]
    fn tool_build_errors_include_hint() {
        let root = fixture_path("broken-missing-lisp");
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: root.display().to_string(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        let err = response.error.expect("structured error");
        assert!(
            err.hint
                .as_ref()
                .is_some_and(|hint| hint.contains("validate_package_layout"))
        );
    }
}
