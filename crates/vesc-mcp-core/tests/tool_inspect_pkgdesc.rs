//! Integration tests for the `inspect_pkgdesc` MCP tool.

use std::fs;

use serde_json::Value;
use vesc_mcp_core::test_support::{McpTestHarness, TempWorkspace, fixture_path};

#[test]
fn tool_inspect_pkgdesc_rejects_path_outside_env_roots() {
    let harness = McpTestHarness::new();
    let workspace = TempWorkspace::new();
    let path = workspace.root.join("pkgdesc.qml");
    fs::write(&path, "PackageDescription {}").expect("write pkgdesc");

    let response = harness.call_tool(
        "inspect_pkgdesc",
        serde_json::json!({ "path": path.to_string_lossy() }),
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
fn tool_inspect_pkgdesc_refloat_dialect() {
    let harness = McpTestHarness::new();
    let path = fixture_path("refloat-minimal").join("pkgdesc.qml");
    let response = harness.call_tool(
        "inspect_pkgdesc",
        serde_json::json!({ "path": path.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");
    assert_eq!(body["dialect"], "vesc_tool");

    let parsed = &body["parsed"];
    assert_eq!(parsed["pkg_name"], "Refloat Minimal");
    assert_eq!(parsed["output_name"], "refloat-minimal.vescpkg");
    assert_eq!(parsed["description_md_path"], "package_README-gen.md");
    assert_eq!(parsed["lisp_path"], "lisp/package.lisp");
    assert_eq!(parsed["qml_path"], "ui.qml");
    assert_eq!(parsed["qml_is_fullscreen"], false);
}

#[test]
fn tool_inspect_pkgdesc_poc_native_dialect() {
    let harness = McpTestHarness::new();
    let path = fixture_path("native-lib-minimal/package/pkgdesc.qml");
    let response = harness.call_tool(
        "inspect_pkgdesc",
        serde_json::json!({ "path": path.to_string_lossy() }),
    );

    let body: Value = serde_json::from_str(&response).expect("tool returns JSON");
    assert_eq!(body["ok"], true, "response: {body}");
    assert_eq!(body["dialect"], "vesc_tool");

    let parsed = &body["parsed"];
    assert_eq!(parsed["pkg_name"], "native-lib minimal fixture");
    assert_eq!(parsed["output_name"], "native-lib-minimal.vescpkg");
    assert_eq!(parsed["description_md_path"], "README.md");
    assert_eq!(parsed["lisp_path"], "code.lisp");
    assert_eq!(parsed["qml_path"], "");
    assert_eq!(parsed["qml_is_fullscreen"], false);
}
