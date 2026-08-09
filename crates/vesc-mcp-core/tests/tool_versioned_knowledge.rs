use std::fs;

use serde_json::{Value, json};
use vesc_mcp_core::test_support::{McpTestHarness, VersionedKnowledgeFixture};
use vesc_mcp_core::tools::prepare_knowledge::{
    PrepareVescKnowledgeParams, prepare_cached_vesc_knowledge_tool, prepare_vesc_knowledge_tool,
};

async fn assert_default_snapshot_compatibility(
    fixture: &VersionedKnowledgeFixture,
    harness: &McpTestHarness,
    old_uri: &str,
) {
    let layout = vesc_mcp_core::managed_repositories::KnowledgeDataLayout::new(
        fixture
            .knowledge()
            .data_root
            .clone()
            .expect("managed data root"),
    );
    let default = vesc_mcp_core::managed_snapshots::KnowledgeSnapshotStore::new(layout)
        .prepare_default(&fixture.knowledge().repositories)
        .await
        .expect("prepare default snapshot");
    assert_eq!(
        default.manifest.profile,
        vesc_mcp_core::managed_snapshots::SnapshotProfile::CompleteHistory
    );
    let unversioned: Value = serde_json::from_str(&harness.call_tool(
        "search_vesc_knowledge",
        json!({
            "query": "betaunique",
            "mode": "lexical",
            "detail": "full",
            "limit": 1
        }),
    ))
    .expect("unversioned search response");

    assert_eq!(
        unversioned["index"]["snapshot_id"],
        default.manifest.id.as_str()
    );
    // Search reports the exact revision recorded by the complete-history manifest.
    let manifest_bldc = default
        .manifest
        .repositories
        .iter()
        .find(|repository| repository.repository.as_str() == "bldc")
        .expect("bldc manifest repository");
    assert_eq!(
        unversioned["index"]["repositories"]["bldc"],
        manifest_bldc.commit
    );
    assert!(
        unversioned["results"][0]["resource_uri"]
            .as_str()
            .is_some_and(|uri| uri.starts_with("vesc://knowledge/chunk/"))
    );
    assert!(harness.read_resource(old_uri).contains("alphaunique"));
    assert_eq!(
        fs::read_dir(fixture.data_root().join("artifacts"))
            .expect("artifact directory")
            .count(),
        2
    );
}

#[tokio::test]
async fn bounded_mcp_symbol_search_returns_decisive_git_evidence_and_follow_up_resources() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());
    let prepared: Value = serde_json::from_str(
        &harness
            .call_tool_async(
                "prepare_vesc_knowledge",
                VersionedKnowledgeFixture::selection(),
            )
            .await,
    )
    .expect("prepare response");
    let snapshot = prepared["snapshot_id"].as_str().expect("snapshot ID");
    let response = harness.call_tool(
        "search_vesc_knowledge",
        json!({
            "query": "update_pid_position_offset|trait MotorControlBindings|impl.*MotorControlBindings|PidPosition",
            "snapshot_id": snapshot,
            "mode": "lexical",
            "detail": "compact",
            "limit": 5,
            "max_context_bytes": 8192,
            "max_response_bytes": 16384,
            "filters": { "repository": "bldc" }
        }),
    );
    assert!(
        response.len() <= 16_384,
        "bounded response was {} bytes",
        response.len()
    );
    let body: Value = serde_json::from_str(&response).expect("search response");
    assert_eq!(body["ok"], true, "response: {body}");
    assert_eq!(body["snapshot_id"], snapshot);
    let results = body["results"].as_array().expect("compact results");
    assert!(!results.is_empty(), "response: {body}");
    assert!(results.len() <= 5);

    let mut found_declaration = false;
    let mut found_implementation_or_caller = false;
    let mut found_filtered_repository = false;
    for row in results {
        let excerpt = row[2].as_str().expect("compact excerpt");
        found_declaration |= excerpt.contains("trait MotorControlBindings")
            || excerpt.contains("fn update_pid_position_offset");
        found_implementation_or_caller |= excerpt.contains("impl MotorControlBindings")
            || excerpt.contains("bindings.update_pid_position_offset");
        assert!(excerpt.contains("PidPosition"), "excerpt: {excerpt}");
        let source_index = usize::try_from(row[3].as_u64().expect("source index"))
            .expect("source index fits usize");
        let source = body["sources"][source_index]
            .as_str()
            .expect("source provenance");
        assert!(source.ends_with(":motor.rs:1"), "response: {body}");
        let chunk_id = row[4].as_str().expect("chunk ID");
        let chunk_uri = format!("vesc://knowledge/snapshot/{snapshot}/chunk/{chunk_id}");
        let chunk: Value = serde_json::from_str(&harness.read_resource(&chunk_uri))
            .expect("chunk follow-up resource");
        assert_eq!(chunk["chunk_id"], chunk_id);
        assert_eq!(chunk["repository"], "bldc", "chunk: {chunk}");
        found_filtered_repository |= chunk["repository"] == "bldc";
        assert_eq!(chunk["path"], "motor.rs");
        assert_eq!(chunk["revision"], fixture.old_commit());
        assert_eq!(
            chunk["source_span"],
            json!({
                "start_line": 1,
                "end_line": 13,
                "start_byte": 0,
                "end_byte": 432
            })
        );
        let document_id = chunk["document_id"].as_str().expect("document ID");
        let document_uri = format!("vesc://knowledge/snapshot/{snapshot}/document/{document_id}");
        let document: Value = serde_json::from_str(&harness.read_resource(&document_uri))
            .expect("document follow-up resource");
        assert_eq!(document["document_id"], document_id);
        assert!(
            document["text"]
                .as_str()
                .is_some_and(|content| content.contains("trait MotorControlBindings"))
        );
    }
    assert!(found_declaration, "declaration missing: {body}");
    assert!(
        found_implementation_or_caller,
        "implementation/caller missing: {body}"
    );
    assert!(
        found_filtered_repository,
        "filtered repository missing: {body}"
    );
}

#[tokio::test]
async fn unversioned_symbol_search_prefers_default_revision_and_reports_occurrences() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());
    let prepared: Value = serde_json::from_str(
        &harness
            .call_tool_async(
                "prepare_vesc_knowledge",
                VersionedKnowledgeFixture::selection(),
            )
            .await,
    )
    .expect("prepare response");
    assert_eq!(prepared["ok"], true, "prepare response: {prepared}");
    let layout = vesc_mcp_core::managed_repositories::KnowledgeDataLayout::new(
        fixture
            .knowledge()
            .data_root
            .clone()
            .expect("managed data root"),
    );
    vesc_mcp_core::managed_snapshots::KnowledgeSnapshotStore::new(layout)
        .prepare_default(&fixture.knowledge().repositories)
        .await
        .expect("prepare default snapshot");

    let body: Value = serde_json::from_str(&harness.call_tool(
        "search_vesc_knowledge",
        json!({
            "query": "update_pid_position_offset",
            "mode": "lexical",
            "detail": "full",
            "limit": 10,
            "filters": { "repository": "bldc" }
        }),
    ))
    .expect("search response");
    assert_eq!(body["ok"], true, "response: {body}");
    let results = body["results"].as_array().expect("results");
    assert!(!results.is_empty(), "response: {body}");
    if let Some(occurrence) = results.iter().find_map(|result| result.get("occurrence")) {
        assert!(occurrence["count"].as_u64().unwrap_or_default() >= 2);
        assert!(occurrence["revisions"].as_array().is_some_and(|revisions| {
            revisions
                .iter()
                .any(|revision| revision == fixture.old_commit())
        }));
    }
    let normalized_passages = results
        .iter()
        .filter_map(|result| result["passage"].as_str())
        .map(|passage| passage.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        normalized_passages.len(),
        results.len(),
        "duplicate rows: {body}"
    );
}

#[tokio::test]
async fn unversioned_refloat_c_symbol_collapses_unchanged_history() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());
    let layout = vesc_mcp_core::managed_repositories::KnowledgeDataLayout::new(
        fixture
            .knowledge()
            .data_root
            .clone()
            .expect("managed data root"),
    );
    let default = vesc_mcp_core::managed_snapshots::KnowledgeSnapshotStore::new(layout)
        .prepare_default(&fixture.knowledge().repositories)
        .await
        .expect("prepare default snapshot");
    let body: Value = serde_json::from_str(&harness.call_tool(
        "search_vesc_knowledge",
        json!({
            "query": "lbm_add_extension",
            "snapshot_id": default.manifest.id,
            "mode": "lexical",
            "detail": "full",
            "limit": 10,
            "filters": { "repository": "bldc" }
        }),
    ))
    .expect("search response");
    assert_eq!(body["ok"], true, "response: {body}");
    let results = body["results"].as_array().expect("results");
    let c_results = results
        .iter()
        .filter(|result| result["source"]["path"] == "vesc_c_if.c")
        .collect::<Vec<_>>();
    assert_eq!(c_results.len(), 1, "C evidence was not collapsed: {body}");
    if let Some(occurrence) = c_results[0]["occurrence"].as_object() {
        assert!(occurrence["count"].as_u64().unwrap_or_default() >= 2);
        assert!(occurrence["revisions"].as_array().is_some_and(|revisions| {
            revisions
                .iter()
                .any(|revision| revision == fixture.old_commit())
        }));
    }
    assert!(
        c_results[0]["passage"]
            .as_str()
            .is_some_and(|passage| passage.contains("lbm_add_extension"))
    );
}

#[tokio::test]
async fn agent_can_list_prepare_search_and_read_an_explicit_snapshot() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());

    let listed: Value = serde_json::from_str(&harness.call_tool(
        "list_vesc_source_versions",
        json!({"ref_kinds": ["branch", "tag"], "limit": 20}),
    ))
    .expect("list response");
    assert!(listed["ok"].as_bool().unwrap_or_default());

    let first: Value = serde_json::from_str(
        &harness
            .call_tool_async(
                "prepare_vesc_knowledge",
                VersionedKnowledgeFixture::selection(),
            )
            .await,
    )
    .expect("prepare response");
    assert_eq!(first["status"], "built");
    assert_eq!(first["sources"]["bldc"], fixture.old_commit());
    assert_eq!(first["sources"]["vesc_tool"], fixture.old_commit());
    assert_eq!(first["sources"]["refloat"], fixture.tagged_commit());
    let snapshot = first["snapshot_id"].as_str().expect("snapshot ID");

    let second: Value = serde_json::from_str(
        &harness
            .call_tool_async(
                "prepare_vesc_knowledge",
                VersionedKnowledgeFixture::selection(),
            )
            .await,
    )
    .expect("repeat prepare response");
    assert_eq!(second["snapshot_id"], snapshot);
    assert_eq!(second["status"], "reused");

    let search: Value = serde_json::from_str(&harness.call_tool(
        "search_vesc_knowledge",
        json!({
            "query": "alphaunique",
            "snapshot_id": snapshot,
            "mode": "lexical",
            "detail": "full",
            "limit": 1
        }),
    ))
    .expect("search response");
    assert_eq!(search["index"]["snapshot_id"], snapshot);
    assert_eq!(
        search["index"]["repositories"]["bldc"],
        fixture.old_commit()
    );
    let uri = search["results"][0]["resource_uri"]
        .as_str()
        .expect("versioned resource URI")
        .to_owned();
    assert!(harness.read_resource(&uri).contains("alphaunique"));
    assert_eq!(
        fs::read_dir(fixture.data_root().join("artifacts"))
            .expect("artifact directory")
            .count(),
        1
    );

    assert_default_snapshot_compatibility(&fixture, &harness, &uri).await;
}

#[tokio::test]
async fn preparation_errors_are_structured_and_actionable() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let mut unmanaged = fixture.knowledge().clone();
    unmanaged.managed_git = false;
    let direct =
        prepare_vesc_knowledge_tool(&PrepareVescKnowledgeParams::default(), &unmanaged).await;
    assert_eq!(
        direct.error.as_ref().map(|error| error.code),
        Some("not_configured")
    );
    let unmanaged_harness = McpTestHarness::with_knowledge_config(unmanaged);
    let transported: Value = serde_json::from_str(
        &unmanaged_harness
            .call_tool_async("prepare_vesc_knowledge", json!({}))
            .await,
    )
    .expect("disabled managed Git response");
    assert_eq!(transported["ok"], false);
    assert_eq!(transported["error"]["code"], "not_configured");

    let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());
    for (selection, expected) in [
        (
            json!({"sources": {"unknown": "refs/heads/main"}}),
            "unknown_repository",
        ),
        (
            json!({"sources": {"../bldc": "refs/heads/main"}}),
            "unknown_repository",
        ),
        (
            json!({"sources": {"bldc": "refs/tags/missing"}}),
            "unknown_ref",
        ),
        (
            json!({"sources": {"bldc": "https://example.invalid/repository.git"}}),
            "unknown_ref",
        ),
        (
            json!({"sources": {"bldc": "/tmp/unmanaged-repository"}}),
            "unknown_ref",
        ),
        (
            json!({"sources": {"bldc": "ffffffffffffffffffffffffffffffffffffffff"}}),
            "unreachable_commit",
        ),
        (
            json!({
                "sources": {"bldc": "refs/heads/release_6_06"},
                "timeout_secs": 0
            }),
            "timeout",
        ),
    ] {
        let response: Value = serde_json::from_str(
            &harness
                .call_tool_async("prepare_vesc_knowledge", selection)
                .await,
        )
        .expect("prepare error response");
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], expected);
        assert!(
            response["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("list_vesc_source_versions"))
        );
    }
}

#[tokio::test]
async fn stale_managed_source_paths_have_stable_errors() {
    for (relative, expected) in [
        ("repositories/bldc.refs.json", "source_unavailable"),
        ("repositories/bldc.git", "source_unavailable"),
    ] {
        let fixture = VersionedKnowledgeFixture::new().await;
        let target = fixture.data_root().join(relative);
        if target.is_dir() {
            fs::remove_dir_all(&target).expect("remove managed repository");
        } else {
            fs::remove_file(&target).expect("remove managed ref catalog");
        }
        let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());

        let response: Value = serde_json::from_str(
            &harness
                .call_tool_async(
                    "prepare_vesc_knowledge",
                    VersionedKnowledgeFixture::selection(),
                )
                .await,
        )
        .expect("prepare error response");

        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], expected);
        assert!(
            response["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("list_vesc_source_versions"))
        );
    }
}

#[tokio::test]
async fn cached_only_preparation_does_not_build_a_missing_snapshot() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let params =
        serde_json::from_value(VersionedKnowledgeFixture::selection()).expect("snapshot selection");

    let response = prepare_cached_vesc_knowledge_tool(&params, fixture.knowledge()).await;

    assert_eq!(
        response.error.as_ref().map(|error| error.code),
        Some("not_cached")
    );
    assert!(!fixture.data_root().join("artifacts").exists());
}

#[tokio::test]
async fn cached_only_preparation_reuses_a_complete_snapshot() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let params =
        serde_json::from_value(VersionedKnowledgeFixture::selection()).expect("snapshot selection");
    let built = prepare_vesc_knowledge_tool(&params, fixture.knowledge()).await;

    let reused = prepare_cached_vesc_knowledge_tool(&params, fixture.knowledge()).await;

    assert_eq!(reused.snapshot_id, built.snapshot_id);
    assert_eq!(reused.status, Some("reused"));
}
