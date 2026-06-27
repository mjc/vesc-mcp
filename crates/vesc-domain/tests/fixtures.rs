//! Fixture-driven integration tests for pkgdesc, wire, layout, and legacy parsing.

use std::path::{Path, PathBuf};

use vesc_domain::{
    DomainError, ParsedPkgDesc, parse_legacy_buildpkg_colon, parse_pkgdesc_qml,
    parse_vescpkg_fields, read_vescpkg_fields, validate_package_layout, wire,
};

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn fixture_path(name: &str) -> PathBuf {
    fixtures_root().join(name)
}

fn read_fixture(name: &str, relative: impl AsRef<Path>) -> String {
    let path = fixture_path(name).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read fixture {}: {err}", path.display());
    })
}

#[test]
fn fixture_refloat_minimal_pkgdesc_and_layout() {
    let root = fixture_path("refloat-minimal");
    let content = read_fixture("refloat-minimal", "pkgdesc.qml");
    let desc = parse_pkgdesc_qml(&content, root.join("pkgdesc.qml")).expect("parse pkgdesc");
    let ParsedPkgDesc::VescTool(vesc_tool) = &desc;
    assert_eq!(vesc_tool.pkg_name.as_str(), "Refloat Minimal");
    assert_eq!(vesc_tool.output_name.as_str(), "refloat-minimal.vescpkg");
    assert!(validate_package_layout(&root, &desc).is_ok());
}

#[test]
fn fixture_poc_native_lib_minimal_layout_ok() {
    let root = fixture_path("poc-native-lib-minimal/package");
    let content = read_fixture("poc-native-lib-minimal/package", "pkgdesc.qml");
    let desc = parse_pkgdesc_qml(&content, root.join("pkgdesc.qml")).expect("parse pkgdesc");
    assert!(validate_package_layout(&root, &desc).is_ok());
}

#[test]
fn fixture_broken_missing_lisp_validation_fails() {
    let root = fixture_path("broken-missing-lisp");
    let content = read_fixture("broken-missing-lisp", "pkgdesc.qml");
    let desc = parse_pkgdesc_qml(&content, root.join("pkgdesc.qml")).expect("parse pkgdesc");
    let report = validate_package_layout(&root, &desc);
    assert!(!report.is_ok());
}

#[test]
fn fixture_golden_vescpkg_round_trip() {
    let path = fixture_path("golden/poc-minimal.vescpkg");
    let fields = read_vescpkg_fields(&path).expect("read golden vescpkg");
    assert_eq!(fields.name, "POC native-lib minimal fixture");

    let bytes = std::fs::read(&path).expect("read bytes");
    let parsed = parse_vescpkg_fields(&bytes).expect("parse bytes");
    assert_eq!(parsed.name, fields.name);
}

#[test]
fn fixture_broken_bad_magic_wire_error() {
    let path = fixture_path("broken-bad-magic/bad-magic.vescpkg");
    let bytes = std::fs::read(&path).expect("read bytes");
    let err = parse_vescpkg_fields(&bytes).expect_err("bad magic");
    assert!(matches!(err, DomainError::InvalidWireFormat { .. }));
}

#[test]
fn fixture_broken_truncated_wire_error() {
    let path = fixture_path("broken-bad-wire/truncated.vescpkg");
    let bytes = std::fs::read(&path).expect("read bytes");
    let err = parse_vescpkg_fields(&bytes).expect_err("truncated");
    assert!(matches!(err, DomainError::InvalidWireFormat { .. }));
}

#[test]
fn fixture_legacy_colon_descriptor() {
    let content = read_fixture("legacy-colon-desc", "buildpkg.colon");
    let parsed = parse_legacy_buildpkg_colon(&content).expect("parse legacy");
    assert_eq!(parsed.output.as_str(), "refloat-minimal.vescpkg");
    assert_eq!(parsed.name.as_str(), "Refloat Minimal");
}

#[test]
fn fixture_suite_all_green() {
    let golden = fixture_path("golden/poc-minimal.vescpkg");
    assert!(golden.is_file(), "golden vescpkg must exist");

    let sha_path = fixture_path("golden/poc-minimal.sha256");
    let sha_line = std::fs::read_to_string(&sha_path).expect("read sha256");
    assert!(sha_line.contains("poc-minimal.vescpkg"));

    let fields = read_vescpkg_fields(&golden).expect("golden wire");
    let (_, imports) = wire::parse_lisp_imports(&fields.lisp_data).expect("imports");
    assert_eq!(imports.len(), 1);
}
