//! POC native-lib baseline pkgdesc dialect types.

use super::newtypes::{PackageVersion, PkgName, RelativeAssetPath};

/// Descriptor fields from the POC native-lib schema (`packageName`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgDescNativeLib {
    pub package_name: PkgName,
    pub version: PackageVersion,
    pub native_library_path: RelativeAssetPath,
    pub loader_script_path: RelativeAssetPath,
}

impl PkgDescNativeLib {
    #[must_use]
    pub const fn new(
        package_name: PkgName,
        version: PackageVersion,
        native_library_path: RelativeAssetPath,
        loader_script_path: RelativeAssetPath,
    ) -> Self {
        Self {
            package_name,
            version,
            native_library_path,
            loader_script_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn poc_native_fields() -> PkgDescNativeLib {
        PkgDescNativeLib::new(
            PkgName::new("Rust BLE loopback test package"),
            PackageVersion::new("0.1.0"),
            RelativeAssetPath::new("src/package_lib.bin"),
            RelativeAssetPath::new("code.lisp"),
        )
    }

    #[test]
    fn poc_native_pkgdesc_fields_match_schema() {
        let desc = poc_native_fields();
        assert_eq!(desc.package_name.as_str(), "Rust BLE loopback test package");
        assert_eq!(desc.version.as_str(), "0.1.0");
        assert_eq!(
            desc.native_library_path.as_path(),
            Path::new("src/package_lib.bin")
        );
        assert_eq!(desc.loader_script_path.as_path(), Path::new("code.lisp"));
    }

    #[test]
    fn poc_package_name_sanitizes_for_artifact() {
        let desc = poc_native_fields();
        assert_eq!(
            desc.package_name.sanitize_for_artifact(),
            "Rust-BLE-loopback-test-package"
        );
    }
}
