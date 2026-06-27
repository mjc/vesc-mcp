use crate::IndexEntry;

/// One index entry with a relevance score (higher is better).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredEntry {
    pub entry: IndexEntry,
    pub score: u32,
}

/// Rank index entries by relevance to `query`.
///
/// Scoring (case-insensitive): exact name > name prefix > substring in name,
/// keywords, or summary. Non-matching entries are omitted.
#[must_use]
pub fn rank_entries(query: &str, entries: &[IndexEntry]) -> Vec<ScoredEntry> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let query_lower = query.to_ascii_lowercase();
    let mut scored: Vec<(usize, ScoredEntry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            score_entry(&query_lower, entry).map(|score| {
                (
                    index,
                    ScoredEntry {
                        entry: entry.clone(),
                        score,
                    },
                )
            })
        })
        .collect();

    scored.sort_by(|(left_index, left), (right_index, right)| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left_index.cmp(right_index))
    });

    scored.into_iter().map(|(_, scored)| scored).collect()
}

fn score_entry(query_lower: &str, entry: &IndexEntry) -> Option<u32> {
    let name_lower = entry.name.to_ascii_lowercase();

    if name_lower == query_lower {
        return Some(SCORE_EXACT_NAME);
    }
    if name_lower.starts_with(query_lower) {
        return Some(SCORE_NAME_PREFIX);
    }
    if name_lower.contains(query_lower) {
        return Some(SCORE_NAME_SUBSTRING);
    }

    if entry
        .keywords
        .iter()
        .any(|keyword| keyword.to_ascii_lowercase().contains(query_lower))
    {
        return Some(SCORE_KEYWORD_SUBSTRING);
    }

    if entry.summary.to_ascii_lowercase().contains(query_lower) {
        return Some(SCORE_SUMMARY_SUBSTRING);
    }

    None
}

const SCORE_EXACT_NAME: u32 = 1_000;
const SCORE_NAME_PREFIX: u32 = 900;
const SCORE_NAME_SUBSTRING: u32 = 800;
const SCORE_KEYWORD_SUBSTRING: u32 = 700;
const SCORE_SUMMARY_SUBSTRING: u32 = 600;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, SourceRef};

    fn sample_entry(name: &str, summary: &str, keywords: &[&str]) -> IndexEntry {
        IndexEntry {
            id: format!("test.{name}"),
            name: name.into(),
            category: Category::FirmwareApi,
            summary: summary.into(),
            source: SourceRef {
                repo: "test".into(),
                path: "test.rs".into(),
                line: 1,
            },
            keywords: keywords.iter().map(|keyword| (*keyword).into()).collect(),
        }
    }

    #[test]
    fn empty_query_returns_no_results() {
        let entries = vec![sample_entry("foo", "bar", &[])];
        assert!(rank_entries("", &entries).is_empty());
        assert!(rank_entries("   ", &entries).is_empty());
    }
}
