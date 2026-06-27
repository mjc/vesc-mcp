//! Core types and MCP tool wiring for the vesc-mcp server.

pub mod catalog;
pub mod error;
pub mod server;
pub mod tools;

#[doc(hidden)]
pub mod test_support;

pub use catalog::{
    CatalogRepo, CatalogValidationError, RepoRoots, collect_catalog_path_refs,
    find_duplicate_catalog_ids, validate_catalog_paths,
};
pub use error::{CoreError, CoreResult};
pub use server::VescMcpService;
