//! Meta-tests for the refloat-minimal fixture layout.

use vesc_mcp_core::test_support::{fixture_path, read_fixture_file};

#[test]
fn fixtures_refloat_minimal_validates() {
    let root = fixture_path("refloat-minimal");
    let content = read_fixture_file("refloat-minimal", "pkgdesc.qml");

    assert!(content.contains("property string pkgName: \"Refloat Minimal\""));
    assert!(content.contains("property string pkgOutput: \"refloat-minimal.vescpkg\""));
    assert!(content.contains("property string pkgLisp: \"lisp/package.lisp\""));

    for relative in [
        "pkgdesc.qml",
        "lisp/package.lisp",
        "ui.qml",
        "package_README-gen.md",
    ] {
        let asset = root.join(relative);
        assert!(
            asset.is_file(),
            "expected asset {} to exist under {}",
            asset.display(),
            root.display()
        );
    }
}
