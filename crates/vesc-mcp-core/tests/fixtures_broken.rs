//! Negative meta-tests for intentionally broken fixture variants.

use std::path::Path;

use vesc_mcp_core::test_support::{asset_missing, fixture_path, read_fixture_file};

#[test]
fn fixtures_broken_missing_lisp_fails_validation() {
    let root = fixture_path("broken-missing-lisp");
    let pkgdesc = read_fixture_file("broken-missing-lisp", "pkgdesc.qml");
    assert!(pkgdesc.contains("lisp/missing-package.lisp"));
    assert!(asset_missing(
        &root,
        Path::new("lisp/missing-package.lisp")
    ));
}

#[test]
fn fixtures_broken_bad_wire_is_truncated() {
    let path = fixture_path("broken-bad-wire/truncated.vescpkg");
    let bytes = std::fs::read(&path).expect("read truncated vescpkg");
    assert!(
        bytes.len() < 16,
        "truncated fixture should be smaller than a minimal vescpkg header"
    );
}

#[test]
fn fixtures_broken_bad_magic_is_not_vescpkg() {
    let path = fixture_path("broken-bad-magic/bad-magic.vescpkg");
    let bytes = std::fs::read(&path).expect("read bad-magic vescpkg");
    assert_ne!(&bytes[..4.min(bytes.len())], &[0, 0, 0, 0]);
    let text = std::str::from_utf8(&bytes).expect("fixture is utf8 marker");
    assert!(text.starts_with("BADMAGIC"));
}

#[test]
fn fixtures_legacy_colon_desc_matches_oldvt_format() {
    let line = read_fixture_file("legacy-colon-desc", "buildpkg.colon")
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
