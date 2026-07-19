//! Catalog path resolution and validation for curated refloat/vesc knowledge.

mod env;
mod paths;
mod validate;

pub use env::{CatalogRepo, RepoRoots};
pub use paths::{CatalogPathRef, collect_catalog_path_refs, find_duplicate_catalog_ids};
pub use validate::{CatalogValidationError, validate_catalog_paths};
