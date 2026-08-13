//! Searchable firmware and package knowledge index types and builders.

pub mod benchmark;
mod builder;
pub mod corpus;
mod embedded;
mod entry;
pub mod evaluation;
pub mod fusion;
pub mod graph;
pub mod hardware;
pub mod investigation;
pub mod lexical;
pub mod lifecycle;
pub mod parsers;
pub mod path_evaluation;
pub mod pipeline;
pub mod planning;
pub mod release_repositories;
pub mod reranking;
pub mod semantic;

#[cfg(feature = "coz-profile")]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "coz-profile")]
static PROFILE_PROGRESS_LIMIT: AtomicUsize = AtomicUsize::new(usize::MAX);
#[cfg(feature = "coz-profile")]
static PROFILE_PROGRESS_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Configure the profiling-only number of completed work units before exit.
pub fn configure_profile_progress_limit(limit: Option<usize>) {
    #[cfg(feature = "coz-profile")]
    {
        PROFILE_PROGRESS_COUNT.store(0, Ordering::Relaxed);
        PROFILE_PROGRESS_LIMIT.store(limit.unwrap_or(usize::MAX), Ordering::Relaxed);
    }
    #[cfg(not(feature = "coz-profile"))]
    let _ = limit;
}

#[cfg(feature = "coz-profile")]
#[doc(hidden)]
pub fn profile_progress_reached() -> bool {
    let count = PROFILE_PROGRESS_COUNT
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    count >= PROFILE_PROGRESS_LIMIT.load(Ordering::Relaxed)
}

/// Record a Coz progress point and stop a bounded profiling run at a unit boundary.
#[cfg(feature = "coz-profile")]
#[macro_export]
macro_rules! profile_progress {
    ($name:expr) => {{
        coz::progress!($name);
        if $crate::profile_progress_reached() {
            std::process::exit(0);
        }
    }};
}

pub use builder::IndexBuilder;
pub use corpus::chunking::{ChunkingConfig, ChunkingError, chunk_document, chunk_markdown};
pub use corpus::full_history::{
    GitHistoryError, GitHistoryRefreshObservations, GitHistoryTip, ingest_git_history_fast_forward,
};
pub use corpus::git::GitIngestionObservations;
pub use corpus::ingest::{
    IngestionError, IngestionReport, SourceInventory, SourceRejection, SourceSpec,
    vesc_mcp_source_specs,
};
pub use corpus::{
    ArtifactManifest, Chunk, ChunkId, ContentDigest, CorpusManifest, CorpusVersion, DocumentId,
    LicenseStatus, NormalizedDocument, RepositoryId, ResourceUri, RetrievalMetadata, Revision,
    SchemaVersion, SourceKind, SourceSpan, TrustTier, validate_chunk_adjacency,
};
pub use embedded::{embedded_entries, lexical_index, search_lexical_knowledge};
pub use entry::{Category, IndexEntry, SourceRef};
pub use fusion::{
    ExpandedContext, FusedCandidate, FusedHit, FusionConfig, expand_adjacent_context,
    fuse_candidate_metadata, fuse_candidates,
};
pub use graph::{
    GRAPH_ARTIFACT_SCHEMA_V1, GraphArtifact, GraphArtifactError, GraphArtifactSummary, GraphEdge,
    GraphEvidence, GraphNode,
};
pub use hardware::{
    JINA_CODE_FP16_SHA256, JINA_CODE_INGEST_BATCH_SIZE, JINA_CODE_INGEST_MAX_LENGTH,
    JINA_CODE_INT8_SHA256, JINA_CODE_MAX_LENGTH, JINA_CODE_MODEL_ID, JINA_CODE_MODEL_REVISION,
    JinaCodeQueryProfile, Rx5700Xt8600gProfile, sha256_file,
};
pub use lexical::{LexicalCandidate, LexicalError, LexicalFilters, LexicalHit, LexicalIndex};
pub use lifecycle::{
    BuildObservations, BuildPhase, BuildSummary, LifecycleError,
    PROVENANCE_OVERHEAD_THRESHOLD_PERCENT, active_generation_path, active_manifest_path,
    artifact_component_versions, build_allowlisted_artifacts,
    build_allowlisted_artifacts_with_provider, build_embedded_artifacts,
    build_embedded_artifacts_with_provider, git_history_corpus_versions_are_compatible,
    inspect_manifest, inspect_previous_artifact, validate_active_generation,
};
pub use lifecycle::{
    IncrementalGitHistoryBuildSummary, PreviousArtifactSummary, PreviousGitHistoryArtifact,
    PreviousVectorArtifact, build_git_artifacts, build_git_artifacts_with_provider,
    build_git_history_artifacts_from_previous, build_git_history_artifacts_incrementally,
    remove_git_history_lexical_stage,
};
pub use parsers::native_lib_abi::NativeLibAbiParseError;
pub use parsers::priorities::PrioritiesParseError;
pub use parsers::refloat_commands::RefloatCommandsParseError;
pub use parsers::vesc_c_if::VescCIfParseError;
pub use semantic::{
    DEFAULT_SEMANTIC_BATCH_SIZE, EmbeddingBatchSize, EmbeddingError, EmbeddingProfile,
    EmbeddingProvider, FakeEmbeddingProvider, FileBackedVectorArtifact, OutputNormalization,
    Pooling, SemanticHit, SequenceBucket, TokenStatistics, VectorArtifact, VectorBuildObservations,
    VectorSearch, WindowAggregation, aggregate_window_vectors, default_semantic_intra_threads,
    embedding_text, semantic_query_text, sequence_bucket_plan,
};
#[cfg(feature = "semantic-fastembed")]
pub use semantic::{DocumentWindowVectors, FastEmbedProvider};
#[cfg(feature = "semantic-fastembed")]
pub use semantic::{
    SemanticExecutionProvider, SemanticRuntimeDiagnostics, SequenceBucketCensus,
    SequenceLengthCensus, configure_ort_verbose_logging, semantic_runtime_diagnostics,
    sequence_length_census, sequence_length_census_iter,
};
