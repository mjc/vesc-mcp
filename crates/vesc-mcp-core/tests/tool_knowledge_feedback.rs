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

fn loader_correction(authorization: CorrectionAuthorization) -> CorrectVescKnowledgeParams {
    CorrectVescKnowledgeParams {
        question: "What does load-native-lib receive?".into(),
        authorization,
        mistaken_conclusion: "It receives a source path.".into(),
        correction: "It receives an import tag resolving to embedded bytes.".into(),
        reasoning_failure:
            "A generic file-loading analogy was treated as authoritative before checking the package import contract."
                .into(),
        gap_diagnoses: vec![
            vesc_mcp_core::tools::knowledge_feedback::GapDiagnosis::RetrievedButNotSalient,
        ],
        retrieval_trace: vesc_mcp_core::tools::knowledge_feedback::RetrievalTrace {
            query: "lisp imports load-native-lib".into(),
            mode: Some("lexical".into()),
            limit: 10,
            max_response_bytes: Some(32_768),
            max_context_bytes: Some(8_192),
            filters: Vec::new(),
            results: Vec::new(),
            decisive_evidence: vec!["vesc://catalog/doc/topic/lisp_imports".into()],
            distractors: vec!["generic native loader examples".into()],
            insufficient_evidence_next: vec![
                "Read vesc://catalog/doc/topic/lisp_imports before inferring loader arguments."
                    .into(),
            ],
        },
        qualifiers: Vec::new(),
        affected_resources: Vec::new(),
        evidence_resources: vec!["vesc://catalog/doc/topic/lisp_imports".into()],
        project_references: Vec::new(),
        related_queries: vec!["native loader path versus tag".into()],
        identifiers: vec!["load-native-lib".into(), "lispData".into()],
        tags: vec!["loader".into()],
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
fn correction_requires_gap_diagnosis_and_retrieval_trace() {
    let error = serde_json::from_value::<CorrectVescKnowledgeParams>(serde_json::json!({
        "question": "What does load-native-lib receive?",
        "authorization": "explicit_user_request",
        "mistaken_conclusion": "It receives a source path.",
        "correction": "It receives an import tag.",
        "reasoning_failure": "The decisive evidence was not surfaced.",
        "gap_diagnoses": ["retrieved_but_not_salient"],
        "evidence_resources": ["vesc://catalog/doc/topic/lisp_imports"]
    }))
    .expect_err("retrieval trace is required");

    assert!(error.to_string().contains("retrieval_trace"), "{error}");
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
fn correction_gap_trace_survives_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let store = FeedbackStore::new(temp.path());
    let response = correct_vesc_knowledge_tool_with_store(
        &loader_correction(CorrectionAuthorization::ExplicitUserRequest),
        &store,
        &resources,
    );
    let id = response.id.expect("correction id");

    let reopened = FeedbackStore::new(temp.path());
    let record = reopened.get(&id).expect("read store").expect("correction");
    let body = serde_json::to_value(record).expect("correction JSON");

    assert_eq!(body["gap_diagnoses"][0], "retrieved_but_not_salient");
    assert_eq!(body["recommended_data_actions"][0], "emphasize_qualifier");
    assert_eq!(
        body["retrieval_trace"]["query"],
        "lisp imports load-native-lib"
    );
}

#[test]
fn correction_replay_measures_base_knowledge_without_advisory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let store = FeedbackStore::new(temp.path());
    let mut correction = loader_correction(CorrectionAuthorization::ExplicitUserRequest);
    let baseline = vesc_mcp_core::tools::search_knowledge::search_vesc_knowledge_tool_with_config(
        &SearchVescKnowledgeParams {
            query: correction.retrieval_trace.query.clone(),
            category: None,
            limit: correction.retrieval_trace.limit,
            mode: None,
            filters: vesc_mcp_core::tools::search_knowledge::SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: vesc_mcp_core::tools::search_knowledge::SearchResponseDetail::Full,
        },
        &KnowledgeConfig::default(),
    );
    let decisive_id = baseline.results[0].id.clone();
    correction.retrieval_trace.decisive_evidence = vec![decisive_id.clone()];
    let write = correct_vesc_knowledge_tool_with_store(&correction, &store, &resources);

    let report = vesc_mcp_core::tools::search_knowledge::replay_vesc_knowledge_correction(
        &vesc_mcp_core::tools::search_knowledge::ReplayVescKnowledgeCorrectionParams {
            correction_id: write.id.expect("correction id"),
            mark_covered: true,
            authorization: Some(CorrectionAuthorization::ExplicitUserRequest),
        },
        &KnowledgeConfig::default(),
        &store,
    );

    assert!(report.ok, "{report:?}");
    assert!(report.covered_by_base_knowledge, "{report:?}");
    assert!(report.marked_covered, "{report:?}");
    assert_eq!(report.matched_decisive_evidence, vec![decisive_id]);
    assert!(report.missing_decisive_evidence.is_empty());
    assert!(store.active_records().expect("active records").is_empty());
}

#[test]
fn marking_base_coverage_requires_user_authorization() {
    let temp = tempfile::tempdir().expect("tempdir");
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let store = FeedbackStore::new(temp.path());
    let write = correct_vesc_knowledge_tool_with_store(
        &loader_correction(CorrectionAuthorization::ExplicitUserRequest),
        &store,
        &resources,
    );

    let report = vesc_mcp_core::tools::search_knowledge::replay_vesc_knowledge_correction(
        &vesc_mcp_core::tools::search_knowledge::ReplayVescKnowledgeCorrectionParams {
            correction_id: write.id.expect("correction id"),
            mark_covered: true,
            authorization: None,
        },
        &KnowledgeConfig::default(),
        &store,
    );

    assert!(!report.ok);
    assert!(
        report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("authorization"))
    );
}

#[test]
fn correction_requires_and_digests_registered_vesc_resources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let mut params = loader_correction(CorrectionAuthorization::ExplicitUserRequest);
    params.question = "Does load-native-lib read a source path?".into();
    params.qualifiers = vec!["The source path is used while the package is built.".into()];
    params.affected_resources = vec!["vesc://catalog/doc/topic/vescpackage_reference".into()];

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
    let correction = loader_correction(CorrectionAuthorization::ConfirmedAfterPrompt);
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
    assert_eq!(body["corrections"][0]["state"], "resource_backed_gap");
    assert!(
        body["corrections"][0]["what_we_know"]
            .as_str()
            .is_some_and(|text| text.contains("import tag"))
    );
    assert_eq!(
        body["corrections"][0]["gap_diagnoses"][0],
        "retrieved_but_not_salient"
    );
    assert!(
        body["corrections"][0]["check_next"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
}

#[test]
fn unrelated_search_does_not_surface_loader_advisory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FeedbackStore::new(temp.path());
    let resources = ResourceRegistry::with_defaults().expect("resource registry");
    let correction = loader_correction(CorrectionAuthorization::ExplicitUserRequest);
    assert!(correct_vesc_knowledge_tool_with_store(&correction, &store, &resources).ok);

    let json = search_vesc_knowledge_json_with_feedback(
        &SearchVescKnowledgeParams {
            query: "realtime data field identifiers".into(),
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

    assert!(
        body.get("corrections")
            .is_none_or(|corrections| corrections.as_array().is_some_and(Vec::is_empty)),
        "{body}"
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
    let mut correction = loader_correction(CorrectionAuthorization::ExplicitUserRequest);
    correction.affected_resources = vec![affected.clone()];
    correction.related_queries = vec![params.query.clone()];
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
