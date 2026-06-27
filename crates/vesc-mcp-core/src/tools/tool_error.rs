//! Structured tool error responses shared across MCP tools.

use serde::{Deserialize, Serialize};
use vesc_domain::DomainError;
use vesc_mcp_adapters::AdapterError;

use crate::config::VESC_PACKAGE_ROOTS_ENV;

/// Epic error contract: `{ code, message, path?, hint? }`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ToolError {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
            hint: None,
        }
    }

    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[must_use]
pub fn tool_error_from_sandbox(message: String) -> ToolError {
    let code = if message.contains("outside configured") {
        "SANDBOX_DENIED"
    } else if message.contains("not a directory") {
        "INVALID_PATH"
    } else if message.contains("path sandbox") {
        "SANDBOX_NOT_CONFIGURED"
    } else {
        "SANDBOX_DENIED"
    };
    ToolError::new(code, message).with_hint(format!(
        "set {VESC_PACKAGE_ROOTS_ENV} to allow package roots (comma-separated paths)"
    ))
}

#[must_use]
pub fn tool_error_from_domain(err: DomainError) -> ToolError {
    match err {
        DomainError::MissingAsset { asset, root } => ToolError::new(
            "MISSING_ASSET",
            format!(
                "missing asset `{}` relative to package root `{}`",
                asset.display(),
                root.display()
            ),
        )
        .with_path(asset.display().to_string())
        .with_hint("add the missing file or update pkgdesc asset paths"),
        DomainError::MissingProperty { property, path } => ToolError::new(
            "MISSING_PKGDESC_PROPERTY",
            format!("missing pkgdesc property `{property}` in {}", path.display()),
        )
        .with_path(path.display().to_string())
        .with_hint("add the required vesc_tool property to pkgdesc.qml"),
        DomainError::InvalidProperty {
            property,
            path,
            message,
        } => ToolError::new(
            "INVALID_PKGDESC_PROPERTY",
            format!(
                "invalid pkgdesc property `{property}` in {}: {message}",
                path.display()
            ),
        )
        .with_path(path.display().to_string()),
        DomainError::UnknownDialect { path } => ToolError::new(
            "UNKNOWN_PKGDESC_DIALECT",
            format!(
                "unknown pkgdesc dialect in {}: expected vesc_tool properties starting with `pkgName`",
                path.display()
            ),
        )
        .with_path(path.display().to_string())
        .with_hint("use vesc_tool pkgdesc properties (`pkgName`, `pkgLisp`, …)"),
        DomainError::LegacyPocDialect { path } => ToolError::new(
            "LEGACY_POC_DIALECT",
            format!(
                "legacy POC pkgdesc dialect in {}: use vesc_tool properties instead of POC-only fields",
                path.display()
            ),
        )
        .with_path(path.display().to_string())
        .with_hint("rename POC fields to vesc_tool names (`pkgName`, `pkgLisp`, …)"),
        DomainError::InvalidWireFormat { message } => {
            ToolError::new("INVALID_WIRE_FORMAT", message)
        }
        DomainError::Io { path, source } => ToolError::new(
            "IO_ERROR",
            format!("failed to read {}: {source}", path.display()),
        )
        .with_path(path.display().to_string())
        .with_hint("ensure the referenced file exists and is readable"),
        DomainError::LegacyDescriptor { message } => {
            ToolError::new("LEGACY_DESCRIPTOR", message)
        }
    }
}

#[must_use]
pub fn tool_error_from_adapter(err: AdapterError) -> ToolError {
    match err {
        AdapterError::Domain(domain) => tool_error_from_domain(domain),
        AdapterError::Io { path, source } => ToolError::new(
            "IO_ERROR",
            format!("failed to read {}: {source}", path.display()),
        )
        .with_path(path.display().to_string())
        .with_hint("ensure the referenced file exists and is readable"),
        AdapterError::LayoutInvalid { root } => ToolError::new(
            "LAYOUT_INVALID",
            format!("package layout invalid under {}", root.display()),
        )
        .with_path(root.display().to_string())
        .with_hint("run validate_package_layout to list missing assets"),
        AdapterError::Message { message } => {
            if message.contains("no pkgdesc.qml") {
                ToolError::new("MISSING_PKGDESC", message)
                    .with_hint("add pkgdesc.qml or package/pkgdesc.qml under the package root")
            } else {
                ToolError::new("BUILD_FAILED", message)
            }
        }
        _ => ToolError::new("BUILD_FAILED", err.to_string()),
    }
}

#[must_use]
pub fn tool_error_from_build_timeout(
    label: &str,
    root: &std::path::Path,
    timeout_secs: u64,
) -> ToolError {
    ToolError::new(
        "BUILD_TIMEOUT",
        format!(
            "{label} timed out after {timeout_secs}s (root {})",
            root.display()
        ),
    )
    .with_path(root.display().to_string())
    .with_hint("increase timeout_secs or fix a stuck build")
}

#[must_use]
pub fn tool_error_from_vesc_tool(err: String, root: &std::path::Path) -> ToolError {
    if err.contains("timed out") {
        return ToolError::new("BUILD_TIMEOUT", err)
            .with_path(root.display().to_string())
            .with_hint("increase timeout_secs or fix a stuck vesc_tool build");
    }
    if err.starts_with("spawn ") {
        return ToolError::new("VESC_TOOL_SPAWN_FAILED", err)
            .with_hint("install vesc_tool or set VESC_TOOL_PATH to the binary");
    }
    if err.starts_with("vesc_tool exited") {
        return ToolError::new("VESC_TOOL_BUILD_FAILED", err)
            .with_path(root.display().to_string())
            .with_hint("run vesc_tool --buildPkgFromDesc manually to inspect stderr");
    }
    if err.contains("artifact") && err.contains("not found") {
        return ToolError::new("ARTIFACT_NOT_FOUND", err)
            .with_path(root.display().to_string())
            .with_hint("check pkgdesc outputName matches the file vesc_tool writes");
    }
    ToolError::new("BUILD_FAILED", err)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn tool_errors_include_hint_for_layout_invalid() {
        let err = tool_error_from_adapter(AdapterError::LayoutInvalid {
            root: PathBuf::from("fixtures/broken-missing-lisp"),
        });
        assert_eq!(err.code, "LAYOUT_INVALID");
        assert!(err.hint.is_some());
        assert!(err.path.is_some());
    }

    #[test]
    fn tool_errors_include_hint_for_sandbox_denied() {
        let err = tool_error_from_sandbox(
            "path /tmp/outside is outside configured VESC_PACKAGE_ROOTS".into(),
        );
        assert_eq!(err.code, "SANDBOX_DENIED");
        assert!(
            err.hint
                .is_some_and(|hint| hint.contains("VESC_PACKAGE_ROOTS"))
        );
    }

    #[test]
    fn tool_errors_map_missing_asset_from_domain() {
        let err = tool_error_from_domain(DomainError::MissingAsset {
            asset: PathBuf::from("lisp/missing-package.lisp"),
            root: PathBuf::from("fixtures/broken-missing-lisp"),
        });
        assert_eq!(err.code, "MISSING_ASSET");
        assert_eq!(err.path.as_deref(), Some("lisp/missing-package.lisp"));
        assert!(err.hint.is_some());
    }
}
