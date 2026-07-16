use std::env;
use std::fs;
use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--benchmark-search") {
        run_benchmark(&args)?;
        return Ok(());
    }

    if args.iter().any(|arg| arg == "--http") {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
        vesc_mcp_server::http::run(vesc_mcp_server::http::HttpServerConfig::from_env()).await?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    vesc_mcp_core::server::run_stdio_server().await
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
