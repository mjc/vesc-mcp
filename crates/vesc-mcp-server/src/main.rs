use std::env;
use std::fs;
use std::path::PathBuf;

use tracing_subscriber::EnvFilter;
use vesc_mcp_core::managed_git::ManagedGitStore;
use vesc_mcp_core::managed_repositories::{KnowledgeDataLayout, RepositoryPolicy};
use vesc_mcp_core::managed_snapshots::{KnowledgeSnapshotStore, SnapshotDisposition};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--benchmark-search") {
        run_benchmark(&args)?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    synchronize_managed_repositories().await?;
    if args.iter().any(|arg| arg == "--refresh-repositories") {
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--http") {
        vesc_mcp_server::http::run(vesc_mcp_server::http::HttpServerConfig::from_env()).await?;
        return Ok(());
    }

    vesc_mcp_core::server::run_stdio_server().await
}

async fn synchronize_managed_repositories() -> anyhow::Result<()> {
    let config = vesc_mcp_core::config::McpConfig::load();
    if config.knowledge.repositories.is_empty() {
        return Ok(());
    }
    let data_root = config
        .knowledge
        .data_root
        .clone()
        .ok_or_else(|| anyhow::anyhow!("managed repositories require a data root"))?;
    let layout = KnowledgeDataLayout::new(data_root);
    let store = ManagedGitStore::new(layout.clone());
    for (id, result) in store.startup_sync(&config.knowledge.repositories).await {
        match result {
            Ok(outcome) => {
                if let Some(warning) = outcome.warning {
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
                    .is_some_and(|repository| repository.policy() == RepositoryPolicy::Required);
                if required {
                    return Err(anyhow::anyhow!("required repository {id} failed: {error}"));
                }
                tracing::warn!(repository = %id, %error, "optional managed repository unavailable");
            }
        }
    }
    let prepared = KnowledgeSnapshotStore::new(layout)
        .prepare_configured(&config.knowledge.repositories, &config.knowledge.prewarm)
        .await?;
    if prepared.default.disposition == SnapshotDisposition::Stale {
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
