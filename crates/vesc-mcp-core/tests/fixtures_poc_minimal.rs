//! Meta-tests for the poc-native-lib-minimal fixture layout.

use vesc_mcp_core::test_support::{fixture_path, read_fixture_file};

#[test]
fn fixtures_poc_native_lib_minimal_validates() {
    let root = fixture_path("poc-native-lib-minimal");
    let content = read_fixture_file("poc-native-lib-minimal", "package/pkgdesc.qml");

    assert!(content.contains("property string pkgName:"));
    assert!(content.contains("property string pkgLisp: \"code.lisp\""));
    assert!(content.contains("property string pkgOutput:"));

    for relative in [
        "package/pkgdesc.qml",
        "package/code.lisp",
        "package/README.md",
        "src/rules.mk",
        "src/package_lib.bin",
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
