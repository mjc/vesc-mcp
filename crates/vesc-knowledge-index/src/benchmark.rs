//! Reproducible local retrieval benchmark measurements.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::evaluation::EvaluationMode;
use crate::{Chunk, FusionConfig, LexicalFilters, LexicalIndex, embedded_entries, fuse_candidates};

/// A percentile summary over monotonic elapsed-time samples in microseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimingDistribution {
    pub samples: usize,
    pub min_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

/// A percentile summary over serialized response sizes in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteDistribution {
    pub samples: usize,
    pub min_bytes: u64,
    pub p50_bytes: u64,
    pub p95_bytes: u64,
    pub max_bytes: u64,
}

impl TimingDistribution {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        let index = |percentile: usize| {
            ((percentile * samples.len()).saturating_add(99) / 100)
                .saturating_sub(1)
                .min(samples.len().saturating_sub(1))
        };
        Self {
            samples: samples.len(),
            min_us: samples[0],
            p50_us: samples[index(50)],
            p95_us: samples[index(95)],
            max_us: samples[samples.len() - 1],
        }
    }
}

impl ByteDistribution {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        let index = |percentile: usize| {
            ((percentile * samples.len()).saturating_add(99) / 100)
                .saturating_sub(1)
                .min(samples.len().saturating_sub(1))
        };
        Self {
            samples: samples.len(),
            min_bytes: samples[0],
            p50_bytes: samples[index(50)],
            p95_bytes: samples[index(95)],
            max_bytes: samples[samples.len() - 1],
        }
    }
}

/// Machine information that affects benchmark interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    pub os: String,
    pub arch: String,
    pub rust_target: String,
}

/// Stable benchmark output for build, load, search, fusion, and response size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkReport {
    pub schema: u16,
    pub mode: EvaluationMode,
    pub warmup_iterations: usize,
    pub repetitions: usize,
    pub query_count: usize,
    pub corpus_documents: usize,
    pub corpus_chunks: usize,
    pub artifact_bytes: Option<u64>,
    pub build: TimingDistribution,
    pub load: TimingDistribution,
    pub query: TimingDistribution,
    pub fusion: TimingDistribution,
    pub response_bytes: ByteDistribution,
    pub rss_before_bytes: Option<u64>,
    pub rss_after_bytes: Option<u64>,
    pub rss_delta_bytes: Option<i64>,
    pub machine: MachineProfile,
    pub warnings: Vec<String>,
}

/// Errors raised while measuring a local lexical artifact.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BenchmarkError {
    #[error("benchmark requires at least one query")]
    EmptyQueries,
    #[error("benchmark repetitions must be positive")]
    InvalidRepetitions,
    #[error("benchmark I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("benchmark lexical artifact failed: {0}")]
    Lexical(#[from] crate::LexicalError),
}

/// Measures the local lexical pipeline without network or wall-clock metadata.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when inputs are empty, the artifact cannot be
/// loaded, or the lexical index cannot be built.
#[allow(clippy::too_many_lines)]
pub fn benchmark_lexical(
    artifact: Option<&Path>,
    queries: &[String],
    warmup_iterations: usize,
    repetitions: usize,
) -> Result<BenchmarkReport, BenchmarkError> {
    if queries.is_empty() {
        return Err(BenchmarkError::EmptyQueries);
    }
    if repetitions == 0 {
        return Err(BenchmarkError::InvalidRepetitions);
    }

    let chunks = match artifact {
        Some(root) => {
            let path = lexical_path(root)?;
            LexicalIndex::open_artifact(&path)?
                .chunks()
                .values()
                .cloned()
                .collect()
        }
        None => embedded_chunks(),
    };
    let mut build_samples = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let start = Instant::now();
        let _ = LexicalIndex::build(&chunks)?;
        build_samples.push(elapsed_us(start));
    }

    let mut warnings = Vec::new();
    let (index, load_samples, artifact_bytes) = if let Some(root) = artifact {
        let path = lexical_path(root)?;
        let bytes = fs::metadata(&path)?.len();
        let mut load_samples = Vec::with_capacity(repetitions);
        for _ in 0..warmup_iterations {
            let _ = LexicalIndex::open_artifact(&path)?;
        }
        let mut loaded = LexicalIndex::open_artifact(&path)?;
        for _ in 0..repetitions {
            let start = Instant::now();
            let candidate = LexicalIndex::open_artifact(&path)?;
            load_samples.push(elapsed_us(start));
            loaded = candidate;
        }
        (loaded, load_samples, Some(bytes))
    } else {
        let start = Instant::now();
        let index = LexicalIndex::build(&chunks)?;
        let load_us = elapsed_us(start);
        warnings.push("load measures in-memory build because no artifact was supplied".into());
        (index, vec![load_us; repetitions], None)
    };

    for _ in 0..warmup_iterations {
        for query in queries {
            let _ = index.search(query, &LexicalFilters::default(), 10);
        }
    }

    let rss_before_bytes = current_rss_bytes();
    let mut query_samples = Vec::with_capacity(queries.len() * repetitions);
    let mut fusion_samples = Vec::with_capacity(queries.len() * repetitions);
    let mut response_sizes = Vec::with_capacity(queries.len() * repetitions);
    for _ in 0..repetitions {
        for query in queries {
            let start = Instant::now();
            let hits = index
                .search(query, &LexicalFilters::default(), 10)
                .unwrap_or_default();
            query_samples.push(elapsed_us(start));
            let response_ids: Vec<_> = hits
                .iter()
                .map(|hit| hit.chunk.chunk_id.to_string())
                .collect();
            response_sizes.push(serde_json::to_vec(&response_ids).unwrap_or_default().len() as u64);

            let start = Instant::now();
            let _ = fuse_candidates(
                &hits,
                &[],
                index.chunks(),
                FusionConfig {
                    limit: 10,
                    ..FusionConfig::default()
                },
            );
            fusion_samples.push(elapsed_us(start));
        }
    }
    let rss_after_bytes = current_rss_bytes();
    let corpus_documents = index
        .chunks()
        .values()
        .map(|chunk| chunk.document_id.clone())
        .collect::<BTreeSet<_>>()
        .len();
    Ok(BenchmarkReport {
        schema: 1,
        mode: EvaluationMode::Lexical,
        warmup_iterations,
        repetitions,
        query_count: queries.len(),
        corpus_documents,
        corpus_chunks: index.chunks().len(),
        artifact_bytes,
        build: TimingDistribution::from_samples(build_samples),
        load: TimingDistribution::from_samples(load_samples),
        query: TimingDistribution::from_samples(query_samples),
        fusion: TimingDistribution::from_samples(fusion_samples),
        response_bytes: ByteDistribution::from_samples(response_sizes),
        rss_before_bytes,
        rss_after_bytes,
        rss_delta_bytes: rss_before_bytes
            .zip(rss_after_bytes)
            .and_then(|(before, after)| {
                i64::try_from(after)
                    .ok()?
                    .checked_sub(i64::try_from(before).ok()?)
            }),
        machine: MachineProfile {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            rust_target: host_target().into(),
        },
        warnings,
    })
}

fn lexical_path(root: &Path) -> Result<PathBuf, BenchmarkError> {
    if root.is_file() {
        return Ok(root.to_owned());
    }
    let manifest = crate::inspect_manifest(&crate::active_manifest_path(root))
        .map_err(|error| BenchmarkError::Lexical(crate::LexicalError::Io(error.to_string())))?;
    Ok(root
        .join("generations")
        .join(manifest.corpus.content_digest.to_string())
        .join("lexical.json"))
}

fn embedded_chunks() -> Vec<Chunk> {
    embedded_entries()
        .iter()
        .filter_map(|entry| {
            crate::NormalizedDocument::from_legacy(entry)
                .ok()
                .and_then(|document| document.legacy_chunk().ok())
        })
        .collect()
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn current_rss_bytes() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kilobytes = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1024)
}

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_summary_uses_nearest_rank() {
        let summary = TimingDistribution::from_samples(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(summary.samples, 10);
        assert_eq!(summary.min_us, 1);
        assert_eq!(summary.p50_us, 5);
        assert_eq!(summary.p95_us, 10);
        assert_eq!(summary.max_us, 10);
    }

    #[test]
    fn benchmark_reports_stable_shape_for_embedded_index() {
        let report =
            benchmark_lexical(None, &["lbm_add_extension".into()], 1, 2).expect("benchmark");
        assert_eq!(report.schema, 1);
        assert_eq!(report.query_count, 1);
        assert_eq!(report.repetitions, 2);
        assert!(report.corpus_chunks > 0);
        assert_eq!(report.query.samples, 2);
        assert_eq!(report.fusion.samples, 2);
    }
}
