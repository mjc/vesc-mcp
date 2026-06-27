//! Integration tests for vescpkg manifest MCP resources.

use serde_json::Value;
use vesc_mcp_core::resources::{ResourceRegistry, read_manifest, register_manifest_resources};
use vesc_mcp_core::test_support::{fixture_path, fixture_sandbox_roots};
use vesc_mcp_core::tools::inspect::inspect_pkgdesc_with_sandbox;

fn dynamic_manifest_uri(relative: &str) -> String {
    format!(
        "vescpkg://manifest/{}",
        vesc_mcp_core::resources::encode_manifest_path(relative)
    )
}

fn parse_manifest_json(body: &str) -> Value {
    let json_part = body.split("\n---\n").next().expect("manifest JSON body");
    serde_json::from_str(json_part).expect("resource returns JSON")
}

#[test]
fn resource_manifest_matches_tool_output() {
    let allowed = fixture_sandbox_roots();
    let path = fixture_path("refloat-minimal").join("pkgdesc.qml");
    let tool_response = inspect_pkgdesc_with_sandbox(&path.to_string_lossy(), Some(&allowed));
    let tool_body =
        serde_json::to_value(&tool_response).expect("serialize inspect_pkgdesc response");

    let fixture_uri = "vescpkg://fixture/refloat-minimal/manifest";
    let resource_body = read_manifest(fixture_uri, &allowed)
        .unwrap_or_else(|err| panic!("read fixture manifest: {err}"));
    let resource_json = parse_manifest_json(&resource_body);

    assert_eq!(
        resource_json["ok"], tool_body["ok"],
        "resource: {resource_json}"
    );
    assert_eq!(resource_json["dialect"], tool_body["dialect"]);
    assert_eq!(resource_json["parsed"], tool_body["parsed"]);
    assert!(
        resource_json["raw_qml"]
            .as_str()
            .is_some_and(|raw| raw.contains("Refloat Minimal")),
        "missing raw pkgdesc text:\n{resource_json}"
    );
    assert!(
        resource_body.contains("Source: vesc-mcp/tests/fixtures/refloat-minimal/pkgdesc.qml"),
        "missing manifest attribution footer:\n{resource_body}"
    );
}

#[test]
fn resource_fixture_poc_native_lib_manifest_valid() {
    let allowed = fixture_sandbox_roots();
    let uri = "vescpkg://fixture/native-lib-minimal/manifest";
    let body = read_manifest(uri, &allowed).unwrap_or_else(|err| panic!("read poc fixture: {err}"));
    let json = parse_manifest_json(&body);

    assert_eq!(json["ok"], true, "response: {json}");
    assert_eq!(json["dialect"], "vesc_tool");
    assert_eq!(json["parsed"]["pkg_name"], "native-lib minimal fixture");
    assert_eq!(json["parsed"]["output_name"], "native-lib-minimal.vescpkg");
    assert_eq!(json["parsed"]["qml_path"], "");
    assert!(
        json["raw_qml"]
            .as_str()
            .is_some_and(|raw| raw.contains("native-lib minimal fixture")),
        "missing raw pkgdesc text:\n{json}"
    );
}

#[test]
fn resource_dynamic_manifest_matches_fixture_path() {
    let allowed = fixture_sandbox_roots();
    let uri = dynamic_manifest_uri("refloat-minimal/pkgdesc.qml");
    let body =
        read_manifest(&uri, &allowed).unwrap_or_else(|err| panic!("read dynamic manifest: {err}"));
    let json = parse_manifest_json(&body);

    assert_eq!(json["ok"], true, "response: {json}");
    assert_eq!(json["parsed"]["pkg_name"], "Refloat Minimal");
    assert!(
        json["raw_qml"].as_str().is_some(),
        "expected raw_qml field:\n{json}"
    );
}

#[test]
fn resource_manifest_registers_fixture_resources() {
    let mut registry = ResourceRegistry::new();
    register_manifest_resources(&mut registry).expect("register manifest resources");
    assert_eq!(registry.list_static().len(), 2);

    let uris: Vec<_> = registry
        .list_static()
        .iter()
        .map(|meta| meta.uri.as_str())
        .collect();
    assert!(uris.contains(&"vescpkg://fixture/refloat-minimal/manifest"));
    assert!(uris.contains(&"vescpkg://fixture/native-lib-minimal/manifest"));
}

#[test]
fn resource_manifest_rejects_outside_sandbox() {
    let allowed = fixture_sandbox_roots();
    let uri = dynamic_manifest_uri("/etc/passwd");
    let err = read_manifest(&uri, &allowed).expect_err("outside sandbox");
    assert!(
        err.to_string()
            .contains("outside configured VESC_PACKAGE_ROOTS")
            || err.to_string().contains("not found"),
        "unexpected error: {err}"
    );
}
