//! MCP service definition and tool routing.

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};

use crate::tools::inspect::{InspectPkgdescParams, inspect_pkgdesc_json};
use crate::tools::list_packages::{ListPackagesParams, list_vesc_packages_json};

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug)]
pub struct VescMcpService {
    tool_router: ToolRouter<Self>,
}

impl Default for VescMcpService {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl VescMcpService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Tool names registered on this service (for integration test harnesses).
    #[must_use]
    pub fn list_tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

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

    #[tool(
        description = "Discover vescpkg package roots by scanning for pkgdesc.qml under configured paths"
    )]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn list_vesc_packages(&self, Parameters(params): Parameters<ListPackagesParams>) -> String {
        list_vesc_packages_json(&params)
    }

    #[tool(description = "Parse a pkgdesc.qml file and return structured descriptor fields")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn inspect_pkgdesc(&self, Parameters(params): Parameters<InspectPkgdescParams>) -> String {
        inspect_pkgdesc_json(&params)
    }
}

#[tool_handler(
    router = self.tool_router,
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
    let service = VescMcpService::new();
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

    #[test]
    fn list_tool_names_includes_ping() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "ping"));
    }

    #[test]
    fn list_tool_names_includes_list_vesc_packages() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "list_vesc_packages"));
    }

    #[test]
    fn list_tool_names_includes_inspect_pkgdesc() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "inspect_pkgdesc"));
    }
}
