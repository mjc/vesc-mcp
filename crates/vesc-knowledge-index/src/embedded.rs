//! Compile-time embedded knowledge index and search helpers.

use std::sync::OnceLock;

use crate::{Category, IndexEntry};
use crate::{LexicalError, LexicalFilters, LexicalHit, LexicalIndex, NormalizedDocument};

static ENTRIES: OnceLock<Vec<IndexEntry>> = OnceLock::new();
static LEXICAL: OnceLock<LexicalIndex> = OnceLock::new();

/// Load the compile-time embedded index entries.
///
/// # Panics
///
/// Panics if the embedded JSON snapshot is invalid.
#[must_use]
pub fn embedded_entries() -> &'static [IndexEntry] {
    ENTRIES
        .get_or_init(|| {
            let json = include_str!(concat!(env!("OUT_DIR"), "/index.json"));
            serde_json::from_str(json).expect("valid embedded knowledge index json")
        })
        .as_slice()
}

/// Builds the fielded lexical index from the embedded catalog once.
///
/// # Panics
///
/// Panics if the embedded generated corpus violates the catalog contract or
/// Tantivy cannot build its in-memory index.
#[must_use]
pub fn lexical_index() -> &'static LexicalIndex {
    LEXICAL.get_or_init(|| {
        let chunks = embedded_entries()
            .iter()
            .map(|entry| {
                NormalizedDocument::from_catalog_entry(entry)
                    .and_then(|document| document.catalog_chunk())
                    .expect("embedded catalog entry converts to a chunk")
            })
            .collect::<Vec<_>>();
        LexicalIndex::build(&chunks).expect("embedded lexical index builds")
    })
}

/// Searches the fielded lexical index over the embedded catalog.
///
/// # Errors
///
/// Returns [`LexicalError`] for empty queries or Tantivy search failures.
pub fn search_lexical_knowledge(
    query: &str,
    category: Option<Category>,
    limit: usize,
) -> Result<Vec<LexicalHit>, LexicalError> {
    lexical_index().search(
        query,
        &LexicalFilters {
            category,
            ..LexicalFilters::default()
        },
        limit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_entries_non_empty() {
        assert!(!embedded_entries().is_empty());
    }

    #[test]
    fn lexical_search_preserves_exact_identifier() {
        let hits = search_lexical_knowledge("lbm_add_extension", None, 1).expect("search");
        assert_eq!(
            hits[0]
                .chunk
                .identifiers
                .first()
                .map(compact_str::CompactString::as_str),
            Some("lbm_add_extension")
        );
        assert!(hits[0].exact_identifier);
    }
}
