//! Shared paths for adapter integration tests.

use std::path::PathBuf;

#[must_use]
pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[must_use]
pub fn fixture_path(name: &str) -> PathBuf {
    fixtures_root().join(name)
}
