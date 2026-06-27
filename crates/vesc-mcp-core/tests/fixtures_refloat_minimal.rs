//! Meta-tests for the refloat-minimal fixture layout.

use std::path::PathBuf;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn refloat_minimal_root() -> PathBuf {
    fixtures_root().join("refloat-minimal")
}

#[test]
fn fixtures_refloat_minimal_validates() {
    let root = refloat_minimal_root();
    let pkgdesc_path = root.join("pkgdesc.qml");
    let content = std::fs::read_to_string(&pkgdesc_path).expect("read pkgdesc.qml");

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
