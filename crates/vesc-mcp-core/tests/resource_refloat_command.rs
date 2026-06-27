//! Integration tests for catalog refloat command doc MCP resources.

use std::path::PathBuf;

use vesc_mcp_core::RepoRoots;
use vesc_mcp_core::resources::{
    REALTIME_DATA_COMMAND_URI, ResourceRegistry, read_refloat_command,
    register_refloat_command_resources,
};

fn catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[test]
fn resource_refloat_command_realtime_data() {
    let roots = RepoRoots::from_env();
    let body = read_refloat_command(
        REALTIME_DATA_COMMAND_URI,
        &catalog_root(),
        roots.root_for(vesc_mcp_core::CatalogRepo::Refloat),
    )
    .unwrap_or_else(|err| panic!("read REALTIME_DATA command doc: {err}"));
    assert!(
        !body.is_empty(),
        "REALTIME_DATA entry should not be empty:\n{body}"
    );
    assert!(
        body.contains("selectable realtime data") || body.contains("bitmask"),
        "expected first-paragraph summary from doc/commands/REALTIME_DATA.md:\n{body}"
    );
    assert!(
        body.contains("Source: refloat/doc/commands/REALTIME_DATA.md"),
        "missing doc attribution footer:\n{body}"
    );
    assert!(
        body.contains("Source: catalog/refloat/commands.yaml"),
        "missing catalog attribution footer:\n{body}"
    );
}

#[test]
fn resource_refloat_command_registers_catalog_commands() {
    let mut registry = ResourceRegistry::new();
    register_refloat_command_resources(&mut registry, &catalog_root())
        .expect("register refloat command resources");
    assert!(
        registry.lookup(REALTIME_DATA_COMMAND_URI).is_some(),
        "REALTIME_DATA should be registered"
    );
    assert!(
        registry.list_static().len() >= 9,
        "expected public + internal commands from catalog"
    );
}
