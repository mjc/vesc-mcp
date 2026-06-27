//! MCP service definition and tool routing.

use rmcp::{
    ServerHandler, ServiceExt, handler::server::wrapper::Parameters, schemars, tool, tool_handler,
    tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PingParams {
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PingResponse {
    pub ok: bool,
    pub echo: String,
    pub server: String,
}

#[derive(Clone)]
pub struct VescMcpService;

#[tool_router]
impl VescMcpService {
    #[tool(description = "Health check — returns server identity and optional echo")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn ping(&self, Parameters(PingParams { message }): Parameters<PingParams>) -> String {
        let payload = PingResponse {
            ok: true,
            echo: decide_ping_echo(message),
            server: "vesc-mcp".into(),
        };
        serde_json::to_string(&payload).unwrap_or_else(|_| {
            r#"{"ok":false,"echo":"serialization failed","server":"vesc-mcp"}"#.into()
        })
    }
}

#[tool_handler(
    name = "vesc-mcp",
    version = "0.1.0",
    instructions = "MCP server for VESC firmware and vescpkg development. Start with ping, then list/inspect tools as they land."
)]
impl ServerHandler for VescMcpService {}

#[must_use]
pub fn decide_ping_echo(message: Option<String>) -> String {
    message.unwrap_or_else(|| "pong".into())
}

/// Run the MCP server on stdio until the client disconnects.
///
/// # Errors
///
/// Returns an error if the transport fails to initialize or the session ends unexpectedly.
pub async fn run_stdio_server() -> anyhow::Result<()> {
    let service = VescMcpService;
    let running = service.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_ping_echo_defaults_to_pong() {
        assert_eq!(decide_ping_echo(None), "pong");
    }

    #[test]
    fn decide_ping_echo_returns_custom_message() {
        assert_eq!(decide_ping_echo(Some("hello vesc".into())), "hello vesc");
    }
}
