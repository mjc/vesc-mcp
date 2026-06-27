//! Smoke test: spawn `vesc-mcp-server` on stdio and verify `tools/list` count.

use std::path::PathBuf;

use rmcp::{
    ServiceExt,
    transport::{ConfigureCommandExt, TokioChildProcess},
};

#[tokio::test]
async fn smoke_tools_list_count_at_least_seven() -> anyhow::Result<()> {
    let server = std::env::var("CARGO_BIN_EXE_vesc-mcp-server")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_vesc-mcp-server");
    let min = std::env::var("DOCS_SMOKE_MIN_TOOLS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(7);

    let transport =
        TokioChildProcess::new(tokio::process::Command::new(server).configure(|cmd| {
            cmd.env_remove("RUST_LOG");
        }))?;

    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    assert!(
        tools.len() >= min,
        "expected at least {min} MCP tools, got {} ({:?})",
        tools.len(),
        tools.iter().map(|tool| &tool.name).collect::<Vec<_>>()
    );
    client.cancel().await?;
    Ok(())
}
