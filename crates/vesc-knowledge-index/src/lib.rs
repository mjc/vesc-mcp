//! Searchable firmware and package knowledge index types and builders.

pub mod benchmark;
mod builder;
pub mod corpus;
mod embedded;
mod entry;
pub mod evaluation;
pub mod fusion;
pub mod lexical;
pub mod lifecycle;
pub mod parsers;
mod search;
pub mod semantic;

pub use builder::IndexBuilder;
pub use corpus::chunking::{ChunkingConfig, ChunkingError, chunk_document, chunk_markdown};
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
pub use lexical::{LexicalError, LexicalFilters, LexicalHit, LexicalIndex};
pub use lifecycle::{
    BuildSummary, LifecycleError, active_manifest_path, build_allowlisted_artifacts,
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
#[cfg(feature = "semantic-fastembed")]
pub use semantic::FastEmbedProvider;
pub use semantic::{
    EmbeddingError, EmbeddingProvider, FakeEmbeddingProvider, SemanticHit, VectorArtifact,
    semantic_query_text,
};
