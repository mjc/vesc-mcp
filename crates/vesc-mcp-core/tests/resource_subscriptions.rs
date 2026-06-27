//! Integration tests for MCP resource subscriptions on [`VescMcpService`].

use std::sync::Arc;

use rmcp::{
    ClientHandler, ServerHandler, ServiceExt,
    model::{ResourceUpdatedNotificationParam, SubscribeRequestParams},
    service::RequestContext,
};
use tokio::sync::Notify;
use vesc_mcp_core::VescMcpService;
use vesc_mcp_core::resources::REFLOAT_MINIMAL_MANIFEST_URI;

#[derive(Clone)]
struct SubscriptionTestServer {
    inner: VescMcpService,
}

impl ServerHandler for SubscriptionTestServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        self.inner.get_info()
    }

    async fn list_resources(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        self.inner.list_resources(request, context).await
    }

    async fn list_resource_templates(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, rmcp::ErrorData> {
        self.inner.list_resource_templates(request, context).await
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResult, rmcp::ErrorData> {
        self.inner.read_resource(request, context).await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.inner
            .subscribe(request.clone(), context.clone())
            .await?;
        self.inner
            .notify_resource_updated_if_subscribed(&context.peer, &request.uri)
            .await
            .map_err(|err| {
                rmcp::ErrorData::internal_error(format!("notify failed: {err}"), None)
            })?;
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: rmcp::model::UnsubscribeRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.inner.unsubscribe(request, context).await
    }
}

struct SubscriptionTestClient {
    receive_signal: Arc<Notify>,
    updated_uri: Arc<tokio::sync::Mutex<Option<String>>>,
}

impl ClientHandler for SubscriptionTestClient {
    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: rmcp::service::NotificationContext<rmcp::RoleClient>,
    ) {
        *self.updated_uri.lock().await = Some(params.uri);
        self.receive_signal.notify_one();
    }
}

#[test]
fn service_rejects_subscription_for_unknown_resource_uri() {
    let service = VescMcpService::new();
    assert!(
        !service
            .resource_registry()
            .is_readable("vesc://catalog/does/not/exist")
    );
}

#[tokio::test]
async fn service_subscribe_emits_resource_updated_notification() -> anyhow::Result<()> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = SubscriptionTestServer {
        inner: VescMcpService::new(),
    };

    tokio::spawn(async move {
        let running = server.serve(server_transport).await?;
        running.waiting().await?;
        anyhow::Ok(())
    });

    let receive_signal = Arc::new(Notify::new());
    let updated_uri = Arc::new(tokio::sync::Mutex::new(None));
    let client = SubscriptionTestClient {
        receive_signal: receive_signal.clone(),
        updated_uri: updated_uri.clone(),
    }
    .serve(client_transport)
    .await?;

    client
        .subscribe(SubscribeRequestParams::new(REFLOAT_MINIMAL_MANIFEST_URI))
        .await?;

    tokio::time::timeout(std::time::Duration::from_secs(5), receive_signal.notified()).await?;

    assert_eq!(
        updated_uri.lock().await.as_deref(),
        Some(REFLOAT_MINIMAL_MANIFEST_URI)
    );

    client.cancel().await?;
    Ok(())
}
