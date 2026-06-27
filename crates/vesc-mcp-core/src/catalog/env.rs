//! Environment-backed repository roots for catalog path validation.

use std::env;
use std::path::{Path, PathBuf};

use crate::workspace::{self, expand_path};

/// Logical catalog source repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogRepo {
    Refloat,
    Bldc,
    Poc,
    VescTool,
    VescMcp,
}

impl CatalogRepo {
    #[must_use]
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Refloat => "VESC_REFLOAT_ROOT",
            Self::Bldc => "VESC_BLDC_ROOT",
            Self::Poc => "VESC_POC_ROOT",
            Self::VescTool => workspace::VESC_VESC_TOOL_ROOT_ENV,
            Self::VescMcp => "VESC_MCP_ROOT",
        }
    }

    #[must_use]
    pub const fn vendor_subdir(self) -> Option<&'static str> {
        match self {
            Self::Refloat => Some("refloat"),
            Self::Bldc => Some("bldc"),
            Self::VescTool => Some("vesc_tool"),
            Self::Poc | Self::VescMcp => None,
        }
    }

    #[must_use]
    pub const fn sibling_default(self) -> &'static str {
        match self {
            Self::Refloat => "~/projects/refloat",
            Self::Bldc => "~/projects/bldc",
            Self::Poc => "~/projects/vesc-rust-poc",
            Self::VescTool => "~/projects/vesc_tool",
            Self::VescMcp => ".",
        }
    }

    /// Resolve checkout root: env override, then initialized `vendor/` submodule, then sibling default.
    #[must_use]
    pub fn resolve_root(self) -> PathBuf {
        if self == Self::VescMcp {
            return workspace::workspace_root().unwrap_or_else(|| expand_path("."));
        }
        if let Ok(path) = env::var(self.env_var()) {
            return PathBuf::from(path);
        }
        if let Some(subdir) = self.vendor_subdir() {
            if let Some(vendor) = workspace::vendor_checkout(subdir) {
                return vendor;
            }
        }
        expand_path(self.sibling_default())
    }
}

/// Resolved checkout roots for sibling repositories and submodules.
#[derive(Debug, Clone)]
pub struct RepoRoots {
    pub refloat: PathBuf,
    pub bldc: PathBuf,
    pub poc: PathBuf,
    pub vesc_tool: PathBuf,
    pub vesc_mcp: PathBuf,
}

impl RepoRoots {
    #[must_use]
    pub fn from_env() -> Self {
        let config = crate::config::McpConfig::load();
        Self {
            refloat: config.refloat_root.clone(),
            bldc: config.bldc_root.clone(),
            poc: config.poc_root.clone(),
            vesc_tool: config.vesc_tool_root.clone(),
            vesc_mcp: CatalogRepo::VescMcp.resolve_root(),
        }
    }

    #[must_use]
    pub fn root_for(&self, repo: CatalogRepo) -> &Path {
        match repo {
            CatalogRepo::Refloat => &self.refloat,
            CatalogRepo::Bldc => &self.bldc,
            CatalogRepo::Poc => &self.poc,
            CatalogRepo::VescTool => &self.vesc_tool,
            CatalogRepo::VescMcp => &self.vesc_mcp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_path_resolves_vendor_relative() {
        let expanded = expand_path("vendor/bldc");
        if let Some(ws) = workspace::workspace_root() {
            assert_eq!(expanded, ws.join("vendor/bldc"));
        }
    }

    #[test]
    fn resolve_root_falls_back_to_sibling_when_vendor_missing() {
        if workspace::vendor_bldc().is_none() {
            let root = CatalogRepo::Bldc.resolve_root();
            assert!(root.ends_with("bldc"));
        }
    }
}
