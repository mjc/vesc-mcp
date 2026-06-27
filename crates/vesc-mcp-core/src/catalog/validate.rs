//! Validate catalog path references against local repository checkouts.

use std::path::Path;

use thiserror::Error;

use super::env::RepoRoots;
use super::paths::{CatalogPathRef, collect_catalog_path_refs, find_duplicate_catalog_ids};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CatalogValidationError {
    #[error("catalog directory missing: {0}")]
    MissingCatalogDir(String),
    #[error("duplicate catalog ids: {0}")]
    DuplicateIds(String),
    #[error("repo root missing: {var} -> {path}")]
    MissingRepoRoot { var: &'static str, path: String },
    #[error("catalog path missing: {repo:?}/{path} (from {catalog_file})")]
    MissingPath {
        repo: String,
        path: String,
        catalog_file: String,
    },
}

/// Validate that all catalog YAML path references exist under configured repo roots.
///
/// # Errors
///
/// Returns [`CatalogValidationError`] when catalog files are missing, ids duplicate,
/// repo roots are absent, or cited paths do not exist.
pub fn validate_catalog_paths(
    catalog_root: &Path,
    roots: &RepoRoots,
) -> Result<(), CatalogValidationError> {
    if !catalog_root.is_dir() {
        return Err(CatalogValidationError::MissingCatalogDir(
            catalog_root.display().to_string(),
        ));
    }

    let duplicates =
        find_duplicate_catalog_ids(catalog_root).map_err(CatalogValidationError::DuplicateIds)?;
    if !duplicates.is_empty() {
        return Err(CatalogValidationError::DuplicateIds(duplicates.join("; ")));
    }

    ensure_repo_roots_exist(roots)?;

    let refs =
        collect_catalog_path_refs(catalog_root).map_err(CatalogValidationError::DuplicateIds)?;

    for reference in refs {
        check_path_ref(&reference, roots)?;
    }

    Ok(())
}

fn ensure_repo_roots_exist(roots: &RepoRoots) -> Result<(), CatalogValidationError> {
    for (repo, path) in [
        (super::CatalogRepo::Refloat, &roots.refloat),
        (super::CatalogRepo::Bldc, &roots.bldc),
        (super::CatalogRepo::Poc, &roots.poc),
    ] {
        if !path.is_dir() {
            return Err(CatalogValidationError::MissingRepoRoot {
                var: repo.env_var(),
                path: path.display().to_string(),
            });
        }
    }
    Ok(())
}

fn check_path_ref(
    reference: &CatalogPathRef,
    roots: &RepoRoots,
) -> Result<(), CatalogValidationError> {
    let root = roots.root_for(reference.repo);
    let full = root.join(&reference.path);
    if !full.is_file() {
        return Err(CatalogValidationError::MissingPath {
            repo: format!("{:?}", reference.repo),
            path: reference.path.clone(),
            catalog_file: reference.catalog_file.display().to_string(),
        });
    }
    Ok(())
}
