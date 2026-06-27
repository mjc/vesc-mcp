//! Build `.vescpkg` artifacts from on-disk package roots.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use vesc_domain::{ParsedPkgDesc, parse_pkgdesc_qml, validate_package_layout};
use vesc_pkg_build::package_format::{VescPackageInput, write_vesc_package};

use crate::error::AdapterError;

/// Result of a successful package build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltPackage {
    pub artifact_path: PathBuf,
    pub bytes_len: usize,
    pub sha256: String,
}

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

/// Build a `.vescpkg` from a fixture or package tree using `vesc_tool` field names on disk.
///
/// # Errors
///
/// Returns [`AdapterError`] on missing assets, parse failures, or I/O errors.
pub fn build_package_from_root(root: &Path) -> Result<BuiltPackage, AdapterError> {
    let (pkgdesc_path, package_root) = locate_pkgdesc(root)?;
    let pkgdesc_src = read_to_string(&pkgdesc_path)?;
    let parsed = parse_pkgdesc_qml(&pkgdesc_src, &pkgdesc_path)?;

    let report = validate_package_layout(&package_root, &parsed);
    if !report.is_ok() {
        return Err(AdapterError::LayoutInvalid { root: package_root });
    }

    let ParsedPkgDesc::VescTool(desc) = parsed;
    let description_md = read_to_string(&package_root.join(desc.description_md_path.as_path()))?;
    let lisp_path = package_root.join(desc.lisp_path.as_path());
    let lisp_source = read_to_string(&lisp_path)?;
    // POC tests pass the fixture/repo root as `lisp_editor_path`, not the `.lisp` file path.
    let lisp_editor_root = root;
    let qml_file = if desc.qml_path.as_path().as_os_str().is_empty() {
        String::new()
    } else {
        read_to_string(&package_root.join(desc.qml_path.as_path()))?
    };

    let artifact_path = root.join(desc.output_name.as_str());
    let input = VescPackageInput {
        name: desc.pkg_name.as_str(),
        description_md: &description_md,
        lisp_source: &lisp_source,
        lisp_editor_path: lisp_editor_root,
        qml_file: &qml_file,
        pkg_desc_qml: &pkgdesc_src,
        qml_is_fullscreen: desc.qml_is_fullscreen,
    };
    let bytes = write_vesc_package(&artifact_path, &input).map_err(|source| AdapterError::Io {
        path: artifact_path.clone(),
        source,
    })?;

    Ok(BuiltPackage {
        artifact_path,
        bytes_len: bytes.len(),
        sha256: sha256_hex(&bytes),
    })
}

fn read_to_string(path: &Path) -> Result<String, AdapterError> {
    fs::read_to_string(path).map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    #[test]
    fn adapter_crate_compiles() {
        assert!(std::mem::size_of::<BuiltPackage>() > 0);
    }

    #[test]
    fn locate_pkgdesc_finds_poc_fixture() {
        let (path, package_root) =
            locate_pkgdesc(&fixtures_root().join("poc-native-lib-minimal")).expect("pkgdesc");
        assert!(path.ends_with("package/pkgdesc.qml"));
        assert!(package_root.ends_with("package"));
    }
}
