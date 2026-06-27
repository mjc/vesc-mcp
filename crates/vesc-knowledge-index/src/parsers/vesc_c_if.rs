//! Parse `catalog/bldc/vesc_c_if.yaml` function groups into index entries.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::{Category, IndexEntry, SourceRef};

/// Relative path from the catalog root to the `vesc_c_if` catalog document.
pub const CATALOG_REL_PATH: &str = "bldc/vesc_c_if.yaml";

/// Function groups indexed for package-relevant APIs (Wave 1 filter).
pub const PACKAGE_API_GROUPS: &[&str] = &["lbm_core", "lbm_symbols", "os", "comm", "nvm"];

const HEADER_REL_PATH: &str = "lispBM/c_libs/vesc_c_if.h";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct VescCIfCatalog {
    source_repo: String,
    header: VescCIfHeader,
    function_groups: Vec<FunctionGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct VescCIfHeader {
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FunctionGroup {
    id: String,
    #[serde(default)]
    lines: Option<[u64; 2]>,
    symbols: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
}

/// Errors while parsing or validating the `vesc_c_if` catalog surface.
#[derive(Debug, Error)]
pub enum VescCIfParseError {
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
    /// A required function group id was absent from the catalog.
    #[error("missing function group `{0}` in catalog")]
    MissingGroup(String),
    /// A catalog symbol was not found in the upstream header.
    #[error("symbol `{symbol}` not found in {header_path}")]
    SymbolNotInHeader {
        /// Symbol name from the catalog.
        symbol: String,
        /// Header path that was scanned.
        header_path: String,
    },
}

/// Load package-relevant `vesc_c_if` groups from catalog YAML.
///
/// # Errors
///
/// Returns [`VescCIfParseError`] when the catalog file is missing, invalid, or
/// lacks a required function group.
pub fn parse_catalog(catalog_root: &Path) -> Result<Vec<IndexEntry>, VescCIfParseError> {
    let catalog = load_catalog(catalog_root)?;
    entries_from_catalog(&catalog)
}

/// Parse catalog entries and optionally validate symbol names against the upstream header.
///
/// When `bldc_root` is `Some`, every indexed symbol must appear in
/// `{bldc_root}/lispBM/c_libs/vesc_c_if.h`.
///
/// # Errors
///
/// Returns [`VescCIfParseError`] on catalog or header validation failure.
pub fn parse_catalog_with_header_validation(
    catalog_root: &Path,
    bldc_root: Option<&Path>,
) -> Result<Vec<IndexEntry>, VescCIfParseError> {
    let entries = parse_catalog(catalog_root)?;
    if let Some(root) = bldc_root {
        validate_against_header(&entries, root)?;
    }
    Ok(entries)
}

fn load_catalog(catalog_root: &Path) -> Result<VescCIfCatalog, VescCIfParseError> {
    let path = catalog_root.join(CATALOG_REL_PATH);
    let content = std::fs::read_to_string(&path).map_err(|source| VescCIfParseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(serde_yaml::from_str(&content)?)
}

fn entries_from_catalog(catalog: &VescCIfCatalog) -> Result<Vec<IndexEntry>, VescCIfParseError> {
    let mut entries = Vec::new();

    for group_id in PACKAGE_API_GROUPS {
        let group = catalog
            .function_groups
            .iter()
            .find(|group| group.id == *group_id)
            .ok_or_else(|| VescCIfParseError::MissingGroup((*group_id).to_string()))?;

        let line = group
            .lines
            .map_or(1, |lines| u32::try_from(lines[0]).unwrap_or(1));
        let group_notes = group.notes.as_deref().unwrap_or("");

        for symbol in &group.symbols {
            let summary = if group_notes.is_empty() {
                format!("`{symbol}` in vesc_c_if `{group_id}` group")
            } else {
                format!("{group_notes} (`{symbol}`)")
            };

            entries.push(IndexEntry {
                id: format!("vesc_c_if.{symbol}"),
                name: symbol.clone(),
                category: Category::FirmwareApi,
                summary,
                source: SourceRef {
                    repo: catalog.source_repo.clone(),
                    path: catalog.header.path.clone(),
                    line,
                },
                keywords: vec!["vesc_c_if".into(), (*group_id).into(), symbol.clone()],
            });
        }
    }

    Ok(entries)
}

fn validate_against_header(
    entries: &[IndexEntry],
    bldc_root: &Path,
) -> Result<(), VescCIfParseError> {
    let header_path = bldc_root.join(HEADER_REL_PATH);
    let content =
        std::fs::read_to_string(&header_path).map_err(|source| VescCIfParseError::Io {
            path: header_path.display().to_string(),
            source,
        })?;

    for entry in entries {
        if !content.contains(&entry.name) {
            return Err(VescCIfParseError::SymbolNotInHeader {
                symbol: entry.name.clone(),
                header_path: header_path.display().to_string(),
            });
        }
    }

    Ok(())
}
