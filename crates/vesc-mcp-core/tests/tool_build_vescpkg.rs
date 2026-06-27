//! Integration tests for the `build_vescpkg` MCP tool (`vesc_tool` only).

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, TempWorkspace, fixture_path};

fn structured_error(body: &Value) -> &Value {
    let err = body.get("error").expect("error field");
    assert!(
        err.is_object(),
        "error should be a structured object: {body}"
    );
    err
}

#[test]
fn tool_build_outside_sandbox_fails() {
    let harness = McpTestHarness::new();
    let workspace = TempWorkspace::new();
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": workspace.root.to_string_lossy(),
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    let err = structured_error(&body);
    assert_eq!(err["code"], "SANDBOX_DENIED");
}

#[test]
fn tool_build_invalid_layout_fails_before_spawn() {
    let harness = McpTestHarness::new();
    let root = fixture_path("broken-missing-lisp");
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": root.to_string_lossy(),
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    let err = structured_error(&body);
    assert_eq!(err["code"], "LAYOUT_INVALID");
    assert!(
        !err["message"]
            .as_str()
            .is_some_and(|message| message.contains("spawn")),
        "layout errors must not come from vesc_tool spawn: {body}"
    );
}

#[test]
fn tool_build_vesc_tool_spawn_fails_without_binary() {
    let harness = McpTestHarness::new();
    let root = fixture_path("refloat-minimal");
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": root.to_string_lossy(),
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    let err = structured_error(&body);
    assert_eq!(err["code"], "VESC_TOOL_SPAWN_FAILED");
    assert!(err["hint"].as_str().is_some());
}
