//! Meta-tests for the poc-native-lib-minimal fixture layout.

use std::path::PathBuf;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn poc_minimal_root() -> PathBuf {
    fixtures_root().join("poc-native-lib-minimal")
}

#[test]
fn fixtures_poc_native_lib_minimal_validates() {
    let root = poc_minimal_root();
    let pkgdesc_path = root.join("package/pkgdesc.qml");
    let content = std::fs::read_to_string(&pkgdesc_path).expect("read pkgdesc.qml");

    assert!(content.contains("property string packageName:"));
    assert!(content.contains("property string nativeLibraryPath: \"src/package_lib.bin\""));
    assert!(content.contains("property string loaderScriptPath: \"code.lisp\""));

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
