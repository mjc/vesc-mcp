//! Legacy `--buildPkg` colon descriptor format (OLDVT=1).

use crate::error::DomainError;
use crate::pkgdesc::{OutputFileName, PkgName, RelativeAssetPath};

/// Parsed legacy colon-format `--buildPkg` descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBuildPkgDescriptor {
    pub output: OutputFileName,
    pub lisp: RelativeAssetPath,
    pub qml: RelativeAssetPath,
    pub qml_is_fullscreen: bool,
    pub readme: RelativeAssetPath,
    pub name: PkgName,
}

/// Parse a legacy colon-format `--buildPkg` descriptor line.
///
/// Format: `output:lisp:qml:fullscreen:readme:name`
///
/// # Errors
///
/// Returns [`DomainError::LegacyDescriptor`] when the string is malformed.
pub fn parse_legacy_buildpkg_colon(input: &str) -> Result<LegacyBuildPkgDescriptor, DomainError> {
    let line = input.trim();
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() != 6 {
        return Err(DomainError::LegacyDescriptor {
            message: format!("expected 6 colon-separated fields, got {}", fields.len()),
        });
    }

    let fullscreen = match fields[3] {
        "0" => false,
        "1" => true,
        other => {
            return Err(DomainError::LegacyDescriptor {
                message: format!("invalid fullscreen flag {other:?}, expected 0 or 1"),
            });
        }
    };

    Ok(LegacyBuildPkgDescriptor {
        output: OutputFileName::new(fields[0]),
        lisp: RelativeAssetPath::new(fields[1]),
        qml: RelativeAssetPath::new(fields[2]),
        qml_is_fullscreen: fullscreen,
        readme: RelativeAssetPath::new(fields[4]),
        name: PkgName::new(fields[5]),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixtures_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    #[test]
    fn parse_legacy_buildpkg_colon_format() {
        let path = fixtures_root().join("legacy-colon-desc/buildpkg.colon");
        let content = std::fs::read_to_string(&path).expect("read fixture");
        let parsed = parse_legacy_buildpkg_colon(&content).expect("parse legacy descriptor");

        assert_eq!(parsed.output.as_str(), "refloat-minimal.vescpkg");
        assert_eq!(parsed.lisp.as_path(), PathBuf::from("lisp/package.lisp"));
        assert_eq!(parsed.qml.as_path(), PathBuf::from("ui.qml"));
        assert!(!parsed.qml_is_fullscreen);
        assert_eq!(
            parsed.readme.as_path(),
            PathBuf::from("package_README-gen.md")
        );
        assert_eq!(parsed.name.as_str(), "Refloat Minimal");
    }
}
