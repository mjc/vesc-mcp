mod common;

use std::fs;

use vesc_mcp_adapters::{AdapterError, build_package_from_root};

use common::fixture_path;

#[test]
fn adapter_builds_poc_native_lib_minimal() {
    let built = build_package_from_root(&fixture_path("poc-native-lib-minimal")).expect("build");
    assert!(built.artifact_path.is_file());
    assert!(built.bytes_len > 0);
    assert_eq!(
        built.artifact_path.file_name().unwrap(),
        "poc-native-lib-minimal.vescpkg"
    );
}

#[test]
fn adapter_missing_native_bin_returns_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let package = root.join("package");
    fs::create_dir_all(&package).expect("package dir");
    fs::write(
        package.join("pkgdesc.qml"),
        r#"import QtQuick 2.15
Item {
    property string pkgName: "broken"
    property string pkgDescriptionMd: "README.md"
    property string pkgLisp: "code.lisp"
    property string pkgQml: ""
    property bool pkgQmlIsFullscreen: false
    property string pkgOutput: "broken.vescpkg"
}
"#,
    )
    .expect("pkgdesc");
    fs::write(package.join("README.md"), "readme").expect("readme");
    fs::write(
        package.join("code.lisp"),
        "(import \"../src/missing.bin\" 'package-lib)\n",
    )
    .expect("lisp");
    // deliberately omit src/missing.bin

    let err = build_package_from_root(root).expect_err("missing native bin");
    assert!(matches!(err, AdapterError::Io { .. }));
}

#[test]
fn adapter_missing_pkgdesc_returns_message_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let err = build_package_from_root(temp.path()).expect_err("no pkgdesc");
    assert!(matches!(err, AdapterError::Message { .. }));
}
