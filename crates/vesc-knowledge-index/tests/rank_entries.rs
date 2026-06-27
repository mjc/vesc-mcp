//! Ranking tests for knowledge index search.

use vesc_knowledge_index::{Category, IndexEntry, ScoredEntry, SourceRef, rank_entries};

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
fn rank_exact_symbol_first() {
    let entries = vec![
        sample_entry("lbm_add_extension_helper", "helper for extensions", &[]),
        sample_entry("lbm_add_extension", "Primary extension registration", &[]),
        sample_entry("other_symbol", "See lbm_add_extension for details", &[]),
    ];

    let results = rank_entries("lbm_add_extension", &entries);

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].entry.name, "lbm_add_extension");
    assert!(
        results[0].score > results[1].score,
        "exact match should outrank prefix/substring hits"
    );
    assert!(
        results[1].score > results[2].score,
        "prefix match should outrank summary-only substring hits"
    );
}

#[test]
fn rank_case_insensitive() {
    let entries = vec![
        sample_entry("NVM_WRITE", "Write bytes to non-volatile memory", &["nvm"]),
        sample_entry("unrelated", "nothing here", &[]),
    ];

    let results = rank_entries("nvm_write", &entries);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.name, "NVM_WRITE");
}

#[test]
fn rank_limits_results() {
    let entries = vec![
        sample_entry("alpha_match", "first", &[]),
        sample_entry("beta_match", "second", &[]),
        sample_entry("gamma", "no hit", &[]),
        sample_entry("delta_match", "fourth", &[]),
    ];

    let results = rank_entries("match", &entries);

    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|ScoredEntry { entry, .. }| entry.name.contains("match"))
    );
}
