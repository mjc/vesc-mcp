//! Parse `catalog/poc/minimal-test-package-abi.yaml` requirements into index entries.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::{Category, IndexEntry, SourceRef};

/// Relative path from the catalog root to the minimal test package ABI document.
pub const CATALOG_REL_PATH: &str = "poc/minimal-test-package-abi.yaml";

const ABI_INVENTORY_SOURCE_MARKER: &str = "abi_inventory.rs";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct PocAbiCatalog {
    source_repo: String,
    package_id: String,
    sources: Vec<AbiSource>,
    requirements: Vec<AbiRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AbiSource {
    path: String,
    #[serde(default)]
    lines: Option<[u64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AbiRequirement {
    name: String,
    kind: String,
    caller: String,
}

/// Errors while parsing or validating the POC ABI catalog surface.
#[derive(Debug, Error)]
pub enum PocAbiParseError {
    /// Failed to read a file from disk.
    #[error("read {path}: {source}")]
    Io {
        /// Path that could not be read.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to deserialize catalog YAML.
    #[error("parse catalog YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    /// The catalog lacks an `abi_inventory.rs` source reference.
    #[error("missing abi_inventory source in catalog")]
    MissingAbiInventorySource,
    /// A catalog symbol was not found in the upstream abi inventory source.
    #[error("symbol `{symbol}` not found in {source_path}")]
    SymbolNotInSource {
        /// Symbol name from the catalog.
        symbol: String,
        /// Source path that was scanned.
        source_path: String,
    },
}

/// Load POC ABI requirements from catalog YAML.
///
/// # Errors
///
/// Returns [`PocAbiParseError`] when the catalog file is missing or invalid.
pub fn parse_catalog(catalog_root: &Path) -> Result<Vec<IndexEntry>, PocAbiParseError> {
    let catalog = load_catalog(catalog_root)?;
    entries_from_catalog(&catalog)
}

/// Parse catalog entries and optionally validate symbol names against the upstream source.
///
/// When `poc_root` is `Some`, every indexed symbol must appear in the primary
/// `abi_inventory.rs` source file under that checkout.
///
/// # Errors
///
/// Returns [`PocAbiParseError`] on catalog or source validation failure.
pub fn parse_catalog_with_source_validation(
    catalog_root: &Path,
    poc_root: Option<&Path>,
) -> Result<Vec<IndexEntry>, PocAbiParseError> {
    let catalog = load_catalog(catalog_root)?;
    let entries = entries_from_catalog(&catalog)?;
    if let Some(root) = poc_root {
        validate_against_source(&catalog, &entries, root)?;
    }
    Ok(entries)
}

fn load_catalog(catalog_root: &Path) -> Result<PocAbiCatalog, PocAbiParseError> {
    let path = catalog_root.join(CATALOG_REL_PATH);
    let content = std::fs::read_to_string(&path).map_err(|source| PocAbiParseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(serde_yaml::from_str(&content)?)
}

fn primary_abi_source(catalog: &PocAbiCatalog) -> Result<&AbiSource, PocAbiParseError> {
    catalog
        .sources
        .iter()
        .find(|source| source.path.contains(ABI_INVENTORY_SOURCE_MARKER))
        .ok_or(PocAbiParseError::MissingAbiInventorySource)
}

fn entries_from_catalog(catalog: &PocAbiCatalog) -> Result<Vec<IndexEntry>, PocAbiParseError> {
    let source = primary_abi_source(catalog)?;
    let line = source
        .lines
        .map_or(1, |lines| u32::try_from(lines[0]).unwrap_or(1));

    let entries = catalog
        .requirements
        .iter()
        .map(|requirement| IndexEntry {
            id: format!("poc_abi.{}", requirement.name),
            name: requirement.name.clone(),
            category: Category::PocAbi,
            summary: requirement.caller.clone(),
            source: SourceRef {
                repo: catalog.source_repo.clone(),
                path: source.path.clone(),
                line,
            },
            keywords: vec![
                "poc_abi".into(),
                catalog.package_id.clone(),
                requirement.kind.clone(),
                requirement.name.clone(),
            ],
        })
        .collect();

    Ok(entries)
}

fn validate_against_source(
    catalog: &PocAbiCatalog,
    entries: &[IndexEntry],
    poc_root: &Path,
) -> Result<(), PocAbiParseError> {
    let source = primary_abi_source(catalog)?;
    let source_path = poc_root.join(&source.path);
    let content = std::fs::read_to_string(&source_path).map_err(|source| PocAbiParseError::Io {
        path: source_path.display().to_string(),
        source,
    })?;

    for entry in entries {
        let needle = symbol_search_token(&entry.name);
        if !content.contains(needle) {
            return Err(PocAbiParseError::SymbolNotInSource {
                symbol: entry.name.clone(),
                source_path: source_path.display().to_string(),
            });
        }
    }

    Ok(())
}

fn symbol_search_token(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}
