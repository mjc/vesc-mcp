//! Environment-backed repository roots for catalog path validation.

use std::env;
use std::path::{Path, PathBuf};

/// Logical catalog source repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogRepo {
    Refloat,
    Bldc,
    Poc,
}

impl CatalogRepo {
    #[must_use]
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Refloat => "VESC_REFLOAT_ROOT",
            Self::Bldc => "VESC_BLDC_ROOT",
            Self::Poc => "VESC_POC_ROOT",
        }
    }

    #[must_use]
    pub const fn default_relative(self) -> &'static str {
        match self {
            Self::Refloat => "~/projects/refloat",
            Self::Bldc => "~/projects/bldc",
            Self::Poc => "~/projects/vesc-rust-poc",
        }
    }

    #[must_use]
    pub fn resolve_root(self) -> PathBuf {
        env::var(self.env_var())
            .map_or_else(|_| expand_tilde(self.default_relative()), PathBuf::from)
    }
}

/// Resolved checkout roots for sibling repositories.
#[derive(Debug, Clone)]
pub struct RepoRoots {
    pub refloat: PathBuf,
    pub bldc: PathBuf,
    pub poc: PathBuf,
}

impl RepoRoots {
    #[must_use]
    pub fn from_env() -> Self {
        let config = crate::config::McpConfig::load();
        Self {
            refloat: config.refloat_root.clone(),
            bldc: config.bldc_root.clone(),
            poc: config.poc_root.clone(),
        }
    }

    #[must_use]
    pub fn root_for(&self, repo: CatalogRepo) -> &Path {
        match repo {
            CatalogRepo::Refloat => &self.refloat,
            CatalogRepo::Bldc => &self.bldc,
            CatalogRepo::Poc => &self.poc,
        }
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_uses_home() {
        let expanded = expand_tilde("~/projects/refloat");
        assert!(expanded.ends_with("projects/refloat"));
    }
}
