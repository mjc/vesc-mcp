//! Parse `catalog/abi/minimal-test-package-abi.yaml` requirements into index entries.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::{Category, IndexEntry, SourceRef};

/// Relative path from the catalog root to the minimal test package ABI document.
pub const CATALOG_REL_PATH: &str = "abi/minimal-test-package-abi.yaml";

const ABI_DOC_SOURCE_MARKER: &str = "vesc-pkg-lib-abi.md";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct NativeLibAbiCatalog {
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
pub enum NativeLibAbiParseError {
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
    /// The catalog lacks the in-repo ABI doc source reference.
    #[error("missing vesc-pkg-lib-abi.md source in catalog")]
    MissingAbiDocSource,
    /// A catalog symbol was not found in the primary ABI doc source.
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
/// Returns [`NativeLibAbiParseError`] when the catalog file is missing or invalid.
pub fn parse_catalog(catalog_root: &Path) -> Result<Vec<IndexEntry>, NativeLibAbiParseError> {
    let catalog = load_catalog(catalog_root)?;
    entries_from_catalog(&catalog)
}

/// Parse catalog entries and optionally validate symbol names against the in-repo doc.
///
/// When `repo_root` is `Some`, every indexed symbol must appear in the primary
/// `docs/vesc-pkg-lib-abi.md` source file under that checkout.
///
/// # Errors
///
/// Returns [`NativeLibAbiParseError`] on catalog or source validation failure.
pub fn parse_catalog_with_source_validation(
    catalog_root: &Path,
    repo_root: Option<&Path>,
) -> Result<Vec<IndexEntry>, NativeLibAbiParseError> {
    let catalog = load_catalog(catalog_root)?;
    let entries = entries_from_catalog(&catalog)?;
    if let Some(root) = repo_root {
        validate_against_source(&catalog, &entries, root)?;
    }
    Ok(entries)
}

fn load_catalog(catalog_root: &Path) -> Result<NativeLibAbiCatalog, NativeLibAbiParseError> {
    let path = catalog_root.join(CATALOG_REL_PATH);
    let content = std::fs::read_to_string(&path).map_err(|source| NativeLibAbiParseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(serde_yaml::from_str(&content)?)
}

fn primary_abi_source(catalog: &NativeLibAbiCatalog) -> Result<&AbiSource, NativeLibAbiParseError> {
    catalog
        .sources
        .iter()
        .find(|source| source.path.contains(ABI_DOC_SOURCE_MARKER))
        .ok_or(NativeLibAbiParseError::MissingAbiDocSource)
}

fn entries_from_catalog(
    catalog: &NativeLibAbiCatalog,
) -> Result<Vec<IndexEntry>, NativeLibAbiParseError> {
    let source = primary_abi_source(catalog)?;
    let line = source
        .lines
        .map_or(1, |lines| u32::try_from(lines[0]).unwrap_or(1));

    let entries = catalog
        .requirements
        .iter()
        .map(|requirement| IndexEntry {
            id: format!("native_lib_abi.{}", requirement.name),
            name: requirement.name.clone(),
            category: Category::NativeLibAbi,
            summary: requirement.caller.clone(),
            source: SourceRef {
                repo: catalog.source_repo.clone(),
                path: source.path.clone(),
                line,
            },
            keywords: vec![
                "native_lib_abi".into(),
                catalog.package_id.clone(),
                requirement.kind.clone(),
                requirement.name.clone(),
            ],
        })
        .collect();

    Ok(entries)
}

fn validate_against_source(
    catalog: &NativeLibAbiCatalog,
    entries: &[IndexEntry],
    repo_root: &Path,
) -> Result<(), NativeLibAbiParseError> {
    let source = primary_abi_source(catalog)?;
    let source_path = repo_root.join(&source.path);
    let content =
        std::fs::read_to_string(&source_path).map_err(|source| NativeLibAbiParseError::Io {
            path: source_path.display().to_string(),
            source,
        })?;

    for entry in entries {
        let needle = symbol_search_token(&entry.name);
        if !content.contains(needle) {
            return Err(NativeLibAbiParseError::SymbolNotInSource {
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
