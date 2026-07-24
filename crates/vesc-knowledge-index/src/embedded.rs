//! Compile-time embedded knowledge index and search helpers.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::{Category, IndexEntry, SourceRef, rank_entries};
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

/// Builds the fielded lexical index from the legacy embedded corpus once.
///
/// # Panics
///
/// Panics if the embedded generated corpus violates the migration contract or
/// Tantivy cannot build its in-memory index.
#[must_use]
pub fn lexical_index() -> &'static LexicalIndex {
    LEXICAL.get_or_init(|| {
        let chunks = embedded_entries()
            .iter()
            .map(|entry| {
                NormalizedDocument::from_legacy(entry)
                    .and_then(|document| document.legacy_chunk())
                    .expect("embedded legacy entry migrates to a chunk")
            })
            .collect::<Vec<_>>();
        LexicalIndex::build(&chunks).expect("embedded lexical index builds")
    })
}

/// Searches the fielded lexical index over the embedded legacy corpus.
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

/// One ranked search hit from the embedded index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeSearchHit {
    pub id: String,
    pub name: String,
    pub category: Category,
    pub summary: String,
    pub source: SourceRef,
    pub score: u32,
}

/// Search the embedded index with optional category filter and result limit.
#[must_use]
pub fn search_knowledge(
    query: &str,
    category: Option<Category>,
    limit: usize,
) -> Vec<KnowledgeSearchHit> {
    let limit = limit.max(1);
    rank_entries(query, embedded_entries())
        .into_iter()
        .filter(|hit| category.is_none_or(|cat| hit.entry.category == cat))
        .take(limit)
        .map(|hit| KnowledgeSearchHit {
            id: hit.entry.id,
            name: hit.entry.name,
            category: hit.entry.category,
            summary: hit.entry.summary,
            source: hit.entry.source,
            score: hit.score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_entries_non_empty() {
        assert!(!embedded_entries().is_empty());
    }

    #[test]
    fn search_knowledge_respects_limit_and_category() {
        let all = search_knowledge("nvm", None, 3);
        assert!(!all.is_empty());
        assert!(all.len() <= 3);

        let filtered = search_knowledge("nvm", Some(Category::FirmwareApi), 10);
        assert!(filtered.iter().all(|h| h.category == Category::FirmwareApi));
    }

    #[test]
    fn search_knowledge_zero_limit_becomes_one() {
        let hits = search_knowledge("pkg", None, 0);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn lexical_search_preserves_exact_identifier() {
        let hits = search_lexical_knowledge("lbm_add_extension", None, 1).expect("search");
        assert_eq!(
            hits[0]
                .chunk
                .identifiers
                .iter()
                .next()
                .map(compact_str::CompactString::as_str),
            Some("lbm_add_extension")
        );
        assert!(hits[0].exact_identifier);
    }
}
