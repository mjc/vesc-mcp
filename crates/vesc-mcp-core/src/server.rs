//! MCP service definition and tool routing.

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        Implementation, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
        ResourceTemplate, ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};

use std::sync::Arc;

use crate::resources::{ResourceReadError, ResourceRegistry, ResourceSubscriptions};
use serde::{Deserialize, Serialize};

use crate::tools::build::{BuildVescpkgParams, build_vescpkg_json};
use crate::tools::check::{RunPackageChecksParams, run_package_checks_json};
use crate::tools::inspect::{
    InspectPkgdescParams, InspectVescpkgParams, inspect_pkgdesc_json, inspect_vescpkg_json,
};
use crate::tools::list_packages::{ListPackagesParams, list_vesc_packages_json};
use crate::tools::search_knowledge::{SearchVescKnowledgeParams, search_vesc_knowledge_json};
use crate::tools::validate::{ValidatePackageLayoutParams, validate_package_layout_json};

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
    resources: Arc<ResourceRegistry>,
    resource_subscriptions: Arc<ResourceSubscriptions>,
}

impl Default for VescMcpService {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl VescMcpService {
    /// Create a new MCP service with default tools and resource registry.
    ///
    /// # Panics
    ///
    /// Panics if built-in resource registration fails.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            resources: Arc::new(
                ResourceRegistry::with_defaults().expect("default MCP resource registry"),
            ),
            resource_subscriptions: Arc::new(ResourceSubscriptions::new()),
        }
    }

    #[must_use]
    pub fn resource_registry(&self) -> &ResourceRegistry {
        self.resources.as_ref()
    }

    #[must_use]
    pub fn resource_subscriptions(&self) -> &ResourceSubscriptions {
        self.resource_subscriptions.as_ref()
    }

    /// Notify subscribed clients that a resource body changed.
    ///
    /// Returns `true` when the URI was subscribed and a notification was sent.
    ///
    /// # Errors
    ///
    /// Propagates MCP transport errors from the peer notification call.
    pub async fn notify_resource_updated_if_subscribed(
        &self,
        peer: &rmcp::Peer<RoleServer>,
        uri: &str,
    ) -> Result<bool, rmcp::ServiceError> {
        if !self.resource_subscriptions.is_subscribed(uri) {
            return Ok(false);
        }
        peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
            .await?;
        Ok(true)
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

    #[tool(description = "Read a .vescpkg wire artifact and return structured package fields")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn inspect_vescpkg(&self, Parameters(params): Parameters<InspectVescpkgParams>) -> String {
        inspect_vescpkg_json(&params)
    }

    #[tool(
        description = "Validate that all assets referenced by pkgdesc.qml exist under a package root"
    )]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn validate_package_layout(
        &self,
        Parameters(params): Parameters<ValidatePackageLayoutParams>,
    ) -> String {
        validate_package_layout_json(&params)
    }

    #[tool(
        description = "Build a .vescpkg wire artifact from a package root (rust in-tree adapter or vesc_tool CLI subprocess)"
    )]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn build_vescpkg(&self, Parameters(params): Parameters<BuildVescpkgParams>) -> String {
        build_vescpkg_json(&params)
    }

    #[tool(description = "Run cargo fmt/clippy/test checks in a sandboxed vescpkg package root")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn run_package_checks(&self, Parameters(params): Parameters<RunPackageChecksParams>) -> String {
        run_package_checks_json(&params)
    }

    #[tool(description = "Search the embedded VESC firmware and package knowledge index")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    fn search_vesc_knowledge(
        &self,
        Parameters(params): Parameters<SearchVescKnowledgeParams>,
    ) -> String {
        search_vesc_knowledge_json(&params)
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "vesc-mcp",
    version = "0.1.0",
    instructions = "MCP server for VESC firmware and vescpkg development. Start with ping, then list/inspect tools as they land."
)]
impl ServerHandler for VescMcpService {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: self
                .resources
                .list_mcp_resources()
                .into_iter()
                .map(|resource| Resource::new(resource, None))
                .collect(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: self
                .resources
                .list_mcp_templates()
                .into_iter()
                .map(|template| ResourceTemplate::new(template, None))
                .collect(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match self.resources.read(&request.uri) {
            Ok(text) => {
                let mime_type = self
                    .resources
                    .lookup(&request.uri)
                    .map_or("application/json", |meta| meta.mime_type.as_str());
                Ok(ReadResourceResult::new(vec![
                    ResourceContents::text(text, &request.uri).with_mime_type(mime_type),
                ]))
            }
            Err(ResourceReadError::NotFound { uri }) => Err(McpError::resource_not_found(
                format!("resource not found: {uri}"),
                None,
            )),
            Err(ResourceReadError::ReadFailed { uri, message }) => Err(
                McpError::resource_not_found(format!("read failed for {uri}: {message}"), None),
            ),
        }
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if !self.resources.is_readable(&request.uri) {
            return Err(McpError::resource_not_found(
                format!("resource not found: {}", request.uri),
                None,
            ));
        }
        self.resource_subscriptions.subscribe(request.uri);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.resource_subscriptions.unsubscribe(&request.uri);
        Ok(())
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new("vesc-mcp", "0.1.0"))
        .with_instructions(
            "MCP server for VESC firmware and vescpkg development. Start with ping, then list/inspect tools as they land.",
        )
    }
}

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

    #[test]
    fn list_tool_names_includes_inspect_vescpkg() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "inspect_vescpkg"));
    }

    #[test]
    fn list_tool_names_includes_validate_package_layout() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "validate_package_layout"));
    }

    #[test]
    fn list_tool_names_includes_build_vescpkg() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "build_vescpkg"));
    }

    #[test]
    fn get_info_advertises_tools_and_resources() {
        let service = VescMcpService::new();
        let info = service.get_info();
        assert!(info.capabilities.tools.is_some());
        let resources = info
            .capabilities
            .resources
            .as_ref()
            .expect("resources capability");
        assert!(resources.subscribe.unwrap_or(false));
    }

    #[test]
    fn list_tool_names_includes_run_package_checks() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "run_package_checks"));
    }

    #[test]
    fn list_tool_names_includes_search_vesc_knowledge() {
        let service = VescMcpService::new();
        let names = service.list_tool_names();
        assert!(names.iter().any(|name| name == "search_vesc_knowledge"));
    }
}
