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

fn default_snapshot_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(DEFAULT_SNAPSHOT_FILE)
}

fn read_default_snapshot(root: &std::path::Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(default_snapshot_path(root))
}

#[cfg(test)]
mod default_snapshot_tests {
    use super::*;

    #[test]
    fn default_snapshot_does_not_read_an_unversioned_pointer() {
        let root = tempfile::tempdir().expect("data root");
        std::fs::write(root.path().join("default-snapshot.json"), b"obsolete")
            .expect("unversioned pointer");

        assert_eq!(
            read_default_snapshot(root.path())
                .expect_err("unversioned pointer must be ignored")
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
