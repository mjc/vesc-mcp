use std::net::SocketAddr;

use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo, ReadResourceRequestParams},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use vesc_mcp_core::test_support::VersionedKnowledgeFixture;
use vesc_mcp_core::{VescMcpService, resources::VESC_C_IF_URI};
use vesc_mcp_server::http::{HttpServerConfig, router};

#[tokio::test]
async fn streamable_http_shares_safe_tools_and_resources_between_clients() -> anyhow::Result<()> {
    let cancellation = CancellationToken::new();
    let config = HttpServerConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        path: "/mcp".into(),
        allowed_hosts: vec!["127.0.0.1".into()],
        allowed_origins: Vec::new(),
        auth_token: None,
    };
    let app = router(&config, VescMcpService::new().http_service(), &cancellation);
    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(server_cancellation.cancelled_owned())
            .await
    });
    let endpoint = format!("http://{address}/mcp");

    let first = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(endpoint.clone()),
        ))
        .await?;
    let second = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(endpoint),
        ))
        .await?;

    let tools = first.list_all_tools().await?;
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        vec![
            "list_vesc_source_versions",
            "ping",
            "prepare_vesc_knowledge",
            "replay_vesc_knowledge_correction",
            "search_vesc_knowledge",
        ]
    );
    let resources = first.list_all_resources().await?;
    assert!(
        resources
            .iter()
            .any(|resource| resource.uri == VESC_C_IF_URI)
    );
    let resource = first
        .read_resource(ReadResourceRequestParams::new(VESC_C_IF_URI))
        .await?;
    assert!(!resource.contents.is_empty());

    let arguments = serde_json::json!({"message": "shared"})
        .as_object()
        .cloned()
        .expect("object arguments");
    let response = second
        .call_tool(CallToolRequestParams::new("ping").with_arguments(arguments))
        .await?;
    assert_eq!(response.is_error, Some(false));

    let discovery_arguments = serde_json::Map::new();
    let first_catalog = first
        .call_tool(
            CallToolRequestParams::new("list_vesc_source_versions")
                .with_arguments(discovery_arguments.clone()),
        )
        .await?;
    let second_catalog = second
        .call_tool(
            CallToolRequestParams::new("list_vesc_source_versions")
                .with_arguments(discovery_arguments),
        )
        .await?;
    assert_eq!(first_catalog.content, second_catalog.content);

    first.cancel().await?;
    second.cancel().await?;
    cancellation.cancel();
    server.await??;
    Ok(())
}

#[tokio::test]
async fn two_http_clients_prepare_one_shared_snapshot_artifact() -> anyhow::Result<()> {
    let fixture = VersionedKnowledgeFixture::new().await;
    let cancellation = CancellationToken::new();
    let config = HttpServerConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        path: "/mcp".into(),
        allowed_hosts: vec!["127.0.0.1".into()],
        allowed_origins: Vec::new(),
        auth_token: None,
    };
    let service = VescMcpService::with_knowledge_config(fixture.knowledge().clone());
    let app = router(&config, service.http_service(), &cancellation);
    let listener = TcpListener::bind(config.bind).await?;
    let address = listener.local_addr()?;
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(server_cancellation.cancelled_owned())
            .await
    });
    let endpoint = format!("http://{address}/mcp");
    let first = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(endpoint.clone()),
        ))
        .await?;
    let second = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(endpoint),
        ))
        .await?;
    let arguments = VersionedKnowledgeFixture::selection()
        .as_object()
        .cloned()
        .expect("selection object");

    let (left, right) = tokio::join!(
        first.call_tool(
            CallToolRequestParams::new("prepare_vesc_knowledge").with_arguments(arguments.clone())
        ),
        second.call_tool(
            CallToolRequestParams::new("prepare_vesc_knowledge").with_arguments(arguments)
        )
    );
    let responses = [left?, right?]
        .map(|response| {
            serde_json::from_str::<serde_json::Value>(
                response
                    .content
                    .first()
                    .and_then(|content| content.as_text())
                    .expect("prepare text response")
                    .text
                    .as_str(),
            )
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(responses[0]["snapshot_id"], responses[1]["snapshot_id"]);
    assert_eq!(
        responses
            .iter()
            .filter(|response| response["status"] == "built")
            .count(),
        1
    );
    assert_eq!(
        responses
            .iter()
            .filter(|response| matches!(
                response["status"].as_str(),
                Some("reused" | "deduplicated")
            ))
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_dir(fixture.data_root().join("artifacts"))?.count(),
        1
    );

    first.cancel().await?;
    second.cancel().await?;
    cancellation.cancel();
    server.await??;
    Ok(())
}
