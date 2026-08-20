//! Reproducible local retrieval benchmark measurements.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::corpus::git::GitCorpusSource;
use crate::evaluation::{EvaluationMode, EvaluationReport};
use crate::lexical::EmbeddingTextHydrator;
use crate::{
    Chunk, ChunkId, ContentDigest, EmbeddingProvider, FusionConfig, GraphArtifact, LexicalError,
    LexicalFilters, LexicalIndex, TokenStatistics, VectorArtifact, VectorBuildObservations,
    VectorSearch, embedded_entries, fuse_candidates,
};

/// Runs the persisted-index embedding inventory path for allocation benchmarks.
///
/// # Errors
///
/// Returns [`LexicalError`] when the index's ID columns are invalid.
pub fn embedding_chunk_ids(index: &LexicalIndex) -> Result<Vec<ChunkId>, LexicalError> {
    index.embedding_chunk_ids()
}

/// Prepared persisted inputs for an embedding-text projection benchmark.
pub struct EmbeddingProjectionFixture {
    index: LexicalIndex,
    ids: Vec<ChunkId>,
    inputs: Option<Vec<crate::lexical::EmbeddingLocatorRecord>>,
}

/// Loads the persisted inputs that should remain outside the measured region
/// of an embedding-text projection benchmark.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the artifact, sidecar, or ID inventory
/// cannot be read.
pub fn prepare_embedding_projection(
    path: &Path,
    sources: &[GitCorpusSource],
) -> Result<EmbeddingProjectionFixture, BenchmarkError> {
    let index = LexicalIndex::open_git_search_artifact_with_sources(path, sources)?;
    let ids = index.embedding_chunk_ids()?;
    let inputs = LexicalIndex::read_embedding_inputs(path)?;
    Ok(EmbeddingProjectionFixture { index, ids, inputs })
}

/// Runs only the persisted embedding-text projection, excluding vector
/// construction, checkpoint I/O, and artifact opening from allocation and
/// peak-memory captures.
///
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the sidecar or Git hydration path cannot be
/// read.
pub fn benchmark_embedding_projection_from_fixture(
    fixture: &EmbeddingProjectionFixture,
    projected: bool,
) -> Result<usize, BenchmarkError> {
    let mut hydrator = EmbeddingTextHydrator::default();
    let texts = if projected {
        let inputs = fixture.inputs.as_deref().ok_or_else(|| {
            LexicalError::Artifact("embedding projection sidecar is missing".into())
        })?;
        fixture
            .index
            .embedding_texts_by_id_from_inputs(&fixture.ids, inputs, &mut hydrator)?
    } else {
        fixture
            .index
            .embedding_texts_by_id(&fixture.ids, &mut hydrator)?
    };
    Ok(texts.iter().map(String::len).sum())
}

/// Runs a one-shot embedding projection benchmark, including artifact opening.
/// Prefer [`prepare_embedding_projection`] and
/// [`benchmark_embedding_projection_from_fixture`] for focused profiles.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the artifact, sidecar, ID inventory, or Git
/// hydration path cannot be read.
pub fn benchmark_embedding_projection(
    path: &Path,
    sources: &[GitCorpusSource],
    projected: bool,
) -> Result<usize, BenchmarkError> {
    let fixture = prepare_embedding_projection(path, sources)?;
    benchmark_embedding_projection_from_fixture(&fixture, projected)
}

/// Inputs held outside the measured region of a graph staging benchmark.
pub struct GraphProjectionFixture {
    path: PathBuf,
    inputs: GraphProjectionInputs,
    corpus_digest: ContentDigest,
    lexical_artifact_bytes: u64,
    live_documents: usize,
}

enum GraphProjectionInputs {
    Projected,
    Legacy {
        index: Box<LexicalIndex>,
        ids: Vec<ChunkId>,
    },
}

/// Compact counters returned by the graph staging benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphProjectionStats {
    pub live_documents: usize,
    pub matching_history_documents: usize,
    pub stored_documents_decoded: usize,
    pub git_bodies_hydrated: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub lexical_artifact_bytes: u64,
}

/// Loads a persisted Git artifact and its compact graph inputs before a
/// focused graph projection measurement.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the artifact, source descriptors, or
/// inventory cannot be read.
pub fn prepare_graph_projection(
    path: &Path,
    sources: &[GitCorpusSource],
) -> Result<GraphProjectionFixture, BenchmarkError> {
    let index = LexicalIndex::open_git_search_artifact_with_sources(path, sources)?;
    let ids = index.embedding_chunk_ids()?;
    let (_, _, corpus_digest) = LexicalIndex::corpus_inventory(path)?;
    let lexical_artifact_bytes = graph_artifact_bytes(path)?;
    Ok(GraphProjectionFixture {
        path: path.to_owned(),
        live_documents: ids.len(),
        inputs: GraphProjectionInputs::Legacy {
            index: Box::new(index),
            ids,
        },
        corpus_digest,
        lexical_artifact_bytes,
    })
}

/// Loads only the compact graph inputs for a projected measurement.
///
/// The projected operation does not need a Tantivy reader or an embedding-ID
/// inventory; retaining either in the fixture would make Massif measure setup
/// state instead of the graph projection itself.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the persisted inventory or artifact files
/// cannot be read.
pub fn prepare_graph_projection_projected(
    path: &Path,
    _sources: &[GitCorpusSource],
) -> Result<GraphProjectionFixture, BenchmarkError> {
    let (live_documents, _, corpus_digest) = LexicalIndex::corpus_inventory(path)?;
    let lexical_artifact_bytes = graph_artifact_bytes(path)?;
    Ok(GraphProjectionFixture {
        path: path.to_owned(),
        inputs: GraphProjectionInputs::Projected,
        corpus_digest,
        lexical_artifact_bytes,
        live_documents,
    })
}

/// Runs graph staging from compact sidecar metadata or the old full-chunk
/// readback path. The latter is retained only as a matched benchmark baseline.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when graph inputs or persisted chunks are invalid.
pub fn benchmark_graph_projection_from_fixture(
    fixture: &GraphProjectionFixture,
    projected: bool,
) -> Result<GraphProjectionStats, BenchmarkError> {
    let (graph, stored_documents_decoded, git_bodies_hydrated) = if projected {
        let graph = LexicalIndex::graph_from_sidecar(
            &fixture.path,
            fixture.corpus_digest.clone(),
            Some,
        )?
        .ok_or_else(|| LexicalError::Artifact("graph projection sidecar is missing".into()))?;
        (graph, 0, 0)
    } else {
        let GraphProjectionInputs::Legacy { index, ids } = &fixture.inputs else {
            return Err(LexicalError::Artifact(
                "projected graph fixture has no legacy input".into(),
            )
            .into());
        };
        let chunks = index.chunks_by_id(&ids.iter().cloned().collect())?;
        let decoded = chunks.len();
        let graph = GraphArtifact::from_chunks(
            fixture.corpus_digest.clone(),
            &chunks.into_values().collect::<Vec<_>>(),
        )
        .map_err(|error| LexicalError::Artifact(error.to_string()))?;
        (graph, decoded, decoded)
    };
    Ok(GraphProjectionStats {
        live_documents: fixture.live_documents,
        matching_history_documents: graph.nodes.len(),
        stored_documents_decoded,
        git_bodies_hydrated,
        graph_nodes: graph.nodes.len(),
        graph_edges: graph.edges.len(),
        lexical_artifact_bytes: fixture.lexical_artifact_bytes,
    })
}

/// Inputs held outside the measured region of a history inventory benchmark.
pub struct HistoryInventoryFixture {
    path: PathBuf,
    inputs: HistoryInventoryInputs,
    lexical_artifact_bytes: u64,
    live_documents: usize,
}

enum HistoryInventoryInputs {
    Projected,
    Legacy {
        index: Box<LexicalIndex>,
        ids: Vec<ChunkId>,
    },
}

/// Compact counters returned by the history inventory benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryInventoryStats {
    pub live_documents: usize,
    pub matching_history_documents: usize,
    pub stored_documents_decoded: usize,
    pub git_bodies_hydrated: usize,
    pub history_records: usize,
    pub lexical_artifact_bytes: u64,
}

/// Loads persisted inputs before a focused history inventory measurement.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the artifact, source descriptors, or
/// inventory cannot be read.
pub fn prepare_history_inventory(
    path: &Path,
    sources: &[GitCorpusSource],
) -> Result<HistoryInventoryFixture, BenchmarkError> {
    let index = LexicalIndex::open_git_search_artifact_with_sources(path, sources)?;
    let ids = index.embedding_chunk_ids()?;
    let lexical_artifact_bytes = history_artifact_bytes(path)?;
    Ok(HistoryInventoryFixture {
        path: path.to_owned(),
        live_documents: ids.len(),
        inputs: HistoryInventoryInputs::Legacy {
            index: Box::new(index),
            ids,
        },
        lexical_artifact_bytes,
    })
}

/// Loads only the history sidecar inputs for a projected measurement.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the persisted inventory or artifact files
/// cannot be read.
pub fn prepare_history_inventory_projected(
    path: &Path,
    _sources: &[GitCorpusSource],
) -> Result<HistoryInventoryFixture, BenchmarkError> {
    let (live_documents, _, _) = LexicalIndex::corpus_inventory(path)?;
    let lexical_artifact_bytes = history_artifact_bytes(path)?;
    Ok(HistoryInventoryFixture {
        path: path.to_owned(),
        inputs: HistoryInventoryInputs::Projected,
        lexical_artifact_bytes,
        live_documents,
    })
}

/// Runs history inventory from compact records or the old full-document readback.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when the history sidecar or persisted chunks are invalid.
pub fn benchmark_history_inventory_from_fixture(
    fixture: &HistoryInventoryFixture,
    projected: bool,
) -> Result<HistoryInventoryStats, BenchmarkError> {
    let (history_records, stored_documents_decoded, git_bodies_hydrated) = if projected {
        let records = LexicalIndex::read_history_records(&fixture.path)?
            .ok_or_else(|| LexicalError::Artifact("history inventory sidecar is missing".into()))?;
        (records.len(), 0, 0)
    } else {
        let HistoryInventoryInputs::Legacy { index, ids } = &fixture.inputs else {
            return Err(LexicalError::Artifact(
                "projected history fixture has no legacy input".into(),
            )
            .into());
        };
        let chunks = index.chunks_by_id(&ids.iter().cloned().collect())?;
        let decoded = chunks.len();
        (decoded, decoded, decoded)
    };
    Ok(HistoryInventoryStats {
        live_documents: fixture.live_documents,
        matching_history_documents: history_records,
        stored_documents_decoded,
        git_bodies_hydrated,
        history_records,
        lexical_artifact_bytes: fixture.lexical_artifact_bytes,
    })
}

fn path_bytes(path: &Path) -> Result<u64, std::io::Error> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    fs::read_dir(path)?.try_fold(0_u64, |total, entry| {
        Ok(total.saturating_add(path_bytes(&entry?.path())?))
    })
}

fn graph_artifact_bytes(path: &Path) -> Result<u64, BenchmarkError> {
    [
        path.to_path_buf(),
        path.with_extension("tantivy"),
        path.with_extension("graph-input.json"),
        path.with_extension("history-input.json"),
        path.with_extension("embedding-input.json"),
    ]
    .iter()
    .try_fold(0_u64, |total, path| {
        Ok::<_, std::io::Error>(total.saturating_add(path_bytes(path)?))
    })
    .map_err(Into::into)
}

fn history_artifact_bytes(path: &Path) -> Result<u64, BenchmarkError> {
    [path.to_path_buf(), path.with_extension("tantivy")]
        .iter()
        .try_fold(0_u64, |total, path| {
            Ok::<_, std::io::Error>(total.saturating_add(path_bytes(path)?))
        })
        .map_err(Into::into)
}

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
    #[must_use]
    pub const fn single(micros: u64) -> Self {
        Self {
            samples: 1,
            min_us: micros,
            p50_us: micros,
            p95_us: micros,
            max_us: micros,
        }
    }

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
    /// Process RSS immediately before query measurements; this is retained RSS,
    /// not peak RSS.
    pub rss_before_queries_bytes: Option<u64>,
    /// Process RSS immediately after query measurements; this is retained RSS,
    /// not peak RSS.
    pub rss_after_queries_bytes: Option<u64>,
    /// Difference between the retained RSS samples; peak RSS is measured by an
    /// external host harness.
    pub rss_retained_delta_bytes: Option<i64>,
    pub machine: MachineProfile,
    pub warnings: Vec<String>,
}

/// Release-mode semantic build/query measurements with inference and exact
/// search kept separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkReport {
    pub schema: u16,
    pub mode: EvaluationMode,
    pub model_id: String,
    pub model_revision: String,
    pub corpus_digest: ContentDigest,
    pub build_identity: String,
    pub outer_batch_size: usize,
    #[serde(default)]
    pub intra_threads: Option<usize>,
    #[serde(default)]
    pub length_bucketed: bool,
    /// Effective model input window used for this run.
    #[serde(default)]
    pub effective_max_length: Option<usize>,
    #[serde(default)]
    pub cold_initialization: Option<TimingDistribution>,
    pub warmup_iterations: usize,
    pub repetitions: usize,
    pub query_count: usize,
    pub corpus_chunks: usize,
    pub vector_count: usize,
    pub vector_dimension: usize,
    pub artifact_bytes: u64,
    /// The first query after build/provider setup, not a cold-start query.
    pub first_query_after_build: TimingDistribution,
    pub build: TimingDistribution,
    pub embedding_input: TimingDistribution,
    pub provider_inference: TimingDistribution,
    pub vector_finalization: TimingDistribution,
    pub embedding_input_bytes: u64,
    #[serde(default)]
    pub token_statistics: Option<TokenStatistics>,
    pub embedding: TimingDistribution,
    pub exact_search: BTreeMap<usize, TimingDistribution>,
    /// Process RSS immediately before query measurements; this is retained RSS,
    /// not peak RSS.
    pub rss_before_queries_bytes: Option<u64>,
    /// Process RSS immediately after query measurements; this is retained RSS,
    /// not peak RSS.
    pub rss_after_queries_bytes: Option<u64>,
    /// Difference between the retained RSS samples; peak RSS is measured by an
    /// external host harness.
    pub rss_retained_delta_bytes: Option<i64>,
    /// Peak RSS supplied by an external process harness.
    #[serde(default)]
    pub peak_rss_bytes: Option<u64>,
    /// SHA-256 of the encoded vector artifact when retained by a bake-off.
    #[serde(default)]
    pub vector_artifact_sha256: Option<String>,
    pub machine: MachineProfile,
    pub warnings: Vec<String>,
}

/// Query-only measurements against an existing immutable vector artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticQueryBenchmarkReport {
    pub schema: u16,
    #[serde(default)]
    pub cold_initialization: Option<TimingDistribution>,
    pub warmup_iterations: usize,
    pub repetitions: usize,
    pub query_count: usize,
    pub vector_count: usize,
    pub vector_dimension: usize,
    pub first_query: TimingDistribution,
    pub embedding: TimingDistribution,
    pub exact_search: BTreeMap<usize, TimingDistribution>,
    /// Process RSS before the caller opens the vector artifact.
    #[serde(default)]
    pub rss_before_vector_open_bytes: Option<u64>,
    /// Process RSS immediately after the caller opens and validates the vector
    /// artifact.
    #[serde(default)]
    pub rss_after_vector_open_bytes: Option<u64>,
    /// Process RSS after artifact opening and provider construction, but before
    /// the first measured query.
    #[serde(default)]
    pub rss_before_first_query_bytes: Option<u64>,
    /// Process RSS immediately after the first inference and search.
    #[serde(default)]
    pub rss_after_first_query_bytes: Option<u64>,
    pub rss_before_queries_bytes: Option<u64>,
    pub rss_after_queries_bytes: Option<u64>,
    pub rss_retained_delta_bytes: Option<i64>,
}

/// A stable collection of semantic benchmark runs over different outer
/// embedding batch sizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticBenchmarkMatrixReport {
    pub schema: u16,
    pub runs: Vec<SemanticBenchmarkReport>,
}

/// One pinned model identity used by the reproducible embedding bake-off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeoffCandidateSpec {
    pub name: String,
    pub model_id: String,
    pub model_revision: String,
    /// Relative directory below the operator-provided model root.
    pub directory: String,
    pub license: String,
    pub production_eligible: bool,
    pub quantization: String,
    pub onnx_sha256: String,
    pub onnx_bytes: u64,
}

/// Quality and runtime evidence for one bake-off candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeoffCandidateReport {
    pub candidate: BakeoffCandidateSpec,
    pub benchmark: SemanticBenchmarkReport,
    pub semantic: EvaluationReport,
    pub hybrid: EvaluationReport,
}

/// Machine-readable comparison of the common lexical control and pinned
/// embedding candidates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeoffReport {
    pub schema: u16,
    pub suite_id: String,
    pub corpus_digest: ContentDigest,
    pub corpus_documents: usize,
    pub corpus_chunks: usize,
    /// Chunks actually embedded by this run; smaller only for an explicit benchmark sample.
    pub evaluated_chunks: usize,
    pub lexical: EvaluationReport,
    pub candidates: Vec<BakeoffCandidateReport>,
    pub machine: MachineProfile,
    pub warnings: Vec<String>,
}

impl BakeoffReport {
    /// Render the comparison table from the JSON report fields.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn to_markdown(&self) -> String {
        let mut markdown = String::new();
        writeln!(markdown, "# Embedding model bake-off").expect("write to String");
        writeln!(markdown).expect("write to String");
        writeln!(markdown, "- Suite: {}", self.suite_id).expect("write to String");
        writeln!(markdown, "- Corpus digest: {}", self.corpus_digest).expect("write to String");
        writeln!(
            markdown,
            "- Corpus: {} documents / {} chunks",
            self.corpus_documents, self.corpus_chunks
        )
        .expect("write to String");
        writeln!(markdown, "- Evaluated chunks: {}", self.evaluated_chunks)
            .expect("write to String");
        writeln!(markdown).expect("write to String");
        writeln!(
            markdown,
            "| Candidate | Quantization | Provider p50 (s) | Chunks/s | Fused R@5 | Fused R@10 | Fused MRR@10 | Semantic R@5 | Peak RSS (MiB) |"
        )
        .expect("write to String");
        writeln!(
            markdown,
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
        )
        .expect("write to String");
        writeln!(
            markdown,
            "| lexical control | — | — | — | {:.4} | {:.4} | {:.4} | — | — |",
            self.lexical.recall_at_5, self.lexical.recall_at_10, self.lexical.mrr_at_10
        )
        .expect("write to String");
        for candidate in &self.candidates {
            let benchmark = &candidate.benchmark;
            let provider_seconds = benchmark.provider_inference.p50_us as f64 / 1_000_000.0;
            let chunks_per_second = if provider_seconds == 0.0 {
                0.0
            } else {
                self.evaluated_chunks as f64 / provider_seconds
            };
            let peak_rss = benchmark.peak_rss_bytes.map_or_else(
                || "—".into(),
                |bytes| format!("{:.1}", bytes as f64 / 1_048_576.0),
            );
            writeln!(
                markdown,
                "| {} | {} | {:.3} | {:.2} | {:.4} | {:.4} | {:.4} | {:.4} | {} |",
                candidate.candidate.name,
                candidate.candidate.quantization,
                provider_seconds,
                chunks_per_second,
                candidate.hybrid.recall_at_5,
                candidate.hybrid.recall_at_10,
                candidate.hybrid.mrr_at_10,
                candidate.semantic.recall_at_5,
                peak_rss,
            )
            .expect("write to String");
        }
        markdown
    }
}

impl SemanticBenchmarkMatrixReport {
    /// Render one compact comparison table from the JSON-compatible runs.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut markdown = String::new();
        writeln!(markdown, "# Semantic batch sweep").expect("write to String");
        writeln!(markdown).expect("write to String");
        writeln!(
            markdown,
            "| Batch | Intra threads | Order | Chunks | Provider p50 (µs) | Chunks/sec | Padding (ppm) | Exact K=5 p50 (µs) |"
        )
        .expect("write to String");
        writeln!(
            markdown,
            "| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: |"
        )
        .expect("write to String");
        for report in &self.runs {
            let k5 = report
                .exact_search
                .get(&5)
                .map_or(0, |timing| timing.p50_us);
            let chunks_per_second = (report.corpus_chunks as u64 * 1_000_000)
                .checked_div(report.provider_inference.p50_us)
                .unwrap_or_default();
            let padding = report
                .token_statistics
                .as_ref()
                .map_or(0, |statistics| statistics.padding_ratio_ppm);
            writeln!(
                markdown,
                "| {} | {:?} | {} | {} | {} | {} | {} | {} |",
                report.outer_batch_size,
                report.intra_threads,
                if report.length_bucketed {
                    "token-length"
                } else {
                    "source"
                },
                report.corpus_chunks,
                report.provider_inference.p50_us,
                chunks_per_second,
                padding,
                k5,
            )
            .expect("write to String");
        }
        markdown
    }
}

impl SemanticBenchmarkReport {
    /// Render the stable benchmark fields as a reviewable Markdown report.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut markdown = String::new();
        writeln!(markdown, "# Semantic benchmark").expect("write to String");
        writeln!(markdown).expect("write to String");
        writeln!(markdown, "- Mode: `{:?}`", self.mode).expect("write to String");
        writeln!(markdown, "- Model: `{}`", self.model_id).expect("write to String");
        writeln!(markdown, "- Model revision: `{}`", self.model_revision).expect("write to String");
        writeln!(markdown, "- Corpus digest: `{}`", self.corpus_digest).expect("write to String");
        writeln!(markdown, "- Build identity: `{}`", self.build_identity).expect("write to String");
        writeln!(markdown, "- Machine: `{}`", self.machine.rust_target).expect("write to String");
        writeln!(markdown, "- Outer batch size: `{}`", self.outer_batch_size)
            .expect("write to String");
        writeln!(markdown, "- Intra-op threads: `{:?}`", self.intra_threads)
            .expect("write to String");
        writeln!(
            markdown,
            "- Embedding input bytes: `{}`",
            self.embedding_input_bytes
        )
        .expect("write to String");
        if let Some(statistics) = &self.token_statistics {
            writeln!(
                markdown,
                "- Tokens: real={} padded={} untruncated={} truncated-chunks={} padding-ppm={}",
                statistics.total_real_tokens,
                statistics.total_padded_tokens,
                statistics.total_untruncated_tokens,
                statistics.truncated_chunks,
                statistics.padding_ratio_ppm,
            )
            .expect("write to String");
        }
        writeln!(
            markdown,
            "- Effective max length: `{:?}`",
            self.effective_max_length
        )
        .expect("write to String");
        writeln!(markdown).expect("write to String");
        writeln!(
            markdown,
            "| Measurement | Samples | p50 (µs) | p95 (µs) | max (µs) |"
        )
        .expect("write to String");
        writeln!(markdown, "| --- | ---: | ---: | ---: | ---: |").expect("write to String");
        if let Some(initialization) = &self.cold_initialization {
            write_timing_row(&mut markdown, "Cold initialization", initialization);
        }
        write_timing_row(
            &mut markdown,
            "First query after build",
            &self.first_query_after_build,
        );
        write_timing_row(&mut markdown, "Build", &self.build);
        write_timing_row(&mut markdown, "Embedding input", &self.embedding_input);
        write_timing_row(
            &mut markdown,
            "Provider inference",
            &self.provider_inference,
        );
        write_timing_row(
            &mut markdown,
            "Vector finalization",
            &self.vector_finalization,
        );
        write_timing_row(&mut markdown, "Query embedding", &self.embedding);
        for (limit, timing) in &self.exact_search {
            write_timing_row(&mut markdown, &format!("Exact search K={limit}"), timing);
        }
        markdown
    }
}

fn write_timing_row(markdown: &mut String, label: &str, timing: &TimingDistribution) {
    writeln!(
        markdown,
        "| {label} | {} | {} | {} | {} |",
        timing.samples, timing.p50_us, timing.p95_us, timing.max_us
    )
    .expect("write to String");
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
    #[error("benchmark requires at least one search limit")]
    EmptyLimits,
    #[error("benchmark semantic artifact failed: {0}")]
    Semantic(#[from] crate::EmbeddingError),
}

/// Measures semantic generation, query embedding, and exact search limits.
/// The provider and all inputs are supplied by the caller, so this remains
/// offline and can be run with a pinned local model.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when inputs are empty, repetitions are invalid,
/// or the provider/artifact contract rejects a measurement.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn benchmark_semantic<P: EmbeddingProvider + ?Sized>(
    provider: &mut P,
    chunks: &[Chunk],
    queries: &[String],
    model_id: &str,
    model_revision: &str,
    corpus_digest: &ContentDigest,
    search_limits: &[usize],
    warmup_iterations: usize,
    repetitions: usize,
) -> Result<SemanticBenchmarkReport, BenchmarkError> {
    benchmark_semantic_with_artifact(
        provider,
        chunks,
        queries,
        model_id,
        model_revision,
        corpus_digest,
        search_limits,
        warmup_iterations,
        repetitions,
    )
    .map(|(report, _)| report)
}

/// Measures semantic work and returns the final artifact for quality scoring.
///
/// Returning the artifact prevents a bake-off from embedding the full corpus
/// a second time solely to evaluate the candidate.
///
/// # Errors
///
/// Returns [`BenchmarkError`] when embedding, vector construction, or search
/// measurement fails.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn benchmark_semantic_with_artifact<P: EmbeddingProvider + ?Sized>(
    provider: &mut P,
    chunks: &[Chunk],
    queries: &[String],
    model_id: &str,
    model_revision: &str,
    corpus_digest: &ContentDigest,
    search_limits: &[usize],
    warmup_iterations: usize,
    repetitions: usize,
) -> Result<(SemanticBenchmarkReport, VectorArtifact), BenchmarkError> {
    if queries.is_empty() {
        return Err(BenchmarkError::EmptyQueries);
    }
    if search_limits.is_empty() {
        return Err(BenchmarkError::EmptyLimits);
    }
    if repetitions == 0 {
        return Err(BenchmarkError::InvalidRepetitions);
    }
    let mut build_samples = Vec::with_capacity(repetitions);
    let mut embedding_input_samples = Vec::with_capacity(repetitions);
    let mut provider_samples = Vec::with_capacity(repetitions);
    let mut vector_finalization_samples = Vec::with_capacity(repetitions);
    let mut embedding_input_bytes = 0_u64;
    let chunk_refs = chunks.iter().collect::<Vec<_>>();
    let inference_order = provider.inference_order(&chunk_refs)?;
    let mut build = || {
        let started = Instant::now();
        let (artifact, observations): (VectorArtifact, VectorBuildObservations) =
            VectorArtifact::from_provider_with_observations_and_order(
                provider,
                chunks,
                model_id,
                model_revision,
                corpus_digest.clone(),
                inference_order.as_deref(),
            )?;
        build_samples.push(elapsed_us(started));
        embedding_input_samples.push(observations.embedding_input_us);
        provider_samples.push(observations.provider_us);
        vector_finalization_samples.push(observations.vector_finalization_us);
        embedding_input_bytes = observations.input_bytes;
        Ok::<_, BenchmarkError>(artifact)
    };
    let mut artifact = build()?;
    for _ in 1..repetitions {
        drop(artifact);
        artifact = build()?;
    }
    let (artifact_digest, artifact_bytes) = artifact.encoded_digest()?;
    let queries = benchmark_semantic_queries(
        provider,
        &artifact,
        queries,
        search_limits,
        warmup_iterations,
        repetitions,
    )?;
    Ok((
        SemanticBenchmarkReport {
            schema: 2,
            mode: EvaluationMode::Semantic,
            model_id: model_id.into(),
            model_revision: model_revision.into(),
            corpus_digest: corpus_digest.clone(),
            build_identity: format!(
                "vesc-knowledge-index@{};{}",
                env!("CARGO_PKG_VERSION"),
                host_target()
            ),
            outer_batch_size: provider.embedding_batch_size().get(),
            intra_threads: None,
            length_bucketed: false,
            effective_max_length: None,
            cold_initialization: None,
            warmup_iterations,
            repetitions,
            query_count: queries.query_count,
            corpus_chunks: chunks.len(),
            vector_count: artifact.ids.len(),
            vector_dimension: artifact.dimension,
            artifact_bytes,
            first_query_after_build: queries.first_query,
            build: TimingDistribution::from_samples(build_samples),
            embedding_input: TimingDistribution::from_samples(embedding_input_samples),
            provider_inference: TimingDistribution::from_samples(provider_samples),
            vector_finalization: TimingDistribution::from_samples(vector_finalization_samples),
            embedding_input_bytes,
            token_statistics: None,
            embedding: queries.embedding,
            exact_search: queries.exact_search,
            rss_before_queries_bytes: queries.rss_before_queries_bytes,
            rss_after_queries_bytes: queries.rss_after_queries_bytes,
            rss_retained_delta_bytes: queries.rss_retained_delta_bytes,
            peak_rss_bytes: None,
            vector_artifact_sha256: Some(artifact_digest.to_string()),
            machine: MachineProfile {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                rust_target: host_target().into(),
            },
            warnings: Vec::new(),
        },
        artifact,
    ))
}

/// Measure query embedding and exact search without rebuilding vectors.
///
/// # Errors
///
/// Returns [`BenchmarkError`] for empty inputs or provider/search failures.
pub fn benchmark_semantic_queries<P: EmbeddingProvider + ?Sized, A: VectorSearch + ?Sized>(
    provider: &mut P,
    artifact: &A,
    queries: &[String],
    search_limits: &[usize],
    warmup_iterations: usize,
    repetitions: usize,
) -> Result<SemanticQueryBenchmarkReport, BenchmarkError> {
    if queries.is_empty() {
        return Err(BenchmarkError::EmptyQueries);
    }
    if search_limits.is_empty() {
        return Err(BenchmarkError::EmptyLimits);
    }
    if repetitions == 0 {
        return Err(BenchmarkError::InvalidRepetitions);
    }
    let rss_before_first_query_bytes = process_rss_bytes();
    let first_query = {
        let started = Instant::now();
        let vector = provider.embed_query(&queries[0])?;
        let _ = artifact.search(&vector, search_limits[0])?;
        TimingDistribution::single(elapsed_us(started))
    };
    let rss_after_first_query_bytes = process_rss_bytes();
    for _ in 0..warmup_iterations {
        for query in queries {
            let vector = provider.embed_query(query)?;
            let _ = artifact.search(&vector, search_limits[0])?;
        }
    }

    let rss_before_queries_bytes = process_rss_bytes();
    let mut embedding_samples = Vec::with_capacity(queries.len() * repetitions);
    let mut search_samples = search_limits
        .iter()
        .map(|limit| (*limit, Vec::with_capacity(queries.len() * repetitions)))
        .collect::<BTreeMap<_, _>>();
    for _ in 0..repetitions {
        for query in queries {
            let started = Instant::now();
            let vector = provider.embed_query(query)?;
            embedding_samples.push(elapsed_us(started));
            for limit in search_limits {
                let started = Instant::now();
                let _ = artifact.search(&vector, *limit)?;
                search_samples
                    .entry(*limit)
                    .or_default()
                    .push(elapsed_us(started));
            }
        }
    }
    let rss_after_queries_bytes = process_rss_bytes();
    Ok(SemanticQueryBenchmarkReport {
        schema: 2,
        cold_initialization: None,
        warmup_iterations,
        repetitions,
        query_count: queries.len(),
        vector_count: artifact.len(),
        vector_dimension: artifact.dimension(),
        first_query,
        embedding: TimingDistribution::from_samples(embedding_samples),
        exact_search: search_samples
            .into_iter()
            .map(|(limit, samples)| (limit, TimingDistribution::from_samples(samples)))
            .collect(),
        rss_before_vector_open_bytes: None,
        rss_after_vector_open_bytes: None,
        rss_before_first_query_bytes,
        rss_after_first_query_bytes,
        rss_before_queries_bytes,
        rss_after_queries_bytes,
        rss_retained_delta_bytes: rss_before_queries_bytes
            .zip(rss_after_queries_bytes)
            .and_then(|(before, after)| {
                i64::try_from(after)
                    .ok()?
                    .checked_sub(i64::try_from(before).ok()?)
            }),
    })
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
            LexicalIndex::read_artifact_chunks(&path)?
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
            let _ = LexicalIndex::open_search_artifact(&path)?;
        }
        let mut loaded = LexicalIndex::open_search_artifact(&path)?;
        for _ in 0..repetitions {
            let start = Instant::now();
            let candidate = LexicalIndex::open_search_artifact(&path)?;
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

    let rss_before_queries_bytes = process_rss_bytes();
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
    let rss_after_queries_bytes = process_rss_bytes();
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
        rss_before_queries_bytes,
        rss_after_queries_bytes,
        rss_retained_delta_bytes: rss_before_queries_bytes
            .zip(rss_after_queries_bytes)
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
    crate::active_generation_path(root)
        .map(|generation| generation.join("lexical.json"))
        .map_err(|error| BenchmarkError::Lexical(crate::LexicalError::Io(error.to_string())))
}

fn embedded_chunks() -> Vec<Chunk> {
    embedded_entries()
        .iter()
        .filter_map(|entry| {
            crate::NormalizedDocument::from_catalog_entry(entry)
                .ok()
                .and_then(|document| document.catalog_chunk().ok())
        })
        .collect()
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Returns this process's current resident set size in bytes when the host
/// exposes it through `ps`.
#[must_use]
pub fn process_rss_bytes() -> Option<u64> {
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
    use crate::embedding_text;

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
    fn graph_projection_stats_report_compact_result_shape() {
        let stats = GraphProjectionStats {
            live_documents: 12,
            matching_history_documents: 9,
            stored_documents_decoded: 0,
            git_bodies_hydrated: 0,
            graph_nodes: 9,
            graph_edges: 8,
            lexical_artifact_bytes: 123,
        };
        assert_eq!(stats.stored_documents_decoded, 0);
        assert_eq!(stats.git_bodies_hydrated, 0);
        assert_eq!(stats.graph_nodes, stats.matching_history_documents);
    }

    #[test]
    fn history_inventory_stats_report_zero_legacy_hydration_for_projection() {
        let stats = HistoryInventoryStats {
            live_documents: 12,
            matching_history_documents: 9,
            stored_documents_decoded: 0,
            git_bodies_hydrated: 0,
            history_records: 9,
            lexical_artifact_bytes: 123,
        };
        assert_eq!(stats.history_records, stats.matching_history_documents);
        assert_eq!(stats.stored_documents_decoded, 0);
        assert_eq!(stats.git_bodies_hydrated, 0);
    }

    #[test]
    fn projected_graph_fixture_has_a_setup_without_legacy_index_state() {
        fn compile_projected_preparation(
            path: &Path,
            sources: &[GitCorpusSource],
        ) -> Result<GraphProjectionFixture, BenchmarkError> {
            prepare_graph_projection_projected(path, sources)
        }

        let _ = compile_projected_preparation;
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

    #[test]
    fn semantic_benchmark_separates_embedding_and_search() {
        let chunks = embedded_chunks();
        let mut provider = crate::FakeEmbeddingProvider::new(4);
        let report = benchmark_semantic(
            &mut provider,
            &chunks,
            &["extension".into()],
            "fake",
            "test",
            &ContentDigest::of(b"benchmark"),
            &[5, 10],
            1,
            2,
        )
        .expect("semantic benchmark");
        assert_eq!(report.build.samples, 2);
        assert_eq!(report.first_query_after_build.samples, 1);
        assert_eq!(report.embedding.samples, 2);
        assert_eq!(report.exact_search[&5].samples, 2);
        assert_eq!(report.exact_search[&10].samples, 2);
        let markdown = report.to_markdown();
        assert!(markdown.contains("Model: `fake`"));
        assert!(markdown.contains("Exact search K=5"));
    }

    #[test]
    fn semantic_benchmark_applies_provider_inference_order() {
        struct ReverseProvider {
            inner: crate::FakeEmbeddingProvider,
            first_batch: Option<String>,
            order_calls: usize,
        }

        impl crate::EmbeddingProvider for ReverseProvider {
            fn inference_order(
                &mut self,
                chunks: &[&Chunk],
            ) -> Result<Option<Vec<usize>>, crate::EmbeddingError> {
                self.order_calls += 1;
                Ok(Some((0..chunks.len()).rev().collect()))
            }

            fn embed_documents(
                &mut self,
                texts: &[String],
            ) -> Result<Vec<Vec<f32>>, crate::EmbeddingError> {
                self.first_batch = self.first_batch.clone().or_else(|| texts.first().cloned());
                self.inner.embed_documents(texts)
            }

            fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, crate::EmbeddingError> {
                self.inner.embed_query(text)
            }
        }

        let chunks = embedded_chunks();
        assert!(chunks.len() > 1);
        let expected_first = embedding_text(chunks.last().expect("multiple chunks"));
        let mut provider = ReverseProvider {
            inner: crate::FakeEmbeddingProvider::new(4),
            first_batch: None,
            order_calls: 0,
        };

        benchmark_semantic_with_artifact(
            &mut provider,
            &chunks,
            &["extension".into()],
            "fake",
            "test",
            &ContentDigest::of(b"benchmark-order"),
            &[5],
            0,
            1,
        )
        .expect("semantic benchmark");

        assert_eq!(provider.order_calls, 1);
        assert_eq!(
            provider.first_batch.as_deref(),
            Some(expected_first.as_str())
        );
    }
}
