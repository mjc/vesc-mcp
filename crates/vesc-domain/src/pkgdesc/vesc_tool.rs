//! `vesc_tool` / refloat pkgdesc dialect types.

use super::newtypes::{OutputFileName, PkgName, RelativeAssetPath};

/// Descriptor fields from the `vesc_tool` schema (`pkgName`, `pkgLisp`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgDescVescTool {
    pub pkg_name: PkgName,
    pub description_md_path: RelativeAssetPath,
    pub lisp_path: RelativeAssetPath,
    pub qml_path: RelativeAssetPath,
    pub output_name: OutputFileName,
    pub qml_is_fullscreen: bool,
}

impl PkgDescVescTool {
    #[must_use]
    pub const fn new(
        pkg_name: PkgName,
        description_md_path: RelativeAssetPath,
        lisp_path: RelativeAssetPath,
        qml_path: RelativeAssetPath,
        output_name: OutputFileName,
        qml_is_fullscreen: bool,
    ) -> Self {
        Self {
            pkg_name,
            description_md_path,
            lisp_path,
            qml_path,
            output_name,
            qml_is_fullscreen,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn refloat_fields() -> PkgDescVescTool {
        PkgDescVescTool::new(
            PkgName::new("Refloat"),
            RelativeAssetPath::new("package_README-gen.md"),
            RelativeAssetPath::new("lisp/package.lisp"),
            RelativeAssetPath::new("ui.qml"),
            OutputFileName::new("refloat.vescpkg"),
            false,
        )
    }

    #[test]
    fn refloat_pkgdesc_fields_match_schema() {
        let desc = refloat_fields();
        assert_eq!(desc.pkg_name.as_str(), "Refloat");
        assert_eq!(
            desc.description_md_path.as_path(),
            Path::new("package_README-gen.md")
        );
        assert_eq!(desc.lisp_path.as_path(), Path::new("lisp/package.lisp"));
        assert_eq!(desc.qml_path.as_path(), Path::new("ui.qml"));
        assert_eq!(desc.output_name.as_str(), "refloat.vescpkg");
        assert!(!desc.qml_is_fullscreen);
    }
}
