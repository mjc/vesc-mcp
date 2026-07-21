//! MCP service definition and tool routing.

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        Implementation, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResult, ResourceContents,
        ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo, SubscribeRequestParams,
        UnsubscribeRequestParams,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
};

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::config::{
    KnowledgeConfig, McpConfig, allowed_package_roots_with_client_roots, validate_sandbox_file,
};
use crate::resources::{
    CatalogSourceWatcher, ResourceReadError, ResourceRegistry, ResourceSubscriptions,
};
use serde::{Deserialize, Serialize};

use crate::tools::build::{BuildVescpkgParams, build_vescpkg_json_with_sandbox};
use crate::tools::check::{RunPackageChecksParams, run_package_checks_json_with_sandbox};
use crate::tools::inspect::{
    InspectPkgdescParams, InspectVescpkgParams, inspect_pkgdesc_json_with_sandbox,
    inspect_vescpkg_json_with_sandbox,
};
use crate::tools::list_packages::{ListPackagesParams, list_vesc_packages_json_with_client_roots};
#[cfg(feature = "managed-git")]
use crate::tools::list_source_versions::{
    ListVescSourceVersionsParams, list_vesc_source_versions_json,
};
#[cfg(feature = "managed-git")]
use crate::tools::prepare_knowledge::{PrepareVescKnowledgeParams, prepare_vesc_knowledge_json};
use crate::tools::search_knowledge::{
    SearchVescKnowledgeParams, search_vesc_knowledge_json_with_feedback,
};
use crate::tools::validate::{
    ValidatePackageLayoutParams, validate_package_layout_json_with_sandbox,
};

use crate::resources::FeedbackResourceHandler;
use crate::tools::knowledge_feedback::{
    CorrectVescKnowledgeParams, FeedbackStore, SubmitKnowledgeFeedbackParams,
    correct_vesc_knowledge_tool_with_store, submit_vesc_knowledge_feedback_with_store,
};

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
    pub knowledge: Option<crate::preparation_status::KnowledgePreparationStatus>,
    pub current_repository: Option<CurrentRepository>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetCurrentRepositoryParams {
    pub repository: String,
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct CurrentRepository {
    pub repository: String,
    pub root: Option<String>,
    pub knowledge_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct CurrentRepositoryResponse {
    ok: bool,
    current_repository: Option<CurrentRepository>,
    error: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct SessionContext {
    current_repository: Arc<RwLock<Option<CurrentRepository>>>,
}

impl SessionContext {
    fn current_repository(&self) -> Option<CurrentRepository> {
        self.current_repository.read().ok()?.clone()
    }

    fn set_current_repository(&self, repository: CurrentRepository) -> Result<(), String> {
        *self
            .current_repository
            .write()
            .map_err(|_| "current repository state is unavailable".to_string())? = Some(repository);
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct VescMcpService {
    tool_router: ToolRouter<Self>,
    state: Arc<SharedMcpState>,
    session: SessionContext,
}

#[derive(Clone, Debug)]
struct SharedMcpState {
    resources: Arc<ResourceRegistry>,
    resource_subscriptions: Arc<ResourceSubscriptions>,
    catalog_watcher: Arc<CatalogSourceWatcher>,
    catalog_root: PathBuf,
    knowledge: KnowledgeConfig,
    feedback: Option<FeedbackStore>,
    feedback_writes_enabled: bool,
}

impl SharedMcpState {
    fn new() -> Self {
        let config = McpConfig::load();
        Self::with_config(
            config.knowledge.clone(),
            config.feedback.path.as_deref().map(FeedbackStore::new),
            config.feedback.writes_enabled,
        )
    }

    fn with_config(
        knowledge: KnowledgeConfig,
        feedback: Option<FeedbackStore>,
        writes_enabled: bool,
    ) -> Self {
        let mut resources = ResourceRegistry::with_knowledge_config(&knowledge)
            .expect("default MCP resource registry");
        if let Some(store) = feedback.clone() {
            resources.register_handler(FeedbackResourceHandler::new(store));
        }
        Self {
            resources: Arc::new(resources),
            resource_subscriptions: Arc::new(ResourceSubscriptions::new()),
            catalog_watcher: Arc::new(CatalogSourceWatcher::new()),
            catalog_root: crate::workspace::catalog_root(),
            knowledge,
            feedback_writes_enabled: writes_enabled && feedback.is_some(),
            feedback,
        }
    }
}

/// HTTP MCP service exposing shared knowledge and authenticated package tools.
#[derive(Clone, Debug)]
pub struct HttpMcpService {
    tool_router: ToolRouter<Self>,
    state: Arc<SharedMcpState>,
    feedback_writes_enabled: bool,
    session: SessionContext,
}

const PACKAGE_TOOL_NAMES: [&str; 6] = [
    "list_vesc_packages",
    "inspect_pkgdesc",
    "inspect_vescpkg",
    "validate_package_layout",
    "build_vescpkg",
    "run_package_checks",
];

impl Default for VescMcpService {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = base_tool_router)]
impl VescMcpService {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let router = Self::base_tool_router();
        #[cfg(feature = "managed-git")]
        let router = router
            .with_route((
                Self::list_vesc_source_versions_tool_attr(),
                Self::list_vesc_source_versions,
            ))
            .with_route((
                Self::prepare_vesc_knowledge_tool_attr(),
                Self::prepare_vesc_knowledge,
            ));
        router
    }

    /// Create a new MCP service with default tools and resource registry.
    ///
    /// # Panics
    ///
    /// Panics if built-in resource registration fails.
    #[must_use]
    pub fn new() -> Self {
        Self::from_state(SharedMcpState::new())
    }

    /// Create a service over an explicit knowledge configuration.
    #[must_use]
    pub fn with_knowledge_config(knowledge: KnowledgeConfig) -> Self {
        Self::from_state(SharedMcpState::with_config(knowledge, None, false))
    }

    /// Create a service with an explicit durable feedback store.
    #[must_use]
    pub fn with_feedback_store(path: &std::path::Path, writes_enabled: bool) -> Self {
        Self::with_knowledge_config_and_feedback_store(
            McpConfig::load().knowledge.clone(),
            path,
            writes_enabled,
        )
    }

    pub(crate) fn with_knowledge_config_and_feedback_store(
        knowledge: KnowledgeConfig,
        path: &std::path::Path,
        writes_enabled: bool,
    ) -> Self {
        Self::from_state(SharedMcpState::with_config(
            knowledge,
            Some(FeedbackStore::new(path)),
            writes_enabled,
        ))
    }

    fn from_state(state: SharedMcpState) -> Self {
        let mut tool_router = Self::tool_router();
        if !state.feedback_writes_enabled {
            tool_router.disable_route("submit_vesc_knowledge_feedback");
            tool_router.disable_route("correct_vesc_knowledge");
        }
        Self {
            tool_router,
            state: Arc::new(state),
            session: SessionContext::default(),
        }
    }

    /// Create the restricted, read-only service used by Streamable HTTP clients.
    #[must_use]
    pub fn http_service(&self) -> HttpMcpService {
        self.http_service_with_authenticated_writes(false)
    }

    /// Create the HTTP service, exposing package tools and feedback writes only for authenticated clients.
    #[must_use]
    pub fn http_service_with_authenticated_writes(&self, authenticated: bool) -> HttpMcpService {
        let feedback_writes_enabled = authenticated && self.state.feedback_writes_enabled;
        let mut tool_router = HttpMcpService::tool_router();
        if !authenticated {
            for name in PACKAGE_TOOL_NAMES {
                tool_router.disable_route(name);
            }
        }
        if !feedback_writes_enabled {
            tool_router.disable_route("submit_vesc_knowledge_feedback");
            tool_router.disable_route("correct_vesc_knowledge");
        }
        HttpMcpService {
            tool_router,
            state: Arc::clone(&self.state),
            feedback_writes_enabled,
            session: SessionContext::default(),
        }
    }

    #[must_use]
    pub fn resource_registry(&self) -> &ResourceRegistry {
        self.state.resources.as_ref()
    }

    #[must_use]
    pub fn resource_subscriptions(&self) -> &ResourceSubscriptions {
        self.state.resource_subscriptions.as_ref()
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
        notify_resource_updated_if_subscribed(&self.state, peer, uri).await
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
        ping_json(message, &self.state.knowledge, &self.session)
    }

    #[tool(
        description = "Set this chat's repository for its MCP session."
    )]
    async fn set_current_repository(
        &self,
        Parameters(params): Parameters<SetCurrentRepositoryParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots =
            allowed_package_roots_with_client_roots(&client_package_roots(&context).await);
        set_current_repository_json(
            &params,
            &self.state.knowledge,
            &self.session,
            &allowed_roots,
        )
    }

    #[tool(
        description = "Discover vescpkg package roots by scanning for pkgdesc.qml under configured or client project paths"
    )]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    async fn list_vesc_packages(
        &self,
        Parameters(params): Parameters<ListPackagesParams>,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> String {
        let client_roots = client_package_roots_with_session(&context, &self.session).await;
        list_vesc_packages_json_with_client_roots(&params, &client_roots)
    }

    #[tool(description = "Parse a pkgdesc.qml file and return structured descriptor fields")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    async fn inspect_pkgdesc(
        &self,
        Parameters(params): Parameters<InspectPkgdescParams>,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        inspect_pkgdesc_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(description = "Read a .vescpkg wire artifact and return structured package fields")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    async fn inspect_vescpkg(
        &self,
        Parameters(params): Parameters<InspectVescpkgParams>,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        inspect_vescpkg_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(
        description = "Validate that all assets referenced by pkgdesc.qml exist under a package root"
    )]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    async fn validate_package_layout(
        &self,
        Parameters(params): Parameters<ValidatePackageLayoutParams>,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        validate_package_layout_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(
        description = "Build a .vescpkg wire artifact from a package root (rust in-tree adapter or vesc_tool CLI subprocess)"
    )]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    async fn build_vescpkg(
        &self,
        Parameters(params): Parameters<BuildVescpkgParams>,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        build_vescpkg_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(description = "Run cargo fmt/clippy/test checks in a sandboxed vescpkg package root")]
    #[allow(clippy::unused_self)] // rmcp tool router requires &self
    async fn run_package_checks(
        &self,
        Parameters(params): Parameters<RunPackageChecksParams>,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        run_package_checks_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(description = "Search VESC knowledge; corrections first, notes unverified.")]
    fn search_vesc_knowledge(
        &self,
        Parameters(params): Parameters<SearchVescKnowledgeParams>,
    ) -> String {
        let params = search_params_for_session(params, &self.session);
        search_vesc_knowledge_json_with_feedback(
            &params,
            &self.state.knowledge,
            self.state.feedback.as_ref(),
            &self.state.resources,
        )
    }

    #[tool(
        description = "Persist a low-trust reusable lesson after a user correction or newly discovered gap. Use for helpful notes that are not yet backed by registered VESC resources; the note remains visibly unverified in later search."
    )]
    fn submit_vesc_knowledge_feedback(
        &self,
        Parameters(params): Parameters<SubmitKnowledgeFeedbackParams>,
    ) -> String {
        feedback_json(self.state.feedback.as_ref(), |store| {
            submit_vesc_knowledge_feedback_with_store(&params, store)
        })
    }

    #[tool(
        description = "Persist an evidence-backed VESC correction only after user authorization. Include exact registered evidence, why reasoning failed, structured gap diagnoses, and the bounded original retrieval trace so the base knowledge defect can be repaired and replayed."
    )]
    fn correct_vesc_knowledge(
        &self,
        Parameters(params): Parameters<CorrectVescKnowledgeParams>,
    ) -> String {
        feedback_json(self.state.feedback.as_ref(), |store| {
            correct_vesc_knowledge_tool_with_store(&params, store, &self.state.resources)
        })
    }

    #[tool(
        description = "Replay a stored correction's original bounded query against base VESC knowledge only. Use the report to prove whether corpus/retrieval changes now surface every decisive evidence ID without the learned advisory."
    )]
    fn replay_vesc_knowledge_correction(
        &self,
        Parameters(params): Parameters<
            crate::tools::search_knowledge::ReplayVescKnowledgeCorrectionParams,
        >,
    ) -> String {
        if params.mark_covered && !self.state.feedback_writes_enabled {
            return replay_error_json(&params, "mark_covered requires enabled feedback writes");
        }
        let Some(store) = self.state.feedback.as_ref() else {
            return replay_error_json(&params, "knowledge feedback is not configured");
        };
        let report = crate::tools::search_knowledge::replay_vesc_knowledge_correction(
            &params,
            &self.state.knowledge,
            store,
        );
        replay_report_json(&report)
    }
}

#[cfg(feature = "managed-git")]
impl VescMcpService {
    #[tool(description = "List cached source refs before prepare/search; never fetches.")]
    fn list_vesc_source_versions(
        &self,
        Parameters(params): Parameters<ListVescSourceVersionsParams>,
    ) -> String {
        list_vesc_source_versions_json(&params, &self.state.knowledge)
    }

    #[tool(description = "Prepare one immutable source snapshot for search.")]
    async fn prepare_vesc_knowledge(
        &self,
        Parameters(params): Parameters<PrepareVescKnowledgeParams>,
    ) -> String {
        prepare_vesc_knowledge_json(&params, &self.state.knowledge).await
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
                .state
                .resources
                .list_mcp_resources()
                .into_iter()
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
                .state
                .resources
                .list_mcp_templates()
                .into_iter()
                .collect(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let catalog_changed = self
            .state
            .catalog_watcher
            .take_change_if_any(&request.uri, &self.state.catalog_root);
        match self.state.resources.read(&request.uri) {
            Ok(text) => {
                let mime_type = self
                    .state
                    .resources
                    .lookup(&request.uri)
                    .map_or("application/json", |meta| meta.mime_type.as_str());
                if catalog_changed
                    && self
                        .state
                        .resource_subscriptions
                        .is_subscribed(&request.uri)
                {
                    let _ = self
                        .notify_resource_updated_if_subscribed(&context.peer, &request.uri)
                        .await;
                }
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
        if !self.state.resources.is_readable(&request.uri) {
            return Err(McpError::resource_not_found(
                format!("resource not found: {}", request.uri),
                None,
            ));
        }
        self.state.resource_subscriptions.subscribe(&request.uri);
        self.state
            .catalog_watcher
            .seed_baseline(&request.uri, &self.state.catalog_root);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.state.resource_subscriptions.unsubscribe(&request.uri);
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
        .with_instructions(server_instructions(
            self.state.feedback.is_some(),
            self.state.feedback_writes_enabled,
        ))
    }
}

#[tool_router(router = base_tool_router)]
impl HttpMcpService {
    fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let router = Self::base_tool_router();
        #[cfg(feature = "managed-git")]
        let router = router
            .with_route((
                Self::list_vesc_source_versions_tool_attr(),
                Self::list_vesc_source_versions,
            ))
            .with_route((
                Self::prepare_vesc_knowledge_tool_attr(),
                Self::prepare_vesc_knowledge,
            ));
        router
    }

    /// Tool names registered on the HTTP-safe service.
    #[must_use]
    pub fn list_tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// Create isolated state for one Streamable HTTP MCP session.
    #[must_use]
    pub fn fresh_session(&self) -> Self {
        Self {
            tool_router: self.tool_router.clone(),
            state: Arc::clone(&self.state),
            feedback_writes_enabled: self.feedback_writes_enabled,
            session: SessionContext::default(),
        }
    }

    #[tool(description = "Health check — returns server identity and optional echo")]
    #[allow(clippy::unused_self)]
    fn ping(&self, Parameters(PingParams { message }): Parameters<PingParams>) -> String {
        ping_json(message, &self.state.knowledge, &self.session)
    }

    #[tool(
        description = "Set this chat's repository for its MCP session."
    )]
    async fn set_current_repository(
        &self,
        Parameters(params): Parameters<SetCurrentRepositoryParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots =
            allowed_package_roots_with_client_roots(&client_package_roots(&context).await);
        set_current_repository_json(
            &params,
            &self.state.knowledge,
            &self.session,
            &allowed_roots,
        )
    }

    #[tool(
        description = "Discover vescpkg package roots under configured or authenticated client project paths"
    )]
    async fn list_vesc_packages(
        &self,
        Parameters(params): Parameters<ListPackagesParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let client_roots = client_package_roots_with_session(&context, &self.session).await;
        list_vesc_packages_json_with_client_roots(&params, &client_roots)
    }

    #[tool(
        description = "Parse a pkgdesc.qml file under configured or authenticated client project roots"
    )]
    async fn inspect_pkgdesc(
        &self,
        Parameters(params): Parameters<InspectPkgdescParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        inspect_pkgdesc_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(
        description = "Read a .vescpkg wire artifact under configured or authenticated client project roots"
    )]
    async fn inspect_vescpkg(
        &self,
        Parameters(params): Parameters<InspectVescpkgParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        inspect_vescpkg_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(
        description = "Validate a package layout under configured or authenticated client project roots"
    )]
    async fn validate_package_layout(
        &self,
        Parameters(params): Parameters<ValidatePackageLayoutParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        validate_package_layout_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(
        description = "Build a .vescpkg artifact under configured or authenticated client project roots"
    )]
    async fn build_vescpkg(
        &self,
        Parameters(params): Parameters<BuildVescpkgParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        build_vescpkg_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(
        description = "Run package checks under configured or authenticated client project roots"
    )]
    async fn run_package_checks(
        &self,
        Parameters(params): Parameters<RunPackageChecksParams>,
        context: RequestContext<RoleServer>,
    ) -> String {
        let allowed_roots = package_roots_for_client(&context, &self.session).await;
        run_package_checks_json_with_sandbox(&params, &allowed_roots)
    }

    #[tool(description = "Search VESC knowledge; corrections first, notes unverified.")]
    fn search_vesc_knowledge(
        &self,
        Parameters(params): Parameters<SearchVescKnowledgeParams>,
    ) -> String {
        let params = search_params_for_session(params, &self.session);
        search_vesc_knowledge_json_with_feedback(
            &params,
            &self.state.knowledge,
            self.state.feedback.as_ref(),
            &self.state.resources,
        )
    }

    #[tool(
        description = "Persist a low-trust reusable lesson after a user correction or newly discovered gap. Use for helpful notes that are not yet backed by registered VESC resources; the note remains visibly unverified in later search."
    )]
    fn submit_vesc_knowledge_feedback(
        &self,
        Parameters(params): Parameters<SubmitKnowledgeFeedbackParams>,
    ) -> String {
        feedback_json(self.state.feedback.as_ref(), |store| {
            submit_vesc_knowledge_feedback_with_store(&params, store)
        })
    }

    #[tool(
        description = "Persist an evidence-backed VESC correction only after user authorization. Include exact registered evidence, why reasoning failed, structured gap diagnoses, and the bounded original retrieval trace so the base knowledge defect can be repaired and replayed."
    )]
    fn correct_vesc_knowledge(
        &self,
        Parameters(params): Parameters<CorrectVescKnowledgeParams>,
    ) -> String {
        feedback_json(self.state.feedback.as_ref(), |store| {
            correct_vesc_knowledge_tool_with_store(&params, store, &self.state.resources)
        })
    }

    #[tool(
        description = "Replay a stored correction's original bounded query against base VESC knowledge only. Use the report to prove whether corpus/retrieval changes now surface every decisive evidence ID without the learned advisory."
    )]
    fn replay_vesc_knowledge_correction(
        &self,
        Parameters(params): Parameters<
            crate::tools::search_knowledge::ReplayVescKnowledgeCorrectionParams,
        >,
    ) -> String {
        if params.mark_covered && !self.feedback_writes_enabled {
            return replay_error_json(
                &params,
                "mark_covered requires authenticated feedback writes",
            );
        }
        let Some(store) = self.state.feedback.as_ref() else {
            return replay_error_json(&params, "knowledge feedback is not configured");
        };
        let report = crate::tools::search_knowledge::replay_vesc_knowledge_correction(
            &params,
            &self.state.knowledge,
            store,
        );
        replay_report_json(&report)
    }
}

#[cfg(feature = "managed-git")]
impl HttpMcpService {
    #[tool(description = "List cached source refs before prepare/search; never fetches.")]
    fn list_vesc_source_versions(
        &self,
        Parameters(params): Parameters<ListVescSourceVersionsParams>,
    ) -> String {
        list_vesc_source_versions_json(&params, &self.state.knowledge)
    }

    #[tool(description = "Prepare one immutable source snapshot for search.")]
    async fn prepare_vesc_knowledge(
        &self,
        Parameters(params): Parameters<PrepareVescKnowledgeParams>,
    ) -> String {
        prepare_vesc_knowledge_json(&params, &self.state.knowledge).await
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "vesc-mcp-http",
    version = "0.1.0",
    instructions = "Shared HTTP MCP service for VESC knowledge search. Authenticated clients may use package-tree tools, sandboxed to configured roots plus that client's MCP roots."
)]
impl ServerHandler for HttpMcpService {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: self
                .state
                .resources
                .list_mcp_resources()
                .into_iter()
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
                .state
                .resources
                .list_mcp_templates()
                .into_iter()
                .collect(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let catalog_changed = self
            .state
            .catalog_watcher
            .take_change_if_any(&request.uri, &self.state.catalog_root);
        match self.state.resources.read(&request.uri) {
            Ok(text) => {
                let mime_type = self
                    .state
                    .resources
                    .lookup(&request.uri)
                    .map_or("application/json", |meta| meta.mime_type.as_str());
                if catalog_changed
                    && self
                        .state
                        .resource_subscriptions
                        .is_subscribed(&request.uri)
                {
                    let _ = notify_resource_updated_if_subscribed(
                        &self.state,
                        &context.peer,
                        &request.uri,
                    )
                    .await;
                }
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
        if !self.state.resources.is_readable(&request.uri) {
            return Err(McpError::resource_not_found(
                format!("resource not found: {}", request.uri),
                None,
            ));
        }
        self.state.resource_subscriptions.subscribe(&request.uri);
        self.state
            .catalog_watcher
            .seed_baseline(&request.uri, &self.state.catalog_root);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.state.resource_subscriptions.unsubscribe(&request.uri);
        Ok(())
    }

    fn get_info(&self) -> ServerInfo {
        http_server_info(self.state.feedback.is_some(), self.feedback_writes_enabled)
    }
}

fn ping_json(
    message: Option<String>,
    knowledge: &KnowledgeConfig,
    session: &SessionContext,
) -> String {
    let payload = PingResponse {
        ok: true,
        echo: decide_ping_echo(message),
        server: "vesc-mcp".into(),
        knowledge: knowledge_preparation_status(knowledge),
        current_repository: session.current_repository(),
    };
    serde_json::to_string(&payload).unwrap_or_else(|_| {
        r#"{"ok":false,"echo":"serialization failed","server":"vesc-mcp"}"#.into()
    })
}

fn set_current_repository_json(
    params: &SetCurrentRepositoryParams,
    knowledge: &KnowledgeConfig,
    session: &SessionContext,
    allowed_roots: &[PathBuf],
) -> String {
    let repository = params.repository.trim();
    let result = if repository.is_empty() {
        Err("repository must not be empty".to_string())
    } else {
        params
            .root
            .as_deref()
            .map(|root| validate_sandbox_file(std::path::Path::new(root), allowed_roots))
            .transpose()
            .and_then(|root| {
                if root.as_ref().is_some_and(|path| !path.is_dir()) {
                    return Err("repository root must be a directory".to_string());
                }
                let selection = CurrentRepository {
                    repository: repository.to_string(),
                    root: root.map(|path| path.to_string_lossy().into_owned()),
                    knowledge_available: knowledge
                        .repositories
                        .iter()
                        .any(|candidate| candidate.id().as_str() == repository),
                };
                session.set_current_repository(selection.clone())?;
                Ok(selection)
            })
    };
    let response = match result {
        Ok(current_repository) => CurrentRepositoryResponse {
            ok: true,
            current_repository: Some(current_repository),
            error: None,
        },
        Err(error) => CurrentRepositoryResponse {
            ok: false,
            current_repository: None,
            error: Some(error),
        },
    };
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

fn search_params_for_session(
    mut params: SearchVescKnowledgeParams,
    session: &SessionContext,
) -> SearchVescKnowledgeParams {
    if params.filters.repository.is_none()
        && let Some(repository) = session
            .current_repository()
            .filter(|repository| repository.knowledge_available)
    {
        params.filters.repository = Some(repository.repository);
    }
    params
}

pub(crate) fn knowledge_preparation_status(
    knowledge: &KnowledgeConfig,
) -> Option<crate::preparation_status::KnowledgePreparationStatus> {
    let repositories_total = knowledge.repositories.iter().len();
    if repositories_total == 0 {
        return None;
    }
    let data_root = knowledge.data_root.as_ref()?;
    Some(crate::preparation_status::read_or_starting(
        data_root.as_path(),
        repositories_total,
    ))
}

fn feedback_json(
    store: Option<&FeedbackStore>,
    write: impl FnOnce(&FeedbackStore) -> crate::tools::knowledge_feedback::FeedbackWriteResponse,
) -> String {
    let response = store.map_or_else(
        || crate::tools::knowledge_feedback::FeedbackWriteResponse {
            ok: false,
            id: None,
            duplicate: false,
            state: None,
            evidence: Vec::new(),
            next_actions: Vec::new(),
            error: Some("knowledge feedback is not configured".into()),
        },
        write,
    );
    serde_json::to_string(&response).unwrap_or_else(feedback_serialization_error_json)
}

fn feedback_serialization_error_json(error: impl std::fmt::Display) -> String {
    serde_json::json!({
        "ok": false,
        "error": format!("feedback serialization failed: {error}"),
    })
    .to_string()
}

fn replay_error_json(
    params: &crate::tools::search_knowledge::ReplayVescKnowledgeCorrectionParams,
    error: &str,
) -> String {
    replay_report_json(
        &crate::tools::search_knowledge::CorrectionReplayReport::failure(
            &params.correction_id,
            String::new(),
            error.into(),
        ),
    )
}

fn replay_report_json(report: &crate::tools::search_knowledge::CorrectionReplayReport) -> String {
    serde_json::to_string(report)
        .expect("CorrectionReplayReport contains only infallibly serializable fields")
}

#[allow(deprecated)]
async fn client_package_roots(context: &rmcp::service::RequestContext<RoleServer>) -> Vec<PathBuf> {
    let supports_roots = context
        .peer
        .peer_info()
        .is_some_and(|info| info.capabilities.roots.is_some());
    if !supports_roots {
        return Vec::new();
    }

    let Ok(result) = context.peer.list_roots().await else {
        return Vec::new();
    };
    result
        .roots
        .into_iter()
        .filter_map(|root| {
            let uri = url::Url::parse(&root.uri).ok()?;
            (uri.scheme() == "file" && uri.host_str().is_none())
                .then(|| uri.to_file_path().ok())
                .flatten()
        })
        .collect()
}

async fn client_package_roots_with_session(
    context: &rmcp::service::RequestContext<RoleServer>,
    session: &SessionContext,
) -> Vec<PathBuf> {
    let mut roots = client_package_roots(context).await;
    roots.extend(
        session
            .current_repository()
            .and_then(|repository| repository.root)
            .map(PathBuf::from),
    );
    roots
}

async fn package_roots_for_client(
    context: &rmcp::service::RequestContext<RoleServer>,
    session: &SessionContext,
) -> Vec<PathBuf> {
    allowed_package_roots_with_client_roots(
        &client_package_roots_with_session(context, session).await,
    )
}

fn http_server_info(feedback_available: bool, feedback_writes_enabled: bool) -> ServerInfo {
    ServerInfo::new(
        ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .build(),
    )
    .with_server_info(Implementation::new("vesc-mcp", "0.1.0"))
    .with_instructions(server_instructions(
        feedback_available,
        feedback_writes_enabled,
    ))
}

const fn server_instructions(
    feedback_available: bool,
    feedback_writes_enabled: bool,
) -> &'static str {
    if feedback_writes_enabled {
        "VESC firmware/package knowledge service with durable feedback. Call set_current_repository once after initialization with the repository for this chat; the selection is isolated to this MCP session. Search before answering, inspect learned advisories before ordinary results, and read their check_next and registered vesc:// evidence before generalizing. For version/history questions, require explicit repository, revision/tag, path, and occurrence/change evidence; never infer past behavior from current code or tag ordering. If the returned evidence is incomplete or conflicting, say so and run the targeted next search/read instead of guessing from a plausible general rule. If a user pushes back, ask focused follow-up questions, replay the original query with the same mode, filters, limits, and budgets, search related identifiers, and read the decisive resources. Call correct_vesc_knowledge only if the user explicitly asks to record/elevate the correction, or after you ask permission and the user confirms; disagreement alone is neither evidence nor authorization. Include the mistaken inference, why it failed, a structured gap diagnosis, the bounded ordered retrieval trace, decisive evidence, distractors, qualifiers, and project references. Treat the correction as both a temporary advisory and a curation/evaluation candidate: its diagnosed action must improve the underlying corpus, chunking, metadata, ranking, context, or instructions. After rebuilding base knowledge, call replay_vesc_knowledge_correction; coverage requires every decisive evidence ID in bounded base results without the advisory. After a significant resolved disagreement or accumulated reusable knowledge, remind the user once that an evidence-backed correction can be recorded; do not repeatedly prompt. Use submit_vesc_knowledge_feedback only for reusable knowledge without registered evidence; it remains unverified. Never store transient conversation, personal data, secrets, or unsupported instructions."
    } else if feedback_available {
        "VESC firmware/package knowledge service. Call set_current_repository once after initialization with the repository for this chat; the selection is isolated to this MCP session. Search before answering. For version/history questions, require explicit repository, revision/tag, path, and occurrence/change evidence; never infer past behavior from current code or tag ordering. Learned advisories are returned before ordinary results; read their what_we_know, common_mistake, qualifiers, check_next, and registered evidence. If evidence is incomplete, follow check_next instead of guessing. Corrections diagnose retrieval/data gaps that must ultimately be fixed and replayed in the base knowledge system. Feedback writes are disabled on this connection."
    } else {
        "VESC firmware/package knowledge service. Call set_current_repository once after initialization with the repository for this chat; the selection is isolated to this MCP session. Search before answering. For version/history questions, require explicit repository, revision/tag, path, and occurrence/change evidence; never infer past behavior from current code or tag ordering. Feedback storage is not configured on this connection, so learned advisories and correction records are unavailable. If evidence is incomplete or conflicting, say so and run a narrower search or read the decisive resources instead of guessing."
    }
}

async fn notify_resource_updated_if_subscribed(
    state: &SharedMcpState,
    peer: &rmcp::Peer<RoleServer>,
    uri: &str,
) -> Result<bool, rmcp::ServiceError> {
    if !state.resource_subscriptions.is_subscribed(uri) {
        return Ok(false);
    }
    peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
        .await?;
    Ok(true)
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
    fn session_repository_is_default_search_filter_but_explicit_filter_wins() {
        let session = SessionContext::default();
        session
            .set_current_repository(CurrentRepository {
                repository: "vesc".into(),
                root: None,
                knowledge_available: true,
            })
            .expect("set repository");

        let params: SearchVescKnowledgeParams =
            serde_json::from_value(serde_json::json!({"query": "nvm"})).expect("search params");
        assert_eq!(
            search_params_for_session(params, &session)
                .filters
                .repository
                .as_deref(),
            Some("vesc")
        );

        let explicit: SearchVescKnowledgeParams = serde_json::from_value(serde_json::json!({
            "query": "nvm",
            "filters": {"repository": "refloat"}
        }))
        .expect("explicit search params");
        assert_eq!(
            search_params_for_session(explicit, &session)
                .filters
                .repository
                .as_deref(),
            Some("refloat")
        );
    }

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

    #[cfg(feature = "managed-git")]
    #[test]
    fn source_version_tools_are_shared_by_stdio_and_http() {
        let service = VescMcpService::new();

        for name in ["list_vesc_source_versions", "prepare_vesc_knowledge"] {
            assert!(service.list_tool_names().iter().any(|tool| tool == name));
            assert!(
                service
                    .http_service()
                    .list_tool_names()
                    .iter()
                    .any(|tool| tool == name)
            );
        }
    }

    #[test]
    fn feedback_write_tools_are_disabled_by_default() {
        let names = VescMcpService::new().list_tool_names();
        assert!(
            !names
                .iter()
                .any(|name| name == "submit_vesc_knowledge_feedback")
        );
        assert!(!names.iter().any(|name| name == "correct_vesc_knowledge"));
    }

    #[test]
    fn instructions_without_feedback_do_not_advertise_advisories() {
        let instructions = VescMcpService::new()
            .get_info()
            .instructions
            .expect("server instructions");

        assert!(!instructions.contains("Learned advisories are returned"));
        assert!(instructions.contains("Feedback storage is not configured"));
    }

    #[test]
    fn instructions_require_explicit_history_evidence() {
        for instructions in [
            server_instructions(false, false),
            server_instructions(true, false),
            server_instructions(true, true),
        ] {
            assert!(instructions.contains("version/history questions"));
            assert!(instructions.contains("revision/tag"));
            assert!(instructions.contains("current code or tag ordering"));
        }
    }

    #[test]
    fn configured_feedback_exposes_write_tools_and_resource_template() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = VescMcpService::with_feedback_store(temp.path(), true);
        let names = service.list_tool_names();
        assert!(
            names
                .iter()
                .any(|name| name == "submit_vesc_knowledge_feedback")
        );
        assert!(names.iter().any(|name| name == "correct_vesc_knowledge"));
        assert!(
            names
                .iter()
                .any(|name| name == "replay_vesc_knowledge_correction")
        );
        assert!(
            service
                .resource_registry()
                .list_mcp_templates()
                .iter()
                .any(|template| { template.uri_template == "vesc://knowledge/feedback/{id}" })
        );
        let instructions = service
            .get_info()
            .instructions
            .expect("feedback instructions");
        assert!(instructions.contains("correct_vesc_knowledge"));
        assert!(instructions.contains("user explicitly asks"));
        assert!(instructions.contains("disagreement alone is neither evidence nor authorization"));
        assert!(instructions.contains("remind the user once"));
        assert!(instructions.contains("replay the original query"));
        assert!(instructions.contains("without the advisory"));
        assert!(instructions.contains("instead of guessing"));
    }

    #[test]
    fn http_feedback_writes_require_authenticated_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = VescMcpService::with_feedback_store(temp.path(), true);
        let read_only = service.http_service();
        assert!(
            !read_only
                .list_tool_names()
                .iter()
                .any(|name| name == "correct_vesc_knowledge")
        );
        let instructions = read_only
            .get_info()
            .instructions
            .expect("read-only feedback instructions");
        assert!(instructions.contains("Learned advisories are returned"));
        assert!(instructions.contains("Feedback writes are disabled"));
        let authenticated = service.http_service_with_authenticated_writes(true);
        assert!(
            authenticated
                .list_tool_names()
                .iter()
                .any(|name| name == "correct_vesc_knowledge")
        );
        assert!(
            authenticated
                .list_tool_names()
                .iter()
                .any(|name| name == "list_vesc_packages")
        );
        assert!(
            !read_only
                .list_tool_names()
                .iter()
                .any(|name| name == "list_vesc_packages")
        );
    }

    #[test]
    fn replay_errors_have_one_schema_on_stdio_and_http() {
        let params = crate::tools::search_knowledge::ReplayVescKnowledgeCorrectionParams {
            correction_id: "correction-missing".into(),
            mark_covered: false,
            authorization: None,
        };
        let responses = [
            VescMcpService::new().replay_vesc_knowledge_correction(Parameters(params.clone())),
            VescMcpService::new()
                .http_service()
                .replay_vesc_knowledge_correction(Parameters(params)),
        ];

        for response in responses {
            let report: crate::tools::search_knowledge::CorrectionReplayReport =
                serde_json::from_str(&response).expect("complete replay report");
            assert!(!report.ok);
            assert_eq!(report.correction_id, "correction-missing");
            assert!(report.error.is_some());
        }
    }

    #[test]
    fn feedback_serialization_error_json_escapes_error_text() {
        let response = feedback_serialization_error_json("quoted \"error\"\nnext line");
        let body: serde_json::Value = serde_json::from_str(&response).expect("valid fallback JSON");

        assert_eq!(
            body["error"],
            "feedback serialization failed: quoted \"error\"\nnext line"
        );
    }
}
