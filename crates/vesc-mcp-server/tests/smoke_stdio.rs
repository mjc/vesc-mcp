//! Smoke test: spawn `vesc-mcp-server` on stdio and verify `tools/list` count.

use std::{path::PathBuf, process::Stdio};

use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

#[tokio::test]
async fn smoke_compact_search_rows_cross_stdio_boundary() -> anyhow::Result<()> {
    let server = std::env::var("CARGO_BIN_EXE_vesc-mcp-server")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_vesc-mcp-server");
    let transport =
        TokioChildProcess::new(tokio::process::Command::new(server).configure(|cmd| {
            cmd.env_remove("RUST_LOG");
        }))?;
    let client = ().serve(transport).await?;
    let arguments = serde_json::json!({"query":"lbm_add_extension"})
        .as_object()
        .cloned()
        .expect("search arguments object");
    let response = client
        .call_tool(CallToolRequestParams::new("search_vesc_knowledge").with_arguments(arguments))
        .await?;
    let text = response
        .content
        .first()
        .and_then(|content| content.as_text())
        .expect("compact search returns text content")
        .text
        .as_str();
    let body: serde_json::Value = serde_json::from_str(text)?;
    assert_eq!(
        body["fields"],
        serde_json::json!([
            "name",
            "category",
            "excerpt",
            "source_index",
            "chunk_id",
            "correction_ids",
            "origin"
        ])
    );
    assert!(
        body["results"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert!(
        text.len() < 4_000,
        "compact response was {} bytes",
        text.len()
    );
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn smoke_wire_payloads_keep_catalog_and_compact_search_bounded() -> anyhow::Result<()> {
    let server = std::env::var("CARGO_BIN_EXE_vesc-mcp-server")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_vesc-mcp-server");
    let mut child = tokio::process::Command::new(server)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("server stdin");
    let stdout = child.stdout.take().expect("server stdout");
    let mut lines = BufReader::new(stdout).lines();
    for request in [
        serde_json::json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":"2024-11-05",
                "capabilities":{},
                "clientInfo":{"name":"wire-smoke","version":"1"}
            }
        }),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        serde_json::json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{
                "name":"search_vesc_knowledge",
                "arguments":{"query":"lbm_add_extension","mode":"lexical","limit":10}
            }
        }),
    ] {
        stdin.write_all(request.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    stdin.flush().await?;

    let mut tools_list_bytes = None;
    let mut search_bytes = None;
    while let Some(line) = lines.next_line().await? {
        let value: serde_json::Value = serde_json::from_str(&line)?;
        match value["id"].as_u64() {
            Some(2) => tools_list_bytes = Some(line.len()),
            Some(3) => {
                search_bytes = Some(line.len());
                break;
            }
            _ => {}
        }
    }
    child.kill().await?;
    child.wait().await?;

    let tools_list_bytes = tools_list_bytes.expect("tools/list response");
    let search_bytes = search_bytes.expect("search response");
    assert!(
        tools_list_bytes <= 8_250,
        "tools/list was {tools_list_bytes} bytes"
    );
    assert!(
        search_bytes <= 3_000,
        "compact search was {search_bytes} bytes"
    );
    Ok(())
}
