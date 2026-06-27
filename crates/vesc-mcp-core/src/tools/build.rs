//! `build_vescpkg` — build `.vescpkg` wire artifacts from on-disk package roots.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vesc_domain::{ParsedPkgDesc, parse_pkgdesc_qml, validate_package_layout};
use vesc_mcp_adapters::locate_pkgdesc;

use crate::config::{McpConfig, allowed_package_roots, validate_sandbox_path};
use crate::tools::tool_error::{
    ToolError, tool_error_from_adapter, tool_error_from_build_timeout, tool_error_from_domain,
    tool_error_from_sandbox, tool_error_from_vesc_tool,
};

/// Default build timeout in seconds (applied when subprocess modes land).
pub const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuildVescpkgParams {
    /// Package root directory containing `pkgdesc.qml` (or `package/pkgdesc.qml`).
    pub root: String,
    /// Build backend: `rust` uses the in-tree adapter; `vesc_tool` spawns the CLI.
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

/// In-process build backend for `rust` mode (mockable in unit tests).
pub trait RustPackageBuilder: Send + Sync {
    /// Build a `.vescpkg` from an on-disk package root.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when layout validation or I/O fails.
    fn build_from_root(
        &self,
        root: &Path,
    ) -> Result<vesc_mcp_adapters::BuiltPackage, vesc_mcp_adapters::AdapterError>;
}

/// Production builder that delegates to [`vesc_mcp_adapters::build_package_from_root`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRustPackageBuilder;

impl RustPackageBuilder for RealRustPackageBuilder {
    fn build_from_root(
        &self,
        root: &Path,
    ) -> Result<vesc_mcp_adapters::BuiltPackage, vesc_mcp_adapters::AdapterError> {
        vesc_mcp_adapters::build_package_from_root(root)
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
    build_vescpkg_tool_with_builders(
        params,
        runner,
        Arc::new(RealRustPackageBuilder),
        vesc_tool_override,
        allowed_roots_override,
    )
}

#[must_use]
pub fn build_vescpkg_tool_with_builders(
    params: &BuildVescpkgParams,
    runner: &dyn VescToolRunner,
    rust_builder: Arc<dyn RustPackageBuilder>,
    vesc_tool_override: Option<&Path>,
    allowed_roots_override: Option<&[PathBuf]>,
) -> BuildVescpkgResponse {
    let root_path = PathBuf::from(&params.root);
    let allowed_roots = allowed_package_roots(allowed_roots_override);
    if let Err(err) = validate_sandbox_path(&root_path, &allowed_roots) {
        return build_error(tool_error_from_sandbox(err));
    }

    match params.mode.as_str() {
        "rust" => build_vescpkg_rust(params, rust_builder),
        "vesc_tool" => build_vescpkg_vesc_tool(params, runner, vesc_tool_override),
        other => build_error(
            ToolError::new(
                "UNSUPPORTED_MODE",
                format!("unsupported build mode {other:?}; expected \"rust\" or \"vesc_tool\""),
            )
            .with_hint("use mode \"rust\" or \"vesc_tool\""),
        ),
    }
}

fn build_vescpkg_rust(
    params: &BuildVescpkgParams,
    builder: Arc<dyn RustPackageBuilder>,
) -> BuildVescpkgResponse {
    let root = PathBuf::from(&params.root);
    let timeout_secs = params.timeout_secs;
    let build_root = root.clone();

    match run_with_timeout(timeout_secs, "rust build", &root, move || {
        builder
            .build_from_root(&build_root)
            .map_err(tool_error_from_adapter)
    }) {
        Ok(built) => BuildVescpkgResponse {
            ok: true,
            artifact_path: Some(built.artifact_path.display().to_string()),
            sha256: Some(built.sha256),
            size_bytes: Some(built.bytes_len),
            error: None,
        },
        Err(err) => build_error(err),
    }
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

fn run_with_timeout<T: Send + 'static>(
    timeout_secs: u64,
    label: &str,
    root: &Path,
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match rx.try_recv() {
            Ok(result) => return result,
            Err(mpsc::TryRecvError::Empty) if Instant::now() >= deadline => {
                return Err(tool_error_from_build_timeout(label, root, timeout_secs));
            }
            Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(50)),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(ToolError::new(
                    "BUILD_FAILED",
                    format!("{label} worker panicked (root {})", root.display()),
                )
                .with_path(root.display().to_string()));
            }
        }
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

    let artifact_path = [root.join(&output_name), package_root.join(&output_name)]
        .into_iter()
        .find(|path| path.is_file());

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
    use std::sync::Arc;

    use super::*;
    use crate::test_support::{TempWorkspace, fixture_path, fixture_sandbox_roots};

    struct SlowRustBuilder {
        delay: Duration,
    }

    impl RustPackageBuilder for SlowRustBuilder {
        fn build_from_root(
            &self,
            _root: &Path,
        ) -> Result<vesc_mcp_adapters::BuiltPackage, vesc_mcp_adapters::AdapterError> {
            thread::sleep(self.delay);
            Err(vesc_mcp_adapters::AdapterError::message(
                "slow builder should have timed out",
            ))
        }
    }

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
        let allowed = vec![workspace.root.clone()];
        let response = build_vescpkg_tool_with_runner(
            &BuildVescpkgParams {
                root: workspace.root.display().to_string(),
                mode: "rust".into(),
                timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
            },
            &RealVescToolRunner,
            None,
            Some(&allowed),
        );

        assert!(!response.ok);
        assert!(response.error.as_ref().is_some_and(|err| {
            err.code == "MISSING_PKGDESC" || err.message.contains("pkgdesc")
        }));
    }

    #[test]
    fn tool_build_rust_mode_respects_timeout() {
        let root = fixture_path("poc-native-lib-minimal");
        let response = build_vescpkg_tool_with_builders(
            &BuildVescpkgParams {
                root: root.display().to_string(),
                mode: "rust".into(),
                timeout_secs: 1,
            },
            &RealVescToolRunner,
            Arc::new(SlowRustBuilder {
                delay: Duration::from_secs(5),
            }),
            None,
            Some(&fixture_sandbox_roots()),
        );

        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("BUILD_TIMEOUT"),
            "error: {:?}",
            response.error
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
        let err = response.error.expect("structured error");
        assert_eq!(err.code, "LAYOUT_INVALID");
        assert!(err.hint.is_some());
    }

    #[test]
    fn tool_build_errors_include_hint() {
        let root = fixture_path("broken-missing-lisp");
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: root.display().to_string(),
            mode: "rust".into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        let err = response.error.expect("structured error");
        assert!(
            err.hint
                .as_ref()
                .is_some_and(|hint| hint.contains("validate_package_layout"))
        );
    }

    #[test]
    fn tool_build_unsupported_mode_fails() {
        let root = fixture_path("poc-native-lib-minimal");
        let response = build_vescpkg_tool(&BuildVescpkgParams {
            root: root.display().to_string(),
            mode: "cmake".into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        });

        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("UNSUPPORTED_MODE"),
            "error: {:?}",
            response.error
        );
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
                mode: "vesc_tool".into(),
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
    fn tool_build_vesc_tool_invalid_layout_skips_subprocess() {
        let root = fixture_path("broken-missing-lisp");
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner = SpyVescToolRunner {
            called: std::sync::Arc::clone(&called),
        };

        let response = build_vescpkg_tool_with_runner(
            &BuildVescpkgParams {
                root: root.display().to_string(),
                mode: "vesc_tool".into(),
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
                mode: "vesc_tool".into(),
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
}
