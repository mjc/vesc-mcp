//! CI wiring assertions for the testing strategy epic.

use std::path::PathBuf;

#[test]
fn ci_runs_nextest_config_present() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.config/nextest.toml");
    assert!(
        path.is_file(),
        "expected nextest config at {}",
        path.display()
    );
    let content = std::fs::read_to_string(&path).expect("read nextest.toml");
    assert!(content.contains("[profile.ci]"));
}

#[test]
fn ci_fixture_catalog_is_documented() {
    let readme = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/README.md");
    assert!(readme.is_file(), "fixture README should exist for CI discoverability");
}
