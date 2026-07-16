//! In-process MCP search benchmark.

use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config::KnowledgeConfig;
use crate::tools::search_knowledge::{
    SearchMode, SearchVescKnowledgeFilters, SearchVescKnowledgeParams,
    search_vesc_knowledge_tool_with_config,
};

/// Percentiles over elapsed MCP handler/serialization samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimingDistribution {
    pub samples: usize,
    pub min_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

/// Percentiles over serialized MCP response sizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteDistribution {
    pub samples: usize,
    pub min_bytes: u64,
    pub p50_bytes: u64,
    pub p95_bytes: u64,
    pub max_bytes: u64,
}

/// Machine profile for interpreting the benchmark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    pub os: String,
    pub arch: String,
    pub rust_target: String,
}

/// Stable report for the in-process MCP search boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpBenchmarkReport {
    pub schema: u16,
    pub mode: SearchMode,
    pub warmup_iterations: usize,
    pub repetitions: usize,
    pub query_count: usize,
    pub handler_and_serialization: TimingDistribution,
    pub response_bytes: ByteDistribution,
    pub rss_before_bytes: Option<u64>,
    pub rss_after_bytes: Option<u64>,
    pub rss_delta_bytes: Option<i64>,
    pub machine: MachineProfile,
    pub warnings: Vec<String>,
}

/// Errors raised while measuring MCP search responses.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BenchmarkError {
    #[error("MCP benchmark requires at least one query")]
    EmptyQueries,
    #[error("MCP benchmark repetitions must be positive")]
    InvalidRepetitions,
    #[error("MCP response serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Measures the synchronous search handler and its JSON response serialization.
///
/// This intentionally excludes stdio transport scheduling; the server smoke
/// test covers that boundary separately. No network or wall-clock metadata is
/// included in the report.
///
/// # Errors
///
/// Returns [`BenchmarkError`] for invalid inputs or response serialization.
#[allow(clippy::too_many_lines)]
pub fn benchmark_search(
    config: &KnowledgeConfig,
    queries: &[String],
    warmup_iterations: usize,
    repetitions: usize,
) -> Result<McpBenchmarkReport, BenchmarkError> {
    if queries.is_empty() {
        return Err(BenchmarkError::EmptyQueries);
    }
    if repetitions == 0 {
        return Err(BenchmarkError::InvalidRepetitions);
    }

    let params = |query: &str| SearchVescKnowledgeParams {
        query: query.to_owned(),
        category: None,
        limit: 10,
        mode: Some(SearchMode::Lexical),
        filters: SearchVescKnowledgeFilters::default(),
        max_response_bytes: None,
        max_context_bytes: None,
    };
    for _ in 0..warmup_iterations {
        for query in queries {
            let response = search_vesc_knowledge_tool_with_config(&params(query), config);
            let _ = serde_json::to_vec(&response)?;
        }
    }

    let rss_before_bytes = process_rss_bytes();
    let mut timings = Vec::with_capacity(queries.len() * repetitions);
    let mut response_sizes = Vec::with_capacity(queries.len() * repetitions);
    for _ in 0..repetitions {
        for query in queries {
            let started = Instant::now();
            let response = search_vesc_knowledge_tool_with_config(&params(query), config);
            let bytes = serde_json::to_vec(&response)?;
            timings.push(elapsed_us(started));
            response_sizes.push(bytes.len() as u64);
        }
    }
    let rss_after_bytes = process_rss_bytes();
    let rss_delta_bytes = rss_before_bytes
        .zip(rss_after_bytes)
        .and_then(|(before, after)| {
            i64::try_from(after)
                .ok()?
                .checked_sub(i64::try_from(before).ok()?)
        });

    Ok(McpBenchmarkReport {
        schema: 1,
        mode: SearchMode::Lexical,
        warmup_iterations,
        repetitions,
        query_count: queries.len(),
        handler_and_serialization: TimingDistribution::from_samples(timings),
        response_bytes: ByteDistribution::from_samples(response_sizes),
        rss_before_bytes,
        rss_after_bytes,
        rss_delta_bytes,
        machine: machine_profile(),
        warnings: vec!["measures the in-process MCP handler, not stdio transport".into()],
    })
}

impl TimingDistribution {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        Self {
            samples: samples.len(),
            min_us: samples[0],
            p50_us: samples[nearest_rank(&samples, 50)],
            p95_us: samples[nearest_rank(&samples, 95)],
            max_us: samples[samples.len() - 1],
        }
    }
}

impl ByteDistribution {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        Self {
            samples: samples.len(),
            min_bytes: samples[0],
            p50_bytes: samples[nearest_rank(&samples, 50)],
            p95_bytes: samples[nearest_rank(&samples, 95)],
            max_bytes: samples[samples.len() - 1],
        }
    }
}

fn nearest_rank(samples: &[u64], percentile: usize) -> usize {
    ((percentile * samples.len()).saturating_add(99) / 100)
        .saturating_sub(1)
        .min(samples.len() - 1)
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn machine_profile() -> MachineProfile {
    MachineProfile {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        rust_target: rust_target().into(),
    }
}

const fn rust_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else {
        "unknown"
    }
}

fn process_rss_bytes() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(std::process::id().to_string())
        .output()
        .ok()?;
    let kilobytes = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_reports_mcp_response_shape() {
        let report = benchmark_search(
            &KnowledgeConfig::default(),
            &["lbm_add_extension".into()],
            1,
            2,
        )
        .expect("benchmark");
        assert_eq!(report.query_count, 1);
        assert_eq!(report.handler_and_serialization.samples, 2);
        assert_eq!(report.response_bytes.samples, 2);
        assert!(report.response_bytes.max_bytes > 0);
    }
}
