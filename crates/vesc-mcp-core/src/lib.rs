//! Core types and MCP tool wiring for the vesc-mcp server.

pub mod benchmark;
pub mod catalog;
pub mod config;
pub mod error;
#[cfg(feature = "managed-git")]
pub mod managed_git;
pub mod managed_repositories;
pub mod resources;
pub mod server;
pub mod tools;
pub mod workspace;

#[doc(hidden)]
pub mod test_support;

pub use catalog::{
    CatalogRepo, CatalogValidationError, RepoRoots, collect_catalog_path_refs,
    find_duplicate_catalog_ids, validate_catalog_paths,
};
pub use error::{CoreError, CoreResult};
pub use server::{HttpMcpService, VescMcpService};
