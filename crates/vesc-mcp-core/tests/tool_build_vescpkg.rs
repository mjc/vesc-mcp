//! Integration tests for the `build_vescpkg` MCP tool (rust mode).

use std::fs;

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, TempWorkspace, fixture_path};

#[test]
fn tool_build_rust_mode_creates_artifact() {
    let harness = McpTestHarness::new();
    let root = fixture_path("poc-native-lib-minimal");
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": root.to_string_lossy(),
            "mode": "rust",
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");

    let artifact_path = body["artifact_path"].as_str().expect("artifact_path");
    assert!(artifact_path.ends_with("poc-native-lib-minimal.vescpkg"));
    assert!(std::path::Path::new(artifact_path).is_file());

    let sha256 = body["sha256"].as_str().expect("sha256");
    assert_eq!(sha256.len(), 64);
    assert!(sha256.chars().all(|ch| ch.is_ascii_hexdigit()));

    let golden_hash =
        fs::read_to_string(fixture_path("golden/poc-minimal.sha256")).expect("golden sha256");
    let expected = golden_hash
        .split_whitespace()
        .next()
        .expect("hash column")
        .to_ascii_lowercase();
    assert_eq!(sha256, expected);

    let size_bytes = body["size_bytes"].as_u64().expect("size_bytes");
    assert!(size_bytes > 0);
}

#[test]
fn tool_build_rust_mode_missing_pkgdesc_fails() {
    let harness = McpTestHarness::new();
    let workspace = TempWorkspace::new();
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": workspace.root.to_string_lossy(),
            "mode": "rust",
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|err| err.contains("pkgdesc")),
        "response: {body}"
    );
}

#[test]
fn tool_build_rust_mode_invalid_layout_fails() {
    let harness = McpTestHarness::new();
    let root = fixture_path("broken-missing-lisp");
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": root.to_string_lossy(),
            "mode": "rust",
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(body["error"].as_str().is_some(), "response: {body}");
}

#[test]
fn tool_build_vesc_tool_mocked_via_harness() {
    let harness = McpTestHarness::new();
    let root = fixture_path("refloat-minimal");
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": root.to_string_lossy(),
            "mode": "vesc_tool",
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    // Without vesc_tool on PATH the real subprocess fails; integration harness uses production runner.
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|err| err.contains("spawn") || err.contains("vesc_tool")),
        "response: {body}"
    );
}

#[test]
fn tool_build_unsupported_mode_fails() {
    let harness = McpTestHarness::new();
    let root = fixture_path("poc-native-lib-minimal");
    let response = harness.call_tool(
        "build_vescpkg",
        serde_json::json!({
            "root": root.to_string_lossy(),
            "mode": "cmake",
        }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], false, "response: {body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|err| err.contains("unsupported build mode")),
        "response: {body}"
    );
}
