//! Parse `catalog/priorities.json` into searchable package-build and gap entries.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::{Category, IndexEntry, SourceRef};

/// Relative path from the catalog root to the priorities document.
pub const CATALOG_REL_PATH: &str = "priorities.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct PrioritiesDoc {
    priorities: Vec<PriorityRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct PriorityRow {
    id: String,
    category: Category,
    catalog_ref: String,
    summary: String,
}

/// Errors while parsing catalog priorities.
#[derive(Debug, Error)]
pub enum PrioritiesParseError {
    /// Failed to read priorities.json.
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Invalid priorities JSON.
    #[error("parse priorities.json: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Parse priority rows from `catalog/priorities.json`.
///
/// # Errors
///
/// Returns [`PrioritiesParseError`] when the file is missing or invalid.
pub fn parse_catalog(catalog_root: &Path) -> Result<Vec<IndexEntry>, PrioritiesParseError> {
    let path = catalog_root.join(CATALOG_REL_PATH);
    let raw = std::fs::read_to_string(&path).map_err(|source| PrioritiesParseError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let doc: PrioritiesDoc = serde_json::from_str(&raw)?;
    Ok(doc
        .priorities
        .into_iter()
        .map(|row| IndexEntry {
            id: format!("priority.{}", row.id),
            name: row.id.replace('-', "_"),
            category: row.category,
            summary: row.summary.clone(),
            source: SourceRef {
                repo: "catalog".into(),
                path: row.catalog_ref,
                line: 1,
            },
            keywords: priority_keywords(&row.id, &row.summary),
        })
        .collect())
}

fn priority_keywords(id: &str, summary: &str) -> Vec<String> {
    let mut keywords: Vec<String> = id
        .split('-')
        .map(str::to_string)
        .chain(summary.split_whitespace().map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        }))
        .filter(|word| !word.is_empty())
        .collect();
    keywords.sort_unstable();
    keywords.dedup();
    keywords
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn parse_missing_priorities_returns_io_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = parse_catalog(temp.path()).expect_err("missing file");
        assert!(matches!(err, PrioritiesParseError::Io { .. }));
    }

    #[test]
    fn parse_invalid_json_returns_parse_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join(CATALOG_REL_PATH), "{not json").expect("write");
        let err = parse_catalog(temp.path()).expect_err("bad json");
        assert!(matches!(err, PrioritiesParseError::Parse(_)));
    }
}
