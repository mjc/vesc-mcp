//! Integration tests for catalog doc topic MCP resources.

use std::path::PathBuf;

use vesc_mcp_core::resources::{
    LISP_IMPORTS_URI, PKGDESC_DIALECTS_URI, ResourceRegistry, VESC_C_IF_URI, read_doc_topic,
    register_doc_topic_resources,
};

fn catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn resource_doc_topic_pkgdesc_dialects_mentions_both_schemas() {
    let body = read_doc_topic(PKGDESC_DIALECTS_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read pkgdesc dialects: {err}"));
    assert!(
        body.contains("pkgName"),
        "missing vesc_tool schema property:\n{body}"
    );
    assert!(
        body.contains("packageName"),
        "missing legacy POC schema property:\n{body}"
    );
    assert!(
        body.contains("Source:"),
        "missing attribution footer:\n{body}"
    );
}

#[test]
fn resource_doc_topic_vesc_c_if_covers_lbm_core() {
    let body = read_doc_topic(VESC_C_IF_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read vesc_c_if topic: {err}"));
    assert!(
        body.contains("lbm_add_extension"),
        "missing lbm_core symbol:\n{body}"
    );
    assert!(
        body.contains("Source: bldc/lispBM/c_libs/vesc_c_if.h"),
        "missing header attribution:\n{body}"
    );
}

#[test]
fn resource_doc_topic_lisp_imports_describes_wire_format() {
    let body = read_doc_topic(LISP_IMPORTS_URI, &catalog_root())
        .unwrap_or_else(|err| panic!("read lisp imports topic: {err}"));
    assert!(
        body.contains("lispData"),
        "missing lispData field reference:\n{body}"
    );
    assert!(
        body.contains("import") && body.contains("offset"),
        "missing import table layout:\n{body}"
    );
    assert!(
        body.contains("Source:"),
        "missing attribution footer:\n{body}"
    );
}

#[test]
fn resource_doc_topic_registers_three_static_resources() {
    let mut registry = ResourceRegistry::new();
    register_doc_topic_resources(&mut registry).expect("register doc topic resources");
    assert_eq!(registry.list_static().len(), 3);

    let uris: Vec<_> = registry
        .list_static()
        .iter()
        .map(|meta| meta.uri.as_str())
        .collect();
    assert!(uris.contains(&PKGDESC_DIALECTS_URI));
    assert!(uris.contains(&VESC_C_IF_URI));
    assert!(uris.contains(&LISP_IMPORTS_URI));

    for meta in registry.list_static() {
        assert_eq!(meta.mime_type, "text/markdown");
    }
}
