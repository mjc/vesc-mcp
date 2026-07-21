use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use vesc_mcp_core::{HttpMcpService, VescMcpService};

const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
const DEFAULT_PATH: &str = "/mcp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpServerConfig {
    pub bind: SocketAddr,
    pub path: String,
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub auth_token: Option<String>,
}

/// A bound HTTP listener that has not started serving yet.
pub struct BoundHttpServer {
    config: HttpServerConfig,
    listener: TcpListener,
}

impl BoundHttpServer {
    /// Serve MCP requests until cancellation or a transport failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP transport stops with a serving error.
    pub async fn serve(self) -> anyhow::Result<()> {
        let cancellation_token = CancellationToken::new();
        let service = VescMcpService::new()
            .http_service_with_authenticated_writes(self.config.auth_token.is_some());
        let router = router(&self.config, service, &cancellation_token);
        tracing::info!(bind = %self.config.bind, path = %self.config.path, "serving Streamable HTTP MCP");
        axum::serve(self.listener, router)
            .with_graceful_shutdown(cancellation_token.cancelled_owned())
            .await?;
        Ok(())
    }
}

impl HttpServerConfig {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            bind: env::var("VESC_MCP_HTTP_BIND")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_BIND),
            path: env::var("VESC_MCP_HTTP_PATH").unwrap_or_else(|_| DEFAULT_PATH.into()),
            allowed_hosts: split_list(
                "VESC_MCP_HTTP_ALLOWED_HOSTS",
                ["localhost", "127.0.0.1", "::1"],
            ),
            allowed_origins: split_list("VESC_MCP_HTTP_ALLOWED_ORIGINS", []),
            auth_token: env::var("VESC_MCP_HTTP_AUTH_TOKEN").ok(),
        }
    }
}

#[derive(Clone)]
struct AuthState {
    token: Option<Arc<str>>,
}

pub fn router(
    config: &HttpServerConfig,
    service: HttpMcpService,
    cancellation_token: &CancellationToken,
) -> Router {
    let http_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(config.allowed_hosts.clone())
        .with_allowed_origins(config.allowed_origins.clone())
        .with_cancellation_token(cancellation_token.child_token());
    let sessions = Arc::new(LocalSessionManager::default());
    let transport =
        StreamableHttpService::new(move || Ok(service.fresh_session()), sessions, http_config);
    Router::new()
        .nest_service(&config.path, transport)
        .layer(middleware::from_fn_with_state(
            AuthState {
                token: config.auth_token.as_deref().map(Arc::from),
            },
            require_auth,
        ))
}

/// Run the Streamable HTTP server until cancellation or a serving failure.
///
/// # Errors
///
/// Returns an error when the listen address cannot be bound or the HTTP
/// transport stops with a serving error.
pub async fn run(config: HttpServerConfig) -> anyhow::Result<()> {
    bind(config).await?.serve().await
}

/// Bind the configured listen address without constructing the MCP service.
///
/// # Errors
///
/// Returns an error when the listen address cannot be bound.
pub async fn bind(config: HttpServerConfig) -> anyhow::Result<BoundHttpServer> {
    let listener = TcpListener::bind(config.bind).await?;
    Ok(BoundHttpServer { config, listener })
}

async fn require_auth(
    State(state): State<AuthState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(token) = state.token else {
        return next.run(request).await;
    };
    let expected = format!("Bearer {token}");
    if request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        == Some(expected.as_str())
    {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "missing or invalid bearer token",
    )
        .into_response()
}

fn split_list<const N: usize>(name: &str, default: [&str; N]) -> Vec<String> {
    env::var(name).map_or_else(
        |_| default.into_iter().map(str::to_owned).collect(),
        |value| {
            value
                .split([',', ';'])
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_defaults_are_local_and_mcp_path() {
        let config = HttpServerConfig {
            bind: DEFAULT_BIND,
            path: DEFAULT_PATH.into(),
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            allowed_origins: Vec::new(),
            auth_token: None,
        };
        assert_eq!(config.bind.ip().to_string(), "127.0.0.1");
        assert_eq!(config.path, "/mcp");
        assert!(config.auth_token.is_none());
    }
}
