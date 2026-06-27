//! Validate catalog path references against local repository checkouts.

use std::path::Path;

use thiserror::Error;

use super::env::{CatalogRepo, RepoRoots};
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

    let refs =
        collect_catalog_path_refs(catalog_root).map_err(CatalogValidationError::DuplicateIds)?;

    ensure_repo_roots_exist(roots, &refs)?;

    for reference in refs {
        check_path_ref(&reference, roots)?;
    }

    Ok(())
}

fn ensure_repo_roots_exist(
    roots: &RepoRoots,
    refs: &[CatalogPathRef],
) -> Result<(), CatalogValidationError> {
    for (repo, path) in [
        (CatalogRepo::Refloat, &roots.refloat),
        (CatalogRepo::Bldc, &roots.bldc),
        (CatalogRepo::Poc, &roots.poc),
    ] {
        if !path.is_dir() {
            return Err(CatalogValidationError::MissingRepoRoot {
                var: repo.env_var(),
                path: path.display().to_string(),
            });
        }
    }

    let needs_vesc_tool = refs
        .iter()
        .any(|reference| reference.repo == CatalogRepo::VescTool);
    if needs_vesc_tool && !roots.vesc_tool.is_dir() {
        return Err(CatalogValidationError::MissingRepoRoot {
            var: CatalogRepo::VescTool.env_var(),
            path: roots.vesc_tool.display().to_string(),
        });
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
