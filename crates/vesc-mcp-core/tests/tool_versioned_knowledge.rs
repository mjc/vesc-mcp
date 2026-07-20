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
        .expect("versioned resource URI")
        .to_owned();
    assert!(harness.read_resource(&uri).contains("alphaunique"));

    assert_eq!(
        fs::read_dir(fixture.data_root().join("artifacts"))
            .expect("artifact directory")
            .count(),
        1
    );

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
    assert_eq!(
        unversioned["index"]["repositories"]["bldc"],
        fixture.tagged_commit()
    );
    assert!(
        unversioned["results"][0]["resource_uri"]
            .as_str()
            .is_some_and(|uri| uri.starts_with("vesc://knowledge/chunk/"))
    );
    assert!(harness.read_resource(&uri).contains("alphaunique"));
    assert_eq!(
        fs::read_dir(fixture.data_root().join("artifacts"))
            .expect("artifact directory")
            .count(),
        2
    );
}

#[tokio::test]
async fn preparation_errors_are_structured_and_actionable() {
    let fixture = VersionedKnowledgeFixture::new().await;
    let harness = McpTestHarness::with_knowledge_config(fixture.knowledge().clone());
    for (selection, expected) in [
        (
            json!({"sources": {"unknown": "refs/heads/main"}}),
            "unknown_repository",
        ),
        (
            json!({"sources": {"bldc": "refs/tags/missing"}}),
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
        ("repositories/bldc.git", "build_failed"),
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
