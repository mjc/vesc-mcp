//! Negative meta-tests for intentionally broken fixture variants.

use std::path::PathBuf;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[test]
fn fixtures_broken_missing_lisp_fails_validation() {
    let root = fixtures_root().join("broken-missing-lisp");
    let pkgdesc = std::fs::read_to_string(root.join("pkgdesc.qml")).expect("read pkgdesc");
    assert!(pkgdesc.contains("lisp/missing-package.lisp"));

    let missing = root.join("lisp/missing-package.lisp");
    assert!(
        !missing.exists(),
        "broken fixture must reference a missing lisp path"
    );
}

#[test]
fn fixtures_broken_bad_wire_is_truncated() {
    let path = fixtures_root().join("broken-bad-wire/truncated.vescpkg");
    let bytes = std::fs::read(&path).expect("read truncated vescpkg");
    assert!(
        bytes.len() < 16,
        "truncated fixture should be smaller than a minimal vescpkg header"
    );
}

#[test]
fn fixtures_broken_bad_magic_is_not_vescpkg() {
    let path = fixtures_root().join("broken-bad-magic/bad-magic.vescpkg");
    let bytes = std::fs::read(&path).expect("read bad-magic vescpkg");
    assert_ne!(&bytes[..4.min(bytes.len())], &[0, 0, 0, 0]);
    let text = std::str::from_utf8(&bytes).expect("fixture is utf8 marker");
    assert!(text.starts_with("BADMAGIC"));
}

#[test]
fn fixtures_legacy_colon_desc_matches_oldvt_format() {
    let path = fixtures_root().join("legacy-colon-desc/buildpkg.colon");
    let line = std::fs::read_to_string(&path)
        .expect("read colon descriptor")
        .trim()
        .to_string();

    let fields: Vec<&str> = line.split(':').collect();
    assert_eq!(
        fields.len(),
        6,
        "legacy buildPkg colon descriptor must have six fields"
    );
    assert_eq!(fields[0], "refloat-minimal.vescpkg");
    assert_eq!(fields[1], "lisp/package.lisp");
    assert_eq!(fields[5], "Refloat Minimal");
}
