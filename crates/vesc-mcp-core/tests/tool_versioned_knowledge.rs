#![cfg(feature = "managed-git")]

use std::fs;

use serde_json::{Value, json};
use vesc_mcp_core::test_support::{McpTestHarness, VersionedKnowledgeFixture};

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
        .expect("versioned resource URI");
    assert!(harness.read_resource(uri).contains("alphaunique"));

    assert_eq!(
        fs::read_dir(fixture.data_root().join("artifacts"))
            .expect("artifact directory")
            .count(),
        1
    );
}
