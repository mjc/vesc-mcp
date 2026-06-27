//! Locate `pkgdesc.qml` under package / fixture roots.

use std::path::{Path, PathBuf};

use crate::error::AdapterError;

/// Locate `pkgdesc.qml` under a package or fixture root.
///
/// # Errors
///
/// Returns [`AdapterError::Message`] when no descriptor is found.
pub fn locate_pkgdesc(root: &Path) -> Result<(PathBuf, PathBuf), AdapterError> {
    const CANDIDATES: [&str; 2] = ["pkgdesc.qml", "package/pkgdesc.qml"];
    for relative in CANDIDATES {
        let path = root.join(relative);
        if path.is_file() {
            let package_root = path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| AdapterError::message("pkgdesc path has no parent"))?;
            return Ok((path, package_root));
        }
    }
    Err(AdapterError::message(format!(
        "no pkgdesc.qml under {}",
        root.display()
    )))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    #[test]
    fn locate_pkgdesc_finds_poc_fixture() {
        let (path, package_root) =
            locate_pkgdesc(&fixtures_root().join("native-lib-minimal")).expect("pkgdesc");
        assert!(path.ends_with("package/pkgdesc.qml"));
        assert!(package_root.ends_with("package"));
    }

    #[test]
    fn locate_pkgdesc_finds_refloat_minimal_at_root() {
        let (path, package_root) =
            locate_pkgdesc(&fixtures_root().join("refloat-minimal")).expect("pkgdesc");
        assert!(path.ends_with("pkgdesc.qml"));
        assert_eq!(package_root, fixtures_root().join("refloat-minimal"));
    }

    #[test]
    fn locate_pkgdesc_missing_returns_message_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = locate_pkgdesc(temp.path()).expect_err("no pkgdesc");
        assert!(matches!(err, AdapterError::Message { .. }));
    }
}
