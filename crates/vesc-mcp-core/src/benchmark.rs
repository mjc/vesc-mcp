//! In-process MCP search benchmark.

use std::collections::BTreeSet;
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config::KnowledgeConfig;
use crate::tools::search_knowledge::{
    SearchMode, SearchResponseDetail, SearchVescKnowledgeFilters, SearchVescKnowledgeParams,
    search_vesc_knowledge_json_with_config,
};

#[derive(Deserialize)]
struct BenchmarkSearchResponse {
    ok: bool,
    mode: SearchMode,
    detail: SearchResponseDetail,
    #[serde(default)]
    results: Vec<serde_json::Value>,
    #[serde(default)]
    warnings: Vec<String>,
    snapshot_id: Option<String>,
    error: Option<String>,
}

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
    pub detail: SearchResponseDetail,
    pub warmup_iterations: usize,
    pub repetitions: usize,
    pub query_count: usize,
    pub snapshot_ids: Vec<String>,
    pub result_counts: Vec<usize>,
    pub response_digests: Vec<String>,
    pub handler_and_serialization: TimingDistribution,
    pub response_bytes: ByteDistribution,
    pub evidence: BenchmarkEvidence,
    pub rss_before_queries_bytes: Option<u64>,
    pub rss_after_queries_bytes: Option<u64>,
    pub rss_retained_delta_bytes: Option<i64>,
    pub machine: MachineProfile,
    pub warnings: Vec<String>,
}

/// Counts the evidence-bearing fields exposed by the benchmarked response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkEvidence {
    pub result_rows: usize,
    pub provenance_rows: usize,
    pub distinct_source_paths: usize,
    pub occurrence_rows: usize,
    pub expanded_context_rows: usize,
    #[serde(skip)]
    seen_source_paths: BTreeSet<String>,
}

impl BenchmarkEvidence {
    fn observe(&mut self, results: &[serde_json::Value]) {
        for result in results {
            self.result_rows += 1;
            if result.get("provenance").is_some() {
                self.provenance_rows += 1;
            }
            if result.get("occurrence").is_some() {
                self.occurrence_rows += 1;
            }
            if result
                .get("explanation")
                .and_then(|explanation| explanation.get("expansion_reason"))
                .is_some()
            {
                self.expanded_context_rows += 1;
            }
            if let Some(path) = result
                .get("source")
                .and_then(|source| source.get("path"))
                .and_then(serde_json::Value::as_str)
            {
                self.seen_source_paths.insert(path.into());
            }
        }
        self.distinct_source_paths = self.seen_source_paths.len();
    }
}

/// Errors raised while measuring MCP search responses.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BenchmarkError {
    #[error("MCP benchmark requires at least one query")]
    EmptyQueries,
    #[error("MCP benchmark repetitions must be positive")]
    InvalidRepetitions,
    #[error("MCP benchmark response JSON was invalid: {0}")]
    ResponseJson(#[from] serde_json::Error),
    #[error("MCP benchmark search failed: {0}")]
    SearchFailed(String),
    #[error("MCP benchmark requested {expected:?}, but the response used {actual:?}")]
    UnexpectedMode {
        expected: SearchMode,
        actual: SearchMode,
    },
    #[error("MCP benchmark requested detail {expected:?}, but the response used {actual:?}")]
    UnexpectedDetail {
        expected: SearchResponseDetail,
        actual: SearchResponseDetail,
    },
    #[error("MCP benchmark search returned no results")]
    NoResults,
    #[error("MCP benchmark search returned warnings: {0}")]
    SearchWarnings(String),
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
    benchmark_search_mode(
        config,
        queries,
        warmup_iterations,
        repetitions,
        SearchMode::Lexical,
    )
}

/// Measures the synchronous search handler in an explicit retrieval mode.
///
/// # Errors
///
/// Returns [`BenchmarkError`] for invalid inputs or response serialization.
pub fn benchmark_search_mode(
    config: &KnowledgeConfig,
    queries: &[String],
    warmup_iterations: usize,
    repetitions: usize,
    mode: SearchMode,
) -> Result<McpBenchmarkReport, BenchmarkError> {
    benchmark_search_profile(
        config,
        queries,
        warmup_iterations,
        repetitions,
        mode,
        SearchResponseDetail::Compact,
    )
}

/// Measures a search profile and reports the evidence shape returned by it.
///
/// This is the benchmark boundary for comparing the default full response
/// with the explicit compact profile without counting transport scheduling.
///
/// # Errors
///
/// Returns [`BenchmarkError`] for invalid inputs or response serialization.
pub fn benchmark_search_profile(
    config: &KnowledgeConfig,
    queries: &[String],
    warmup_iterations: usize,
    repetitions: usize,
    mode: SearchMode,
    detail: SearchResponseDetail,
) -> Result<McpBenchmarkReport, BenchmarkError> {
    if queries.is_empty() {
        return Err(BenchmarkError::EmptyQueries);
    }
    if repetitions == 0 {
        return Err(BenchmarkError::InvalidRepetitions);
    }

    let params = |query: &str| SearchVescKnowledgeParams {
        query: query.to_owned(),
        snapshot_id: None,
        limit: 10,
        mode: Some(mode),
        filters: SearchVescKnowledgeFilters::default(),
        max_response_bytes: None,
        max_context_bytes: None,
        detail,
    };
    for _ in 0..warmup_iterations {
        for query in queries {
            let response = search_vesc_knowledge_json_with_config(&params(query), config);
            validate_search_response(&response, mode, detail)?;
        }
    }

    let rss_before_queries_bytes = process_rss_bytes();
    let mut timings = Vec::with_capacity(queries.len() * repetitions);
    let mut response_sizes = Vec::with_capacity(queries.len() * repetitions);
    let mut snapshot_ids = BTreeSet::new();
    let mut result_counts = BTreeSet::new();
    let mut response_digests = BTreeSet::new();
    let mut evidence = BenchmarkEvidence::default();
    for _ in 0..repetitions {
        for query in queries {
            let started = Instant::now();
            let response = search_vesc_knowledge_json_with_config(&params(query), config);
            let bytes = response.len();
            timings.push(elapsed_us(started));
            let validated = validate_search_response(&response, mode, detail)?;
            snapshot_ids.extend(validated.snapshot_id);
            result_counts.insert(validated.results.len());
            evidence.observe(&validated.results);
            response_digests
                .insert(vesc_knowledge_index::ContentDigest::of(response.as_bytes()).to_string());
            response_sizes.push(bytes as u64);
        }
    }
    let rss_after_queries_bytes = process_rss_bytes();
    let rss_retained_delta_bytes = rss_before_queries_bytes
        .zip(rss_after_queries_bytes)
        .and_then(|(before, after)| {
            i64::try_from(after)
                .ok()?
                .checked_sub(i64::try_from(before).ok()?)
        });

    Ok(McpBenchmarkReport {
        schema: 3,
        mode,
        detail,
        warmup_iterations,
        repetitions,
        query_count: queries.len(),
        snapshot_ids: snapshot_ids.into_iter().collect(),
        result_counts: result_counts.into_iter().collect(),
        response_digests: response_digests.into_iter().collect(),
        handler_and_serialization: TimingDistribution::from_samples(timings),
        response_bytes: ByteDistribution::from_samples(response_sizes),
        evidence,
        rss_before_queries_bytes,
        rss_after_queries_bytes,
        rss_retained_delta_bytes,
        machine: machine_profile(),
        warnings: vec!["measures the in-process MCP handler, not stdio transport".into()],
    })
}

fn validate_search_response(
    response: &str,
    expected_mode: SearchMode,
    expected_detail: SearchResponseDetail,
) -> Result<BenchmarkSearchResponse, BenchmarkError> {
    let response: BenchmarkSearchResponse = serde_json::from_str(response)?;
    if !response.ok {
        return Err(BenchmarkError::SearchFailed(
            response.error.unwrap_or_else(|| "unknown error".into()),
        ));
    }
    if response.mode != expected_mode {
        return Err(BenchmarkError::UnexpectedMode {
            expected: expected_mode,
            actual: response.mode,
        });
    }
    if response.detail != expected_detail {
        return Err(BenchmarkError::UnexpectedDetail {
            expected: expected_detail,
            actual: response.detail,
        });
    }
    if response.results.is_empty() {
        return Err(BenchmarkError::NoResults);
    }
    if !response.warnings.is_empty() {
        return Err(BenchmarkError::SearchWarnings(response.warnings.join("; ")));
    }
    Ok(response)
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
        assert_eq!(report.schema, 3);
        assert_eq!(report.query_count, 1);
        assert_eq!(report.response_digests.len(), 1);
        assert_eq!(report.handler_and_serialization.samples, 2);
        assert_eq!(report.response_bytes.samples, 2);
        assert!(report.response_bytes.max_bytes > 0);
    }

    #[test]
    fn hybrid_benchmark_rejects_lexical_only_configuration() {
        let error = benchmark_search_mode(
            &KnowledgeConfig::default(),
            &["lbm_add_extension".into()],
            0,
            1,
            SearchMode::Hybrid,
        )
        .expect_err("hybrid search must fail against lexical-only configuration");

        assert!(matches!(error, BenchmarkError::SearchFailed(_)));
    }

    #[test]
    fn full_profile_reports_evidence_shape() {
        let report = benchmark_search_profile(
            &KnowledgeConfig::default(),
            &["lbm_add_extension".into()],
            0,
            1,
            SearchMode::Lexical,
            SearchResponseDetail::Full,
        )
        .expect("benchmark");

        assert_eq!(report.detail, SearchResponseDetail::Full);
        assert!(report.evidence.provenance_rows > 0);
        assert!(report.evidence.distinct_source_paths > 0);
    }
}
