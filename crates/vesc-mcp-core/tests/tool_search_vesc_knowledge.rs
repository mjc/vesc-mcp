//! Integration test for the `search_vesc_knowledge` MCP tool.

use serde_json::Value;
use vesc_mcp_core::test_support::McpTestHarness;

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
