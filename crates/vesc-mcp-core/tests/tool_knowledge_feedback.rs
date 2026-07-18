//! Integration tests for durable learned notes and evidence-backed corrections.

use serde_json::Value;
use vesc_mcp_core::{
    config::KnowledgeConfig,
    resources::ResourceRegistry,
    tools::{
        knowledge_feedback::{
            CorrectVescKnowledgeParams, FeedbackStore, SubmitKnowledgeFeedbackParams,
            correct_vesc_knowledge_tool_with_store, submit_vesc_knowledge_feedback_with_store,
        },
        search_knowledge::{SearchVescKnowledgeParams, search_vesc_knowledge_json_with_feedback},
    },
};

use vesc_mcp_core::tools::knowledge_feedback::CorrectionAuthorization;

fn learned_note() -> SubmitKnowledgeFeedbackParams {
    SubmitKnowledgeFeedbackParams {
        question: "What does load-native-lib receive?".into(),
        lesson: "The loader receives the import tag bound to embedded native bytes.".into(),
        related_queries: vec!["native loader import tag".into()],
        identifiers: vec!["load-native-lib".into()],
        tags: vec!["loader".into()],
        source_references: Vec::new(),
        supersedes: None,
    }
}

#[test]
fn correction_requires_user_authorization() {
    let params = serde_json::json!({
        "question": "What does load-native-lib receive?",
        "mistaken_conclusion": "It receives a source path.",
        "correction": "It receives an import tag.",
        "evidence_resources": ["vesc://catalog/doc/topic/lisp_imports"]
    });

    assert!(serde_json::from_value::<CorrectVescKnowledgeParams>(params).is_err());
}

#[test]
fn submitted_note_is_idempotent_and_survives_reopen() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());

    let first = submit_vesc_knowledge_feedback_with_store(&learned_note(), &store);
    let mut equivalent = learned_note();
    equivalent.question = format!("  {}  ", equivalent.question);
    equivalent.related_queries[0] = format!(" {} ", equivalent.related_queries[0]);
    let duplicate = submit_vesc_knowledge_feedback_with_store(&equivalent, &store);
    let reopened = FeedbackStore::new(temp.path());
    let persisted = reopened
        .get(first.id.as_deref().expect("feedback id"))
        .expect("read store")
        .expect("persisted feedback");

    assert!(first.ok);
    assert!(!first.duplicate);
    assert_eq!(duplicate.id, first.id);
    assert!(duplicate.duplicate);
    assert_eq!(persisted.id(), first.id.as_deref().expect("feedback id"));
}

#[test]
fn persisted_feedback_is_readable_by_resource_uri() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());
    let response = submit_vesc_knowledge_feedback_with_store(&learned_note(), &store);
    let id = response.id.expect("feedback id");

    let mut resources = ResourceRegistry::with_defaults().expect("resource registry");
    resources.register_handler(vesc_mcp_core::resources::FeedbackResourceHandler::new(
        store,
    ));
    let body = resources
        .read(&format!("vesc://knowledge/feedback/{id}"))
        .expect("read feedback resource");
    assert!(body.contains("import tag bound to embedded native bytes"));
}

#[test]
fn correction_requires_and_digests_registered_vesc_resources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let params = CorrectVescKnowledgeParams {
        question: "Does load-native-lib read a source path?".into(),
        authorization: CorrectionAuthorization::ExplicitUserRequest,
        mistaken_conclusion: "It receives the source .bin path directly.".into(),
        correction: "The package import table binds a tag to embedded bytes; load-native-lib receives that tag.".into(),
        qualifiers: vec!["The source path is used while the package is built.".into()],
        affected_resources: vec!["vesc://catalog/doc/topic/vescpackage_reference".into()],
        evidence_resources: vec!["vesc://catalog/doc/topic/lisp_imports".into()],
        related_queries: vec!["native loader path versus tag".into()],
        identifiers: vec!["load-native-lib".into(), "lispData".into()],
        tags: vec!["loader".into()],
        supersedes: None,
    };

    let response = correct_vesc_knowledge_tool_with_store(&params, &store, &resources);

    assert!(response.ok, "{response:?}");
    assert_eq!(response.state.as_deref(), Some("resource_backed"));
    assert_eq!(response.evidence.len(), 1);
    assert!(response.evidence[0].content_digest.starts_with("sha256:"));
}

#[test]
fn related_search_returns_correction_before_results() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let correction = CorrectVescKnowledgeParams {
        question: "Does load-native-lib read a source path?".into(),
        authorization: CorrectionAuthorization::ConfirmedAfterPrompt,
        mistaken_conclusion: "It receives the source .bin path directly.".into(),
        correction: "load-native-lib receives an import tag that resolves to embedded bytes."
            .into(),
        qualifiers: Vec::new(),
        affected_resources: Vec::new(),
        evidence_resources: vec!["vesc://catalog/doc/topic/lisp_imports".into()],
        related_queries: vec!["how native loader tags work".into()],
        identifiers: vec!["load-native-lib".into()],
        tags: vec!["loader".into()],
        supersedes: None,
    };
    assert!(correct_vesc_knowledge_tool_with_store(&correction, &store, &resources).ok);

    let json = search_vesc_knowledge_json_with_feedback(
        &SearchVescKnowledgeParams {
            query: "native loader tag".into(),
            category: None,
            limit: 10,
            mode: None,
            filters: vesc_mcp_core::tools::search_knowledge::SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: vesc_mcp_core::tools::search_knowledge::SearchResponseDetail::default(),
        },
        &KnowledgeConfig::default(),
        Some(&store),
        &resources,
    );
    let body: Value = serde_json::from_str(&json).expect("search response JSON");

    assert_eq!(body["ok"], true);
    assert_eq!(body["corrections"][0]["state"], "resource_backed");
    assert!(
        body["corrections"][0]["correction"]
            .as_str()
            .is_some_and(|text| text.contains("import tag"))
    );
}

#[test]
fn affected_search_hit_references_the_correction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let params = SearchVescKnowledgeParams {
        query: "lisp imports load-native-lib".into(),
        category: None,
        limit: 10,
        mode: None,
        filters: vesc_mcp_core::tools::search_knowledge::SearchVescKnowledgeFilters::default(),
        max_response_bytes: None,
        max_context_bytes: None,
        detail: vesc_mcp_core::tools::search_knowledge::SearchResponseDetail::Full,
    };
    let baseline = search_vesc_knowledge_json_with_feedback(
        &params,
        &KnowledgeConfig::default(),
        None,
        &resources,
    );
    let baseline: Value = serde_json::from_str(&baseline).expect("baseline JSON");
    let affected = baseline["results"][0]["resource_uri"]
        .as_str()
        .expect("search result resource URI")
        .to_owned();
    let correction = CorrectVescKnowledgeParams {
        question: "What does load-native-lib receive?".into(),
        authorization: CorrectionAuthorization::ExplicitUserRequest,
        mistaken_conclusion: "It receives a source path.".into(),
        correction: "It receives an import tag resolving to embedded bytes.".into(),
        qualifiers: Vec::new(),
        affected_resources: vec![affected.clone()],
        evidence_resources: vec!["vesc://catalog/doc/topic/lisp_imports".into()],
        related_queries: vec![params.query.clone()],
        identifiers: vec!["load-native-lib".into()],
        tags: vec!["loader".into()],
        supersedes: None,
    };
    let write = correct_vesc_knowledge_tool_with_store(&correction, &store, &resources);
    let correction_id = write.id.expect("correction id");

    let json = search_vesc_knowledge_json_with_feedback(
        &params,
        &KnowledgeConfig::default(),
        Some(&store),
        &resources,
    );
    let body: Value = serde_json::from_str(&json).expect("augmented JSON");
    let hit = body["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["resource_uri"] == affected)
        .expect("affected result");
    assert!(
        hit["correction_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == &correction_id))
    );
}
