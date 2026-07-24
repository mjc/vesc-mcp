use std::env;
use std::fs;
use std::future::Future;
use std::path::PathBuf;

use tracing_subscriber::EnvFilter;
use vesc_mcp_core::config::{McpConfig, SemanticIngestionProvider};
use vesc_mcp_core::managed_git::ManagedGitStore;
use vesc_mcp_core::managed_repositories::{DataRoot, KnowledgeDataLayout, RepositoryPolicy};
use vesc_mcp_core::managed_snapshots::{KnowledgeSnapshotStore, SnapshotDisposition};
use vesc_mcp_core::preparation_status::{
    KnowledgePreparationStatus, PreparationPhase, PreparationState, write_preparation_status,
};

struct PreparationReporter {
    data_root: PathBuf,
    repositories_total: usize,
    repositories_completed: usize,
    finished: bool,
}

impl PreparationReporter {
    fn new(data_root: PathBuf, repositories_total: usize) -> Self {
        let reporter = Self {
            data_root,
            repositories_total,
            repositories_completed: 0,
            finished: false,
        };
        reporter.publish(&KnowledgePreparationStatus::preparing(
            PreparationPhase::Starting,
            0,
            repositories_total,
        ));
        reporter
    }

    fn repositories_synchronized(&mut self, completed: usize) {
        self.repositories_completed = completed;
        self.publish(&KnowledgePreparationStatus::preparing(
            PreparationPhase::SynchronizingRepositories,
            completed,
            self.repositories_total,
        ));
    }

    fn indexing(&mut self) {
        self.repositories_completed = self.repositories_total;
        self.publish(&KnowledgePreparationStatus::preparing(
            PreparationPhase::Indexing,
            self.repositories_completed,
            self.repositories_total,
        ));
    }

    fn finish(&mut self, state: PreparationState) {
        self.publish(&KnowledgePreparationStatus::finished(
            state,
            self.repositories_completed,
            self.repositories_total,
        ));
        self.finished = true;
    }

    fn publish(&self, status: &KnowledgePreparationStatus) {
        if let Err(error) = write_preparation_status(&self.data_root, status) {
            tracing::warn!(%error, "could not publish knowledge preparation status");
        }
    }
}

impl Drop for PreparationReporter {
    fn drop(&mut self) {
        if !self.finished {
            self.publish(&KnowledgePreparationStatus::finished(
                PreparationState::Failed,
                self.repositories_completed,
                self.repositories_total,
            ));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StartupPolicy {
    refresh: bool,
    eager_index: bool,
    allow_offline_restart: bool,
}

impl StartupPolicy {
    fn from_args(args: &[String]) -> Self {
        Self {
            refresh: !args.iter().any(|arg| arg == "--skip-repository-refresh"),
            eager_index: !args.iter().any(|arg| arg == "--skip-eager-index"),
            allow_offline_restart: !args.iter().any(|arg| arg == "--require-fresh-repositories"),
        }
    }
}

fn migraphx_cache_path(
    data_root: Option<&DataRoot>,
    provider: Option<SemanticIngestionProvider>,
) -> Option<PathBuf> {
    match (data_root, provider) {
        (Some(root), Some(SemanticIngestionProvider::Migraphx)) => {
            Some(root.as_path().join("migraphx-cache"))
        }
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn configure_migraphx_cache() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    const CACHE_ENV: &str = "ORT_MIGRAPHX_MODEL_CACHE_PATH";
    if env::var_os(CACHE_ENV).is_some() {
        return Ok(());
    }
    let config = McpConfig::load();
    let provider = config
        .knowledge
        .semantic_ingestion
        .as_ref()
        .map(|ingestion| ingestion.provider);
    let Some(path) = migraphx_cache_path(config.knowledge.data_root.as_ref(), provider) else {
        return Ok(());
    };
    fs::create_dir_all(&path)?;
    let error = std::process::Command::new(env::current_exe()?)
        .args(env::args_os().skip(1))
        .env(CACHE_ENV, path)
        .exec();
    Err(error.into())
}

#[cfg(not(target_os = "linux"))]
fn configure_migraphx_cache() -> anyhow::Result<()> {
    Ok(())
}

fn main() -> anyhow::Result<()> {
    configure_migraphx_cache()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--benchmark-search") {
        run_benchmark(&args)?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let startup_policy = StartupPolicy::from_args(&args);
    if args.iter().any(|arg| arg == "--refresh-repositories") {
        synchronize_managed_repositories(startup_policy).await?;
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--http") {
        run_http(
            vesc_mcp_server::http::HttpServerConfig::from_env(),
            synchronize_managed_repositories(startup_policy),
        )
        .await?;
        return Ok(());
    }

    synchronize_managed_repositories(startup_policy).await?;
    vesc_mcp_core::server::run_stdio_server().await
}

async fn run_http<F>(
    config: vesc_mcp_server::http::HttpServerConfig,
    preparation: F,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let server = vesc_mcp_server::http::bind(config).await?;
    tokio::spawn(async move {
        if let Err(error) = preparation.await {
            tracing::error!(%error, "managed repository preparation failed");
        }
    });
    server.serve().await
}

#[allow(clippy::too_many_lines)]
async fn synchronize_managed_repositories(policy: StartupPolicy) -> anyhow::Result<()> {
    let config = vesc_mcp_core::config::McpConfig::load();
    if config.knowledge.repositories.is_empty() {
        return Ok(());
    }
    let data_root = config
        .knowledge
        .data_root
        .clone()
        .ok_or_else(|| anyhow::anyhow!("managed repositories require a data root"))?;
    let mut reporter = PreparationReporter::new(
        data_root.as_path().to_owned(),
        config.knowledge.repositories.iter().len(),
    );
    let layout = KnowledgeDataLayout::new(data_root);
    if policy.refresh {
        let store = ManagedGitStore::new(layout.clone());
        for (completed, (id, result)) in store
            .startup_sync(&config.knowledge.repositories)
            .await
            .into_iter()
            .enumerate()
        {
            match result {
                Ok(outcome) => {
                    if let Some(warning) = outcome.warning {
                        if !policy.allow_offline_restart {
                            return Err(anyhow::anyhow!(
                                "repository {id} refresh failed and offline restart is disabled: {warning}"
                            ));
                        }
                        tracing::warn!(repository = %id, %warning, "using stale managed repository catalog");
                    } else {
                        tracing::info!(
                            repository = %id,
                            disposition = ?outcome.disposition,
                            refs = outcome.catalog.refs.len(),
                            "synchronized managed repository"
                        );
                    }
                }
                Err(error) => {
                    let required = config
                        .knowledge
                        .repositories
                        .iter()
                        .find(|repository| repository.id() == &id)
                        .is_some_and(|repository| {
                            repository.policy() == RepositoryPolicy::Required
                        });
                    if required {
                        return Err(anyhow::anyhow!("required repository {id} failed: {error}"));
                    }
                    tracing::warn!(repository = %id, %error, "optional managed repository unavailable");
                }
            }
            reporter.repositories_synchronized(completed.saturating_add(1));
        }
    }
    if !policy.eager_index {
        reporter.finish(PreparationState::Stale);
        return Ok(());
    }

    reporter.indexing();
    let prepared = KnowledgeSnapshotStore::new(layout)
        .with_semantic_config(&config.knowledge)?
        .prepare_configured(&config.knowledge.repositories, &config.knowledge.prewarm)
        .await?;
    if prepared.default.disposition == SnapshotDisposition::Stale {
        if !policy.allow_offline_restart {
            return Err(anyhow::anyhow!(
                "default knowledge snapshot is stale and offline restart is disabled"
            ));
        }
        tracing::warn!(
            snapshot = %prepared.default.manifest.id.as_str(),
            "using stale default knowledge snapshot"
        );
    } else {
        tracing::info!(
            snapshot = %prepared.default.manifest.id.as_str(),
            disposition = ?prepared.default.disposition,
            "prepared default knowledge snapshot"
        );
    }
    for snapshot in prepared.prewarmed {
        tracing::info!(
            snapshot = %snapshot.manifest.id.as_str(),
            disposition = ?snapshot.disposition,
            "prepared historical knowledge snapshot"
        );
    }
    reporter.finish(
        if prepared.default.disposition == SnapshotDisposition::Stale {
            PreparationState::Stale
        } else {
            PreparationState::Ready
        },
    );
    Ok(())
}

fn run_benchmark(args: &[String]) -> anyhow::Result<()> {
    let suite = argument_value(args, "--suite").map_or_else(
        || PathBuf::from("tests/evaluation/v1/queries.json"),
        PathBuf::from,
    );
    let raw = fs::read_to_string(&suite)?;
    let values: Vec<serde_json::Value> = serde_json::from_str(&raw)?;
    let queries: Vec<String> = values
        .into_iter()
        .filter_map(|value| {
            value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let warmup = argument_value(args, "--warmup")
        .as_deref()
        .unwrap_or("3")
        .parse::<usize>()?;
    let repetitions = argument_value(args, "--repetitions")
        .as_deref()
        .unwrap_or("10")
        .parse::<usize>()?;
    let format = argument_value(args, "--format").unwrap_or_else(|| "text".into());
    let mut config = vesc_mcp_core::config::McpConfig::load().knowledge.clone();
    if let Some(artifact) = argument_value(args, "--artifact") {
        config.artifact_path = Some(PathBuf::from(artifact));
    }
    let report =
        vesc_mcp_core::benchmark::benchmark_search(&config, &queries, warmup, repetitions)?;
    match format.as_str() {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => {
            println!("mode: {:?}", report.mode);
            println!(
                "machine: {} {} target={}",
                report.machine.os, report.machine.arch, report.machine.rust_target
            );
            println!(
                "iterations: warmup={} repetitions={} queries={}",
                report.warmup_iterations, report.repetitions, report.query_count
            );
            let timing = &report.handler_and_serialization;
            println!(
                "mcp-handler-json: samples={} min-us={} p50-us={} p95-us={} max-us={}",
                timing.samples, timing.min_us, timing.p50_us, timing.p95_us, timing.max_us
            );
            let bytes = &report.response_bytes;
            println!(
                "response-bytes: samples={} min={} p50={} p95={} max={}",
                bytes.samples, bytes.min_bytes, bytes.p50_bytes, bytes.p95_bytes, bytes.max_bytes
            );
            println!(
                "rss-retained-bytes: before={:?} after={:?} delta={:?}",
                report.rss_before_queries_bytes,
                report.rss_after_queries_bytes,
                report.rss_retained_delta_bytes
            );
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
        }
        other => anyhow::bail!("unsupported benchmark format {other:?}; use text or json"),
    }
    Ok(())
}

fn argument_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use vesc_mcp_core::config::SemanticIngestionProvider;
    use vesc_mcp_core::managed_repositories::DataRoot;

    use super::{PreparationReporter, StartupPolicy, migraphx_cache_path, run_http};

    #[test]
    fn migraphx_cache_uses_the_configured_data_root() {
        let root = DataRoot::new(PathBuf::from("/var/lib/vesc-mcp")).expect("absolute data root");

        assert_eq!(
            migraphx_cache_path(Some(&root), Some(SemanticIngestionProvider::Migraphx)),
            Some(PathBuf::from("/var/lib/vesc-mcp/migraphx-cache"))
        );
        assert_eq!(
            migraphx_cache_path(Some(&root), Some(SemanticIngestionProvider::Cpu)),
            None
        );
    }

    #[test]
    fn startup_policy_defaults_to_refresh_eager_and_offline_fallback() {
        assert_eq!(
            StartupPolicy::from_args(&[]),
            StartupPolicy {
                refresh: true,
                eager_index: true,
                allow_offline_restart: true,
            }
        );
    }

    #[test]
    fn startup_policy_flags_disable_work_or_require_fresh_sources() {
        let args = [
            "--skip-repository-refresh".to_owned(),
            "--skip-eager-index".to_owned(),
            "--require-fresh-repositories".to_owned(),
        ];

        assert_eq!(
            StartupPolicy::from_args(&args),
            StartupPolicy {
                refresh: false,
                eager_index: false,
                allow_offline_restart: false,
            }
        );
    }

    #[tokio::test]
    async fn http_binds_before_repository_preparation() {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let bind = reservation.local_addr().expect("reserved address");
        drop(reservation);
        let started = Arc::new(AtomicBool::new(false));
        let preparing = Arc::clone(&started);
        let config = vesc_mcp_server::http::HttpServerConfig {
            bind,
            path: "/mcp".into(),
            allowed_hosts: vec!["127.0.0.1".into()],
            allowed_origins: Vec::new(),
            auth_token: None,
        };

        let server = tokio::spawn(run_http(config, async move {
            preparing.store(true, Ordering::Release);
            std::future::pending::<anyhow::Result<()>>().await
        }));
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        tokio::net::TcpStream::connect(bind)
            .await
            .expect("HTTP listener is available while preparation is pending");
        server.abort();
    }

    #[test]
    fn unfinished_preparation_is_published_as_failed() {
        let root = tempfile::tempdir().expect("data root");
        drop(PreparationReporter::new(root.path().to_owned(), 3));

        let status = vesc_mcp_core::preparation_status::read_preparation_status(root.path())
            .expect("published status");
        assert_eq!(
            status.state,
            vesc_mcp_core::preparation_status::PreparationState::Failed
        );
    }
}
