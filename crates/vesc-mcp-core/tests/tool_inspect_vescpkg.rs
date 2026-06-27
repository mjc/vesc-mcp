//! Integration tests for the `inspect_vescpkg` MCP tool.

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, fixture_path};

#[test]
fn tool_inspect_vescpkg_reads_name() {
    let harness = McpTestHarness::new();
    let path = fixture_path("golden/poc-minimal.vescpkg");
    let response = harness.call_tool(
        "inspect_vescpkg",
        serde_json::json!({ "path": path.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");

    let inspection = &body["inspection"];
    assert_eq!(inspection["name"], "POC native-lib minimal fixture");
    assert_eq!(inspection["lisp_import_count"], 1);
    assert_eq!(inspection["lisp_editor_path"], "package-lib");
}

#[test]
fn tool_inspect_vescpkg_rejects_bad_magic() {
    let harness = McpTestHarness::new();
    let path = fixture_path("broken-bad-magic/bad-magic.vescpkg");
    let response = harness.call_tool(
        "inspect_vescpkg",
        serde_json::json!({ "path": path.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(body.get("inspection").is_none(), "response: {body}");

    let error = body["error"].as_str().expect("actionable error message");
    assert!(
        error.contains("invalid vescpkg wire format"),
        "error should describe wire failure: {error}"
    );
}

#[test]
fn tool_inspect_vescpkg_rejects_truncated_wire() {
    let harness = McpTestHarness::new();
    let path = fixture_path("broken-bad-wire/truncated.vescpkg");
    let response = harness.call_tool(
        "inspect_vescpkg",
        serde_json::json!({ "path": path.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(body.get("inspection").is_none(), "response: {body}");

    let error = body["error"].as_str().expect("actionable error message");
    assert!(
        error.contains("invalid vescpkg wire format"),
        "error should describe truncated package: {error}"
    );
}
