//! Package layout and artifact naming.

use crate::pkgdesc::{OutputFileName, ParsedPkgDesc, PkgDescVescTool};

/// Sanitize a package name for artifact filenames (POC `PackageLayout::artifact_name` rules).
#[must_use]
pub fn sanitize_pkg_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Expected output `.vescpkg` filename from a parsed descriptor.
#[must_use]
pub fn expected_artifact_name(desc: &ParsedPkgDesc) -> OutputFileName {
    match desc {
        ParsedPkgDesc::VescTool(vesc_tool) => expected_artifact_name_vesc_tool(vesc_tool),
    }
}

fn expected_artifact_name_vesc_tool(desc: &PkgDescVescTool) -> OutputFileName {
    desc.output_name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdesc::{PkgName, RelativeAssetPath};

    #[test]
    fn sanitize_pkg_name_replaces_spaces() {
        assert_eq!(
            sanitize_pkg_name("Rust BLE loopback test package"),
            "Rust-BLE-loopback-test-package"
        );
    }

    #[test]
    fn expected_artifact_name_uses_pkg_output() {
        let desc = ParsedPkgDesc::VescTool(PkgDescVescTool::new(
            PkgName::new("Refloat Minimal"),
            RelativeAssetPath::new("package_README-gen.md"),
            RelativeAssetPath::new("lisp/package.lisp"),
            RelativeAssetPath::new("ui.qml"),
            OutputFileName::new("refloat-minimal.vescpkg"),
            false,
        ));
        assert_eq!(
            expected_artifact_name(&desc).as_str(),
            "refloat-minimal.vescpkg"
        );
    }
}
