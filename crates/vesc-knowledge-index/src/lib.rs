//! Searchable firmware and package knowledge index types and builders.

pub mod benchmark;
mod builder;
pub mod corpus;
mod embedded;
mod entry;
pub mod evaluation;
pub mod fusion;
pub mod hardware;
pub mod investigation;
pub mod lexical;
pub mod lifecycle;
pub mod parsers;
pub mod path_evaluation;
mod search;
pub mod semantic;

pub use builder::IndexBuilder;
pub use corpus::chunking::{ChunkingConfig, ChunkingError, chunk_document, chunk_markdown};
#[cfg(feature = "git-corpus")]
pub use corpus::git::GitIngestionObservations;
#[cfg(feature = "git-corpus")]
pub use corpus::history::{
    ChangeEvent, ChangeKind, EmbeddingContract, HistoryContent, HistoryError, HistoryOccurrence,
    HistoryRelease, HistorySemanticHit, HistoryVectorBuildObservations, HistoryVectorIndex,
    TaggedHistory, TaggedHistoryObservations, TaggedHistorySource, ingest_tagged_history,
};
pub use corpus::ingest::{
    IngestionError, IngestionReport, SourceInventory, SourceRejection, SourceSpec,
    vesc_mcp_source_specs,
};
pub use corpus::{
    ArtifactManifest, Chunk, ChunkId, ContentDigest, CorpusManifest, CorpusVersion, DocumentId,
    LicenseStatus, NormalizedDocument, RepositoryId, ResourceUri, Revision, SchemaVersion,
    SourceKind, SourceSpan, TrustTier, validate_chunk_adjacency,
};
pub use embedded::{KnowledgeSearchHit, embedded_entries, search_knowledge};
pub use embedded::{lexical_index, search_lexical_knowledge};
pub use entry::{Category, IndexEntry, SourceRef};
pub use fusion::{
    ExpandedContext, FusedHit, FusionConfig, expand_adjacent_context, fuse_candidates,
};
pub use hardware::{
    JINA_CODE_FP16_SHA256, JINA_CODE_INGEST_BATCH_SIZE, JINA_CODE_INGEST_MAX_LENGTH,
    JINA_CODE_INT8_SHA256, JINA_CODE_MAX_LENGTH, JINA_CODE_MODEL_ID, JINA_CODE_MODEL_REVISION,
    JinaCodeQueryProfile, Rx5700Xt8600gProfile,
};
pub use lexical::{LexicalError, LexicalFilters, LexicalHit, LexicalIndex};
pub use lifecycle::{
    BuildObservations, BuildPhase, BuildSummary, LifecycleError,
    PROVENANCE_OVERHEAD_THRESHOLD_PERCENT, active_manifest_path, build_allowlisted_artifacts,
    build_allowlisted_artifacts_with_provider, build_embedded_artifacts,
    build_embedded_artifacts_with_provider, inspect_manifest,
};
#[cfg(feature = "git-corpus")]
pub use lifecycle::{build_git_artifacts, build_git_artifacts_with_provider};
pub use parsers::native_lib_abi::NativeLibAbiParseError;
pub use parsers::priorities::PrioritiesParseError;
pub use parsers::refloat_commands::RefloatCommandsParseError;
pub use parsers::vesc_c_if::VescCIfParseError;
pub use search::{ScoredEntry, rank_entries};
pub use semantic::{
    DEFAULT_SEMANTIC_BATCH_SIZE, EmbeddingBatchSize, EmbeddingError, EmbeddingProfile,
    EmbeddingProvider, FakeEmbeddingProvider, OutputNormalization, Pooling, SemanticHit,
    SequenceBucket, TokenStatistics, VectorArtifact, VectorBuildObservations, WindowAggregation,
    aggregate_window_vectors, default_semantic_intra_threads, embedding_text, semantic_query_text,
    sequence_bucket_plan,
};
#[cfg(feature = "semantic-fastembed")]
pub use semantic::{DocumentWindowVectors, FastEmbedProvider};
#[cfg(feature = "semantic-fastembed")]
pub use semantic::{
    SemanticExecutionProvider, SemanticRuntimeDiagnostics, SequenceBucketCensus,
    SequenceLengthCensus, configure_ort_verbose_logging, semantic_runtime_diagnostics,
    sequence_length_census,
};
