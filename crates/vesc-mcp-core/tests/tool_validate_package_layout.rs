//! Integration tests for the `validate_package_layout` MCP tool.

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, TempWorkspace, fixture_path};

#[test]
fn tool_validate_package_layout_rejects_path_outside_env_roots() {
    let harness = McpTestHarness::new();
    let workspace = TempWorkspace::new();
    let response = harness.call_tool(
        "validate_package_layout",
        serde_json::json!({ "root": workspace.root.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|err| err.contains("VESC_PACKAGE_ROOTS")),
        "response: {body}"
    );
}

#[test]
fn tool_validate_poc_native_fixture_ok() {
    let harness = McpTestHarness::new();
    let root = fixture_path("native-lib-minimal");
    let response = harness.call_tool(
        "validate_package_layout",
        serde_json::json!({ "root": root.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");
    assert!(
        body.get("issues")
            .is_none_or(|issues| issues.as_array().is_some_and(Vec::is_empty)),
        "response: {body}"
    );
}

#[test]
fn tool_validate_missing_pkgdesc_fails() {
    let harness = McpTestHarness::new();
    let root = fixture_path("broken-missing-pkgdesc");
    let response = harness.call_tool(
        "validate_package_layout",
        serde_json::json!({ "root": root.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    let error = body["error"].as_str().expect("error message");
    assert!(
        error.contains("no pkgdesc.qml"),
        "expected missing pkgdesc error, got: {error}"
    );
}

#[test]
fn tool_validate_refloat_fixture_ok() {
    let harness = McpTestHarness::new();
    let root = fixture_path("refloat-minimal");
    let response = harness.call_tool(
        "validate_package_layout",
        serde_json::json!({ "root": root.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");
    assert!(
        body.get("issues")
            .is_none_or(|issues| issues.as_array().is_some_and(Vec::is_empty)),
        "response: {body}"
    );
}

#[test]
fn tool_validate_broken_fixture_fails() {
    let harness = McpTestHarness::new();
    let root = fixture_path("broken-missing-lisp");
    let response = harness.call_tool(
        "validate_package_layout",
        serde_json::json!({ "root": root.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");

    let issues = body["issues"].as_array().expect("issues array");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["kind"], "missing_asset");
    assert!(
        issues[0]["asset"]
            .as_str()
            .is_some_and(|asset| asset.contains("missing-package.lisp")),
        "issue: {}",
        issues[0]
    );
}
