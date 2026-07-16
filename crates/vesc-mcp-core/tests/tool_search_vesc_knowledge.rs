//! Integration test for the `search_vesc_knowledge` MCP tool.

use serde_json::Value;
use vesc_mcp_core::{VescMcpService, test_support::McpTestHarness};

#[test]
fn tool_search_lbm_add_extension() {
    let harness = McpTestHarness::new();
    let response = harness.call_tool(
        "search_vesc_knowledge",
        serde_json::json!({ "query": "lbm_add_extension" }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");

    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected at least one hit");

    let top = &results[0];
    assert_eq!(top["name"], "lbm_add_extension");
    assert_eq!(top["id"], "vesc_c_if.lbm_add_extension");
    assert_eq!(top["category"], "firmware_api");
    assert!(
        top["score"].as_u64().is_some_and(|score| score > 0),
        "entry: {top}"
    );
    assert!(
        top["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("extension")),
        "entry: {top}"
    );
    assert_eq!(top["source"]["repo"], "bldc");
    assert_eq!(top["source"]["path"], "lispBM/c_libs/vesc_c_if.h");
}

#[test]
fn lexical_mode_returns_readable_provenance_resource() {
    let harness = McpTestHarness::new();
    let response = harness.call_tool(
        "search_vesc_knowledge",
        serde_json::json!({ "query": "lbm_add_extension", "mode": "lexical" }),
    );
    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");
    assert_eq!(body["mode"], "lexical");

    let uri = body["results"][0]["provenance"]["resource_uri"]
        .as_str()
        .expect("lexical result has a resource URI");
    let chunk = VescMcpService::new()
        .resource_registry()
        .read(uri)
        .expect("search provenance resource is readable");
    let chunk: Value = serde_json::from_str(&chunk).expect("chunk resource is JSON");
    assert_eq!(
        chunk["text"],
        "Primary extension registration surface for native packages (`lbm_add_extension`)"
    );

    let document_uri = body["results"][0]["document_uri"]
        .as_str()
        .expect("lexical result has a document URI");
    let document = VescMcpService::new()
        .resource_registry()
        .read(document_uri)
        .expect("search document resource is readable");
    let document: Value = serde_json::from_str(&document).expect("document resource is JSON");
    assert_eq!(document["document_id"], body["results"][0]["document_id"]);
    assert!(
        document["content"].as_str().is_some(),
        "document: {document}"
    );
}

#[test]
fn documented_search_examples_are_behaviorally_supported() {
    let harness = McpTestHarness::new();
    for params in [
        serde_json::json!({ "query": "lbm_add_extension", "mode": "lexical" }),
        serde_json::json!({
            "query": "package lifecycle from descriptor to load",
            "mode": "auto"
        }),
        serde_json::json!({
            "query": "NVM",
            "mode": "lexical",
            "filters": { "category": "firmware_api", "trust_tier": "first_party" }
        }),
    ] {
        let response = harness.call_tool("search_vesc_knowledge", params);
        let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
        assert_eq!(body["ok"], true, "response: {body}");
        assert!(
            body["results"]
                .as_array()
                .is_some_and(|results| !results.is_empty())
        );
    }
}
