//! In-process MCP server integration tests.

use vesc_mcp_core::test_support::McpTestHarness;

#[test]
fn mcp_harness_lists_tools() {
    let harness = McpTestHarness::new();
    let tools = harness.list_tool_names();
    assert!(
        !tools.is_empty(),
        "expected at least one registered MCP tool"
    );
    assert!(
        tools.iter().any(|name| name == "ping"),
        "expected ping tool, got {tools:?}"
    );
}
