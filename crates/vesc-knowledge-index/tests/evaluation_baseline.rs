use std::collections::BTreeSet;

use vesc_knowledge_index::embedded_entries;
use vesc_knowledge_index::evaluation::{EvaluationQuery, Intent, evaluate_suite};

fn fixture() -> Vec<EvaluationQuery> {
    serde_json::from_str(include_str!("../../../tests/evaluation/v1/queries.json"))
        .expect("valid v1 evaluation fixture")
}

#[test]
fn v1_fixture_has_representative_intent_coverage() {
    let queries = fixture();
    let exact = queries
        .iter()
        .filter(|query| query.intent == Intent::Identifier)
        .count();
    let conceptual = queries
        .iter()
        .filter(|query| query.intent == Intent::Concept)
        .count();

    assert!(queries.len() >= 50);
    assert!(exact >= 15);
    assert!(conceptual >= 15);
}

#[test]
fn v1_fixture_references_existing_legacy_entries() {
    let ids: BTreeSet<_> = embedded_entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    for query in fixture() {
        for id in query.relevant.keys() {
            assert!(ids.contains(id.as_str()), "unknown fixture id {id}");
        }
    }
}

#[test]
fn legacy_baseline_report_is_deterministic() {
    let queries = fixture();
    let report = evaluate_suite(&queries, |text| {
        vesc_knowledge_index::search_knowledge(text, None, 50)
            .into_iter()
            .map(|hit| hit.id)
            .collect()
    });
    let first = serde_json::to_vec(&report).expect("serialize report");
    let second = serde_json::to_vec(&report).expect("serialize report");

    assert_eq!(first, second);
    assert_eq!(report.query_count, queries.len());
}
