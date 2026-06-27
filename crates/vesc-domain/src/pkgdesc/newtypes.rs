//! Shared newtypes for pkgdesc dialects.

use std::path::{Path, PathBuf};

/// Human-readable package name from a pkgdesc descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PkgName(String);

impl PkgName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Sanitize for artifact filenames (POC `PackageLayout::artifact_name` rules).
    #[must_use]
    pub fn sanitize_for_artifact(&self) -> String {
        sanitize(self.as_str())
    }
}

/// Relative path to a package asset from the project root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativeAssetPath(PathBuf);

impl RelativeAssetPath {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Output `.vescpkg` filename from a `vesc_tool` descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutputFileName(String);

impl OutputFileName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Semantic version string from a native-lib descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageVersion(String);

impl PackageVersion {
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn sanitize(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_name_sanitize_replaces_spaces_with_hyphens() {
        let name = PkgName::new("Rust BLE loopback test package");
        assert_eq!(
            name.sanitize_for_artifact(),
            "Rust-BLE-loopback-test-package"
        );
    }
}
