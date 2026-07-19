use std::collections::BTreeSet;

use vesc_knowledge_index::evaluation::{EvaluationSuite, V2_FAILURE_CATEGORIES};

fn suite() -> EvaluationSuite {
    serde_json::from_str(include_str!("../../../tests/evaluation/v2/queries.json"))
        .expect("valid v2 evaluation fixture")
}

#[test]
fn v2_fixture_is_a_locked_five_category_suite() {
    let suite = suite();
    assert_eq!(suite.schema, 2);
    assert_eq!(suite.corpus_documents, 2_875);
    assert_eq!(suite.corpus_chunks, 16_586);
    assert_eq!(suite.queries.len(), 25);
    assert_eq!(
        suite.failure_categories(),
        V2_FAILURE_CATEGORIES
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>()
    );

    let mut query_ids = BTreeSet::new();
    for query in suite.queries {
        assert!(query_ids.insert(query.id), "duplicate query ID");
        assert!(!query.text.trim().is_empty());
        assert!(query.relevant.keys().all(|id| id.starts_with("chunk-")));
        assert!(query.relevant.values().all(|grade| *grade <= 2));
    }
}
