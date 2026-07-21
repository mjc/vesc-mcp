//! Workspace root discovery and vendor submodule paths.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment variable overriding the `vesc_tool` source checkout root.
pub const VESC_VESC_TOOL_ROOT_ENV: &str = "VESC_VESC_TOOL_ROOT";

static WORKSPACE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolved workspace root (repository root containing `catalog/` or `flake.nix`).
#[must_use]
pub fn workspace_root() -> Option<PathBuf> {
    WORKSPACE_ROOT.get_or_init(discover_workspace_root).clone()
}

/// Resolve the repository catalog, preferring the runtime-configured workspace.
#[must_use]
pub fn catalog_root() -> PathBuf {
    workspace_root().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog"),
        |root| root.join("catalog"),
    )
}

/// Resolve repository fixtures, preferring the runtime-configured workspace.
#[must_use]
pub fn fixtures_root() -> PathBuf {
    workspace_root().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures"),
        |root| root.join("tests/fixtures"),
    )
}

fn discover_workspace_root() -> Option<PathBuf> {
    if let Ok(root) = env::var("VESC_MCP_WORKSPACE_ROOT") {
        let path = PathBuf::from(root);
        if path.is_dir() {
            return Some(path);
        }
    }

    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("catalog").is_dir() || dir.join("flake.nix").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Return `vendor/{subdir}` when the submodule checkout is initialized.
#[must_use]
pub fn vendor_checkout(subdir: &str) -> Option<PathBuf> {
    let root = workspace_root()?;
    let path = root.join("vendor").join(subdir);
    path.is_dir().then_some(path)
}

#[must_use]
pub fn vendor_vesc() -> Option<PathBuf> {
    vendor_checkout("vesc")
}

#[must_use]
pub fn vendor_refloat() -> Option<PathBuf> {
    vendor_checkout("refloat")
}

#[must_use]
pub fn vendor_vesc_tool() -> Option<PathBuf> {
    vendor_checkout("vesc_tool")
}

/// Expand `~/…` and workspace-relative paths (e.g. `vendor/vesc`).
#[must_use]
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }

    let candidate = PathBuf::from(path);
    if candidate.is_relative()
        && let Some(root) = workspace_root()
    {
        return root.join(path);
    }
    candidate
}

/// True when `path` is nested under the workspace `vendor/` tree.
#[must_use]
pub fn is_under_vendor(path: &Path) -> bool {
    let Some(root) = workspace_root() else {
        return false;
    };
    path.starts_with(root.join("vendor"))
}

/// True when `path` is equal to or nested under `root` (prefix-safe).
#[must_use]
pub fn path_within_root(path: &Path, root: &Path) -> bool {
    let mut root_components = root.components();
    for component in path.components() {
        match root_components.next() {
            Some(expected) if expected == component => {}
            Some(_) => return false,
            None => return true,
        }
    }
    root_components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_path_resolves_vendor_relative() {
        let expanded = expand_path("vendor/vesc");
        if let Some(ws) = workspace_root() {
            assert_eq!(expanded, ws.join("vendor/vesc"));
        }
    }

    #[test]
    fn vendor_checkout_missing_when_uninitialized() {
        if vendor_vesc().is_none() {
            assert!(workspace_root().is_some());
        }
    }
}
