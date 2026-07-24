//! Core types and MCP tool wiring for the vesc-mcp server.

pub mod benchmark;
pub mod catalog;
pub mod config;
pub mod error;
pub mod managed_git;
pub mod managed_repositories;
pub mod managed_snapshots;
pub mod preparation_status;
pub mod resources;
pub mod server;
pub mod tools;
pub mod workspace;

pub(crate) fn install_ring_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

#[doc(hidden)]
pub mod test_support;

pub use catalog::{
    CatalogRepo, CatalogValidationError, RepoRoots, collect_catalog_path_refs,
    find_duplicate_catalog_ids, validate_catalog_paths,
};
pub use error::{CoreError, CoreResult};
pub use server::{HttpMcpService, VescMcpService};

const DEFAULT_SNAPSHOT_FILE: &str = "default-snapshot-corpus-1.1.json";
const LEGACY_DEFAULT_SNAPSHOT_FILE: &str = "default-snapshot.json";

fn default_snapshot_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(DEFAULT_SNAPSHOT_FILE)
}

fn read_default_snapshot(root: &std::path::Path) -> std::io::Result<Vec<u8>> {
    match std::fs::read(default_snapshot_path(root)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::read(root.join(LEGACY_DEFAULT_SNAPSHOT_FILE))
        }
        result => result,
    }
}

#[cfg(test)]
mod default_snapshot_tests {
    use super::*;

    #[test]
    fn versioned_default_preserves_and_supersedes_the_rollback_pointer() {
        let root = tempfile::tempdir().expect("data root");
        let legacy = root.path().join(LEGACY_DEFAULT_SNAPSHOT_FILE);
        std::fs::write(&legacy, b"legacy").expect("legacy pointer");
        assert_eq!(
            read_default_snapshot(root.path()).expect("legacy fallback"),
            b"legacy"
        );

        std::fs::write(default_snapshot_path(root.path()), b"current").expect("versioned pointer");
        assert_eq!(
            read_default_snapshot(root.path()).expect("versioned pointer"),
            b"current"
        );
        assert_eq!(
            std::fs::read(legacy).expect("preserved rollback pointer"),
            b"legacy"
        );
    }
}
