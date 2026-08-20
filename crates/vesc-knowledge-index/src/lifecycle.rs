//! Reproducible corpus and lexical artifact lifecycle helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize, de::IgnoredAny};

use crate::corpus::chunking::{ChunkingConfig, chunk_document, chunk_document_drafts};
use crate::corpus::full_history::{
    GitHistoryBuildPlan, GitHistoryError, GitHistoryRefreshObservations, GitHistoryTip,
    plan_git_history_fast_forward_delta, plan_git_history_fast_forward_owned,
};
use crate::corpus::git::{
    GitCorpusSource, GitIngestionError, GitIngestionObservations, MAX_IDENTIFIERS,
    MAX_REJECTION_SAMPLES, identifier_values, ingest_git_commit,
};
use crate::corpus::ingest::{SourceInventory, SourceRejection, SourceSpec, ingest_allowlisted};
use crate::corpus::{
    ARTIFACT_SCHEMA_V1, ArtifactManifest, Chunk, ContentDigest, CorpusManifest, CorpusVersion,
    NormalizedDocument, RepositoryId, Revision, SchemaVersion,
};
use crate::lexical::EmbeddingTextHydrator;
use crate::semantic::ReconciledVectorArtifact;
use crate::{
    EmbeddingError, EmbeddingProvider, LexicalError, LexicalIndex, VectorArtifact,
    VectorBuildObservations, embedded_entries,
};

/// Errors while building or inspecting generated retrieval artifacts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LifecycleError {
    #[error("artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("graph artifact failed: {0}")]
    Graph(#[from] crate::GraphArtifactError),
    #[error("artifact contract failed: {0}")]
    Contract(String),
    #[error("lexical artifact failed: {0}")]
    Lexical(#[from] LexicalError),
    #[error("vector artifact failed: {0}")]
    Vector(#[from] EmbeddingError),
    #[error("Git corpus ingestion failed: {0}")]
    Git(#[from] GitIngestionError),
    #[error("Git history ingestion failed: {0}")]
    GitHistory(#[from] GitHistoryError),
}

/// Non-identity phase names used by build observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildPhase {
    Ingestion,
    Chunking,
    Corpus,
    Lexical,
    EmbeddingInput,
    Inference,
    VectorFinalization,
    Encoding,
    Writing,
    Manifest,
    Validation,
    Activation,
}

/// Aggregate build timings and counters. These values are intentionally kept
/// out of manifests, generation IDs, and checksums.
///
/// Provenance overhead is considered material at 5% of serialized retrieval
/// artifacts. The threshold is a reporting policy only: provenance remains in
/// the manifest and diagnostics regardless of the result.
pub const PROVENANCE_OVERHEAD_THRESHOLD_PERCENT: u64 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildObservations {
    pub total_duration_us: u64,
    pub phases_us: BTreeMap<BuildPhase, u64>,
    pub visited_files: u64,
    pub accepted_files: u64,
    pub rejected_files: u64,
    pub accepted_source_bytes: u64,
    pub documents: usize,
    pub chunks: usize,
    pub embedding_input_bytes: u64,
    #[serde(default)]
    pub embedding_git_blob_loads: usize,
    pub vector_count: usize,
    pub vector_dimension: Option<usize>,
    pub artifact_bytes: u64,
    pub manifest_bytes: u64,
    pub active_manifest_bytes: u64,
    pub inventory_count: usize,
    pub rejection_count: usize,
    pub resolved_batch_size: Option<usize>,
    pub vector_build: Option<VectorBuildObservations>,
    #[serde(default)]
    pub git_ingestion: Option<GitIngestionObservations>,
    #[serde(default)]
    pub reused_lexical_stage: bool,
}

/// The small atomic selector stored at an artifact root.
///
/// The generation manifest remains the complete inspectable provenance record;
/// this pointer avoids storing that record a second time in `active.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveManifestPointer {
    schema: SchemaVersion,
    generation: ContentDigest,
    manifest_checksum: ContentDigest,
}

impl ActiveManifestPointer {
    fn new(generation: &str, manifest_bytes: &[u8]) -> Result<Self, LifecycleError> {
        Ok(Self {
            schema: ARTIFACT_SCHEMA_V1,
            generation: ContentDigest::try_from(generation)
                .map_err(|error| LifecycleError::Contract(error.to_string()))?,
            manifest_checksum: ContentDigest::of(manifest_bytes),
        })
    }

    fn validate(&self) -> Result<(), LifecycleError> {
        self.schema
            .ensure_major(ARTIFACT_SCHEMA_V1, "active manifest")
            .map(|_| ())
            .map_err(|error| LifecycleError::Contract(error.to_string()))
    }
}

impl BuildObservations {
    #[must_use]
    pub const fn provenance_bytes(&self) -> u64 {
        self.manifest_bytes
            .saturating_add(self.active_manifest_bytes)
    }

    #[must_use]
    pub fn provenance_overhead_percent(&self) -> Option<u64> {
        (self.artifact_bytes > 0).then(|| {
            self.provenance_bytes()
                .saturating_mul(100)
                .checked_div(self.artifact_bytes)
                .unwrap_or(u64::MAX)
        })
    }

    #[must_use]
    pub fn provenance_overhead_is_material(&self) -> bool {
        self.provenance_overhead_percent()
            .is_some_and(|percent| percent >= PROVENANCE_OVERHEAD_THRESHOLD_PERCENT)
    }

    fn record(&mut self, phase: BuildPhase, started: Instant) {
        self.phases_us.insert(phase, elapsed_us(started));
    }

    fn record_duration(&mut self, phase: BuildPhase, duration_us: u64) {
        self.phases_us.insert(phase, duration_us);
    }
}

/// Summary returned after a staged embedded-corpus build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSummary {
    pub generation: String,
    pub document_count: usize,
    pub chunk_count: usize,
    pub lexical_bytes: u64,
    pub vector_bytes: Option<u64>,
    pub graph_bytes: Option<u64>,
    pub build_duration_us: u64,
    pub observations: BuildObservations,
    pub manifest: ArtifactManifest,
}

/// Summary for a complete-history build seeded from a prior immutable snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalGitHistoryBuildSummary {
    pub artifacts: BuildSummary,
    pub refresh: GitHistoryRefreshObservations,
    pub reused_snapshot: bool,
}

/// Build and atomically activate the embedded corpus generation under `root`.
///
/// The generation manifest contains portable IDs, provenance, and checksums;
/// `active.json` is a small checksum-verified selector for that manifest. Files
/// are written beneath a same-filesystem temporary directory before activation.
///
/// # Errors
///
/// Returns [`LifecycleError`] when migration, serialization, validation, or
/// staged activation fails.
pub fn build_embedded_artifacts(root: &Path) -> Result<BuildSummary, LifecycleError> {
    build_artifacts(root, None)
}

/// Build and atomically activate the embedded corpus with a vector artifact.
///
/// Model construction and model-file policy stay outside lifecycle code; the
/// caller supplies an already initialized provider.
///
/// # Errors
///
/// Returns [`LifecycleError`] when embedding, serialization, validation, or
/// staged activation fails.
pub fn build_embedded_artifacts_with_provider(
    root: &Path,
    provider: &mut impl EmbeddingProvider,
    model_id: &str,
    model_revision: &str,
) -> Result<BuildSummary, LifecycleError> {
    build_artifacts(
        root,
        Some(SemanticBuild {
            provider,
            model_id,
            model_revision,
        }),
    )
}

struct SemanticBuild<'a> {
    provider: &'a mut dyn EmbeddingProvider,
    model_id: &'a str,
    model_revision: &'a str,
}

struct IncrementalStage {
    lexical_path: PathBuf,
}

struct ReconciledVectorStage {
    lexical_path: PathBuf,
    path: PathBuf,
    checksum: ContentDigest,
    corpus_digest: ContentDigest,
}

/// Persisted vector data eligible for content-addressed reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousVectorArtifact {
    lexical_path: PathBuf,
    corpus_digest: ContentDigest,
    checksum: ContentDigest,
    path: PathBuf,
    model_id: String,
    model_revision: String,
    dimension: usize,
}

impl PreviousVectorArtifact {
    /// Open and validate vector data that may seed a selected-tree build.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the checksum, schema, corpus, or model
    /// contract does not match.
    pub fn open(
        lexical_path: PathBuf,
        corpus_digest: ContentDigest,
        checksum: ContentDigest,
        path: PathBuf,
        model_id: &str,
        model_revision: &str,
        provider_dimension: Option<usize>,
    ) -> Result<Self, EmbeddingError> {
        let dimension = VectorArtifact::validate_reusable_artifact_dimension(
            &path,
            &checksum,
            &corpus_digest,
            model_id,
            model_revision,
            provider_dimension,
        )?;
        Ok(Self {
            lexical_path,
            corpus_digest,
            checksum,
            path,
            model_id: model_id.to_owned(),
            model_revision: model_revision.to_owned(),
            dimension,
        })
    }

    fn matches(
        &self,
        model_id: &str,
        model_revision: &str,
        provider_dimension: Option<usize>,
    ) -> bool {
        self.model_id == model_id
            && self.model_revision == model_revision
            && provider_dimension.is_none_or(|dimension| dimension == self.dimension)
    }
}

/// Validated predecessor data used by a complete-history fast-forward build.
pub struct PreviousGitHistoryArtifact {
    pub tips: Vec<GitHistoryTip>,
    pub lexical_path: PathBuf,
    pub corpus_digest: ContentDigest,
    pub vector_checksum: Option<ContentDigest>,
    pub vector_path: Option<PathBuf>,
    pub lexical_format_compatible: bool,
}

/// Allocation-bounded metadata needed to reuse an existing artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousArtifactSummary {
    pub generation: ContentDigest,
    pub corpus_version: CorpusVersion,
    pub corpus_digest: ContentDigest,
    pub document_count: usize,
    pub chunk_count: usize,
    pub source_count: usize,
    pub diagnostic_count: usize,
    pub component_versions: BTreeMap<String, String>,
    pub lexical_checksum: Option<ContentDigest>,
    pub vector_checksum: Option<ContentDigest>,
    pub graph_checksum: Option<ContentDigest>,
    pub graph_node_count: Option<u64>,
    pub graph_edge_count: Option<u64>,
}

#[derive(Deserialize)]
struct PreviousArtifactProjection {
    schema: SchemaVersion,
    corpus: PreviousCorpusProjection,
    #[serde(default)]
    component_versions: BTreeMap<String, String>,
    #[serde(default)]
    sources: Vec<IgnoredAny>,
    lexical_checksum: Option<ContentDigest>,
    vector_checksum: Option<ContentDigest>,
    #[serde(default)]
    graph_checksum: Option<ContentDigest>,
    #[serde(default)]
    graph_node_count: Option<u64>,
    #[serde(default)]
    graph_edge_count: Option<u64>,
    #[serde(default)]
    diagnostics: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct PreviousCorpusProjection {
    schema: SchemaVersion,
    corpus_version: CorpusVersion,
    #[serde(default)]
    documents: Vec<IgnoredAny>,
    #[serde(default)]
    chunks: Vec<IgnoredAny>,
    #[serde(default)]
    document_count: usize,
    #[serde(default)]
    chunk_count: usize,
    content_digest: ContentDigest,
}

const GIT_HISTORY_LEXICAL_STAGE_SCHEMA: SchemaVersion = SchemaVersion { major: 1, minor: 3 };
const GIT_HISTORY_LEXICAL_STAGE_DIR: &str = "lexical-stage";
const GIT_HISTORY_LEXICAL_STAGE_MARKER: &str = "complete.json";

#[derive(Serialize)]
struct GitHistoryLexicalContract<'a> {
    schema: SchemaVersion,
    corpus_version: &'static str,
    chunking: ChunkingConfig,
    component_versions: &'a BTreeMap<String, String>,
    embedded_catalog: ContentDigest,
    sources: Vec<GitHistorySourceContract<'a>>,
}

#[derive(Serialize)]
struct GitHistorySourceContract<'a> {
    repository: &'a RepositoryId,
    revision: &'a Revision,
    history_tips: Vec<&'a Revision>,
    trust_tier: crate::TrustTier,
    license: &'a crate::LicenseStatus,
    include_prefixes: &'a [String],
    exclude_prefixes: &'a [String],
    include_patterns: &'a [String],
    exclude_patterns: &'a [String],
    extensions: &'a BTreeSet<String>,
    filenames: &'a BTreeSet<String>,
    max_file_bytes: u64,
    max_files: usize,
    max_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHistoryLexicalStageRefresh {
    reachable_commits: usize,
    reused_commits: usize,
    ingested_commits: usize,
    ingested_blobs: usize,
    reused_blobs: usize,
    #[serde(default)]
    ingested_commit_messages: usize,
    #[serde(default)]
    reused_commit_messages: usize,
    #[serde(default)]
    removed_documents: usize,
    #[serde(default)]
    removed_commit_messages: usize,
    #[serde(default)]
    rejected_commit_messages: usize,
    #[serde(default)]
    budget_rejections: usize,
    reused_contents: usize,
    candidate_chunks: usize,
    materialized_chunks: usize,
    candidate_identifier_count_histogram: Vec<u64>,
    materialized_identifier_count_histogram: Vec<u64>,
    git: GitIngestionObservations,
}

impl GitHistoryLexicalStageRefresh {
    fn from_refresh(refresh: &GitHistoryRefreshObservations) -> Self {
        Self {
            reachable_commits: refresh.reachable_commits,
            reused_commits: refresh.reused_commits,
            ingested_commits: refresh.ingested_commits,
            ingested_blobs: refresh.ingested_blobs,
            reused_blobs: refresh.reused_blobs,
            ingested_commit_messages: refresh.ingested_commit_messages,
            reused_commit_messages: refresh.reused_commit_messages,
            removed_documents: refresh.removed_documents,
            removed_commit_messages: refresh.removed_commit_messages,
            rejected_commit_messages: refresh.rejected_commit_messages,
            budget_rejections: refresh.budget_rejections,
            reused_contents: refresh.reused_contents,
            candidate_chunks: refresh.candidate_chunks,
            materialized_chunks: refresh.materialized_chunks,
            candidate_identifier_count_histogram: refresh
                .candidate_identifier_count_histogram
                .to_vec(),
            materialized_identifier_count_histogram: refresh
                .materialized_identifier_count_histogram
                .to_vec(),
            git: refresh.git.clone(),
        }
    }

    fn to_refresh(&self) -> Result<GitHistoryRefreshObservations, LifecycleError> {
        if self.candidate_identifier_count_histogram.len() != MAX_IDENTIFIERS + 1
            || self.materialized_identifier_count_histogram.len() != MAX_IDENTIFIERS + 1
        {
            return Err(LifecycleError::Contract(
                "Git history lexical stage histogram is invalid".into(),
            ));
        }
        let mut candidate_identifier_count_histogram = [0; MAX_IDENTIFIERS + 1];
        candidate_identifier_count_histogram
            .copy_from_slice(&self.candidate_identifier_count_histogram);
        let mut materialized_identifier_count_histogram = [0; MAX_IDENTIFIERS + 1];
        materialized_identifier_count_histogram
            .copy_from_slice(&self.materialized_identifier_count_histogram);
        Ok(GitHistoryRefreshObservations {
            reachable_commits: self.reachable_commits,
            reused_commits: self.reused_commits,
            ingested_commits: self.ingested_commits,
            ingested_blobs: self.ingested_blobs,
            reused_blobs: self.reused_blobs,
            ingested_commit_messages: self.ingested_commit_messages,
            reused_commit_messages: self.reused_commit_messages,
            removed_documents: self.removed_documents,
            removed_commit_messages: self.removed_commit_messages,
            rejected_commit_messages: self.rejected_commit_messages,
            budget_rejections: self.budget_rejections,
            reused_contents: self.reused_contents,
            candidate_chunks: self.candidate_chunks,
            materialized_chunks: self.materialized_chunks,
            candidate_identifier_count_histogram,
            materialized_identifier_count_histogram,
            git: self.git.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHistoryLexicalStageMarker {
    schema: SchemaVersion,
    contract_digest: ContentDigest,
    lexical_checksum: ContentDigest,
    lexical_sidecar_checksum: ContentDigest,
    lexical_bytes: u64,
    corpus: CorpusManifest,
    history_chunks: usize,
    refresh: GitHistoryLexicalStageRefresh,
    reused_snapshot: bool,
}

struct GitHistoryLexicalStage {
    root: PathBuf,
    marker: GitHistoryLexicalStageMarker,
}

impl GitHistoryLexicalStage {
    fn lexical_path(&self) -> PathBuf {
        self.root.join("lexical.json")
    }
}

fn git_history_lexical_stage_path(root: &Path) -> PathBuf {
    root.join(GIT_HISTORY_LEXICAL_STAGE_DIR)
}

fn git_history_lexical_contract_digest(
    sources: &[GitCorpusSource],
) -> Result<ContentDigest, LifecycleError> {
    let component_versions = git_history_lexical_component_versions();
    let embedded_catalog = ContentDigest::of(&serde_json::to_vec(&embedded_catalog_chunks()?)?);
    let sources = sources
        .iter()
        .map(|source| {
            let mut history_tips = source.history_tips.iter().collect::<Vec<_>>();
            history_tips.sort_unstable();
            history_tips.dedup();
            GitHistorySourceContract {
                repository: &source.repository_id,
                revision: &source.revision,
                history_tips,
                trust_tier: source.trust_tier,
                license: &source.license,
                include_prefixes: &source.policy.include_prefixes,
                exclude_prefixes: &source.policy.exclude_prefixes,
                include_patterns: &source.policy.include_patterns,
                exclude_patterns: &source.policy.exclude_patterns,
                extensions: &source.policy.extensions,
                filenames: &source.policy.filenames,
                max_file_bytes: source.policy.limits.max_file_bytes(),
                max_files: source.policy.limits.max_files(),
                max_total_bytes: source.policy.limits.max_total_bytes(),
            }
        })
        .collect();
    let contract = GitHistoryLexicalContract {
        schema: GIT_HISTORY_LEXICAL_STAGE_SCHEMA,
        corpus_version: "git-full-history-v1",
        chunking: ChunkingConfig::default(),
        component_versions: &component_versions,
        embedded_catalog,
        sources,
    };
    Ok(ContentDigest::of(&serde_json::to_vec(&contract)?))
}

fn read_git_history_lexical_stage(
    root: &Path,
    sources: &[GitCorpusSource],
) -> Result<GitHistoryLexicalStage, LifecycleError> {
    let stage_root = git_history_lexical_stage_path(root);
    let marker: GitHistoryLexicalStageMarker = serde_json::from_slice(&fs::read(
        stage_root.join(GIT_HISTORY_LEXICAL_STAGE_MARKER),
    )?)?;
    if marker.schema != GIT_HISTORY_LEXICAL_STAGE_SCHEMA
        || marker.contract_digest != git_history_lexical_contract_digest(sources)?
    {
        return Err(LifecycleError::Contract(
            "Git history lexical stage contract does not match".into(),
        ));
    }
    marker.refresh.to_refresh()?;
    marker
        .corpus
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let lexical_path = stage_root.join("lexical.json");
    let lexical_bytes = fs::read(&lexical_path)?;
    if u64::try_from(lexical_bytes.len()).unwrap_or(u64::MAX) != marker.lexical_bytes
        || ContentDigest::of(&lexical_bytes) != marker.lexical_checksum
    {
        return Err(LifecycleError::Contract(
            "Git history lexical stage checksum does not match".into(),
        ));
    }
    if LexicalIndex::sidecar_checksum(&lexical_path)? != marker.lexical_sidecar_checksum {
        return Err(LifecycleError::Contract(
            "Git history lexical stage sidecar checksum does not match".into(),
        ));
    }
    let (documents, chunks, digest) = LexicalIndex::corpus_inventory(&lexical_path)?;
    if documents != marker.corpus.document_count()
        || chunks != marker.corpus.chunk_count()
        || digest != marker.corpus.content_digest
    {
        return Err(LifecycleError::Contract(
            "Git history lexical stage inventory does not match".into(),
        ));
    }
    Ok(GitHistoryLexicalStage {
        root: stage_root,
        marker,
    })
}

fn load_git_history_lexical_stage(
    root: &Path,
    sources: &[GitCorpusSource],
) -> Result<Option<GitHistoryLexicalStage>, LifecycleError> {
    let stage_root = git_history_lexical_stage_path(root);
    if !stage_root.exists() {
        return Ok(None);
    }
    if let Ok(stage) = read_git_history_lexical_stage(root, sources) {
        Ok(Some(stage))
    } else {
        if stage_root.is_dir() {
            fs::remove_dir_all(stage_root)?;
        } else {
            fs::remove_file(stage_root)?;
        }
        Ok(None)
    }
}

/// Remove a completed private lexical stage after its snapshot is durable.
///
/// # Errors
///
/// Returns an I/O error when the stage exists but cannot be removed.
pub fn remove_git_history_lexical_stage(root: &Path) -> Result<(), LifecycleError> {
    let stage_root = git_history_lexical_stage_path(root);
    match fs::remove_dir_all(stage_root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn build_git_history_cold(
    root: &Path,
    sources: &[GitCorpusSource],
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    vector_checkpoint_path: Option<&Path>,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<IncrementalGitHistoryBuildSummary, LifecycleError> {
    build_git_history_artifacts_incrementally(
        root,
        sources,
        None,
        None,
        semantic,
        None,
        vector_checkpoint_path,
        progress,
    )
}

fn build_artifacts(
    root: &Path,
    semantic: Option<SemanticBuild<'_>>,
) -> Result<BuildSummary, LifecycleError> {
    let started = Instant::now();
    let ingest_started = Instant::now();
    let chunking_started = Instant::now();
    let chunks = embedded_catalog_chunks()?;
    let mut observations = BuildObservations::default();
    observations.record(BuildPhase::Ingestion, ingest_started);
    observations.record(BuildPhase::Chunking, chunking_started);
    stage_chunks(
        root,
        &chunks,
        None,
        semantic,
        "embedded-catalog-v1",
        Vec::new(),
        Vec::new(),
        started,
        observations,
        None,
        None,
    )
}

fn embedded_catalog_chunks() -> Result<Vec<crate::Chunk>, LifecycleError> {
    embedded_entries()
        .iter()
        .map(|entry| {
            NormalizedDocument::from_catalog_entry(entry)
                .and_then(|document| document.catalog_chunk())
                .map_err(|error| LifecycleError::Contract(error.to_string()))
        })
        .collect()
}

/// Build artifacts from an explicit, allowlisted source inventory.
///
/// # Errors
///
/// Returns [`LifecycleError`] when ingestion, chunking, validation, or staged
/// activation fails.
pub fn build_allowlisted_artifacts(
    root: &Path,
    source_root: &Path,
    repository: &RepositoryId,
    revision: &Revision,
    specs: &[SourceSpec],
) -> Result<BuildSummary, LifecycleError> {
    build_allowlisted_artifacts_with_provider(root, source_root, repository, revision, specs, None)
}

/// Build artifacts from an allowlisted source inventory and an embedding provider.
///
/// # Errors
///
/// Returns [`LifecycleError`] when ingestion, chunking, embedding, validation,
/// or staged activation fails.
pub fn build_allowlisted_artifacts_with_provider(
    root: &Path,
    source_root: &Path,
    repository: &RepositoryId,
    revision: &Revision,
    specs: &[SourceSpec],
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
) -> Result<BuildSummary, LifecycleError> {
    let started = Instant::now();
    let ingest_started = Instant::now();
    let report = ingest_allowlisted(source_root, repository, revision, specs)
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let mut observations = BuildObservations::default();
    observations.record(BuildPhase::Ingestion, ingest_started);
    let crate::corpus::ingest::IngestionReport {
        documents,
        rejected,
        sources,
        visited_files,
        ..
    } = report;
    let chunking_started = Instant::now();
    let mut chunks = embedded_catalog_chunks()?;
    for document in documents {
        chunks.extend(
            chunk_document(&document, ChunkingConfig::default())
                .map_err(|error| LifecycleError::Contract(error.to_string()))?,
        );
    }
    if chunks.is_empty() {
        return Err(LifecycleError::Contract(
            "allowlisted sources produced no chunks".into(),
        ));
    }
    observations.record(BuildPhase::Chunking, chunking_started);
    observations.inventory_count = sources.len();
    observations.rejection_count = rejected.len();
    observations.visited_files = visited_files as u64;
    observations.rejected_files = rejected.len() as u64;
    observations.accepted_files = sources
        .iter()
        .filter(|source| source.rejection.is_none())
        .count() as u64;
    observations.accepted_source_bytes = sources
        .iter()
        .filter(|source| source.rejection.is_none())
        .filter_map(|source| source.byte_count)
        .sum();
    let semantic = semantic.map(|(provider, model_id, model_revision)| SemanticBuild {
        provider,
        model_id,
        model_revision,
    });
    stage_chunks(
        root,
        &chunks,
        None,
        semantic,
        "allowlisted-v1",
        rejected,
        sources,
        started,
        observations,
        None,
        None,
    )
}

/// Build an additive corpus from the embedded catalog and immutable Git trees.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git ingestion, chunking, or artifact staging fails.
pub fn build_git_artifacts(
    root: &Path,
    sources: &[GitCorpusSource],
) -> Result<BuildSummary, LifecycleError> {
    build_git_artifacts_with_provider(root, sources, None, None, None)
}

/// Build complete Git history, reconciling reachable evidence with cached chunks.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git inspection, chunking, embedding, or staging fails.
#[allow(clippy::too_many_arguments)] // Public lifecycle inputs remain explicit.
pub fn build_git_history_artifacts_incrementally(
    root: &Path,
    sources: &[GitCorpusSource],
    previous_tips: Option<Vec<GitHistoryTip>>,
    previous_chunks: Option<Vec<crate::Chunk>>,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<VectorArtifact>,
    vector_checkpoint_path: Option<&Path>,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<IncrementalGitHistoryBuildSummary, LifecycleError> {
    let started = Instant::now();
    if let Some(stage) = load_git_history_lexical_stage(root, sources)? {
        return resume_git_history_lexical_stage(
            root,
            sources,
            &stage,
            semantic,
            previous_vectors,
            None,
            vector_checkpoint_path,
            started,
            progress,
        );
    }
    let ingestion_started = Instant::now();
    let incremental = previous_tips
        .zip(previous_chunks)
        .map_or(Ok(None), |(tips, chunks)| {
            plan_git_history_fast_forward_owned(sources, &tips, chunks)
        })?;
    let (history_plan, refresh, reused_snapshot) = if let Some((plan, refresh)) = incremental {
        (plan, refresh, true)
    } else {
        let (plan, refresh) = plan_git_history_fast_forward_owned(sources, &[], Vec::new())?
            .ok_or_else(|| {
                LifecycleError::Contract("cold Git history ingestion was rejected".into())
            })?;
        (plan, refresh, false)
    };
    let mut observations = BuildObservations::default();
    observations.record_duration(BuildPhase::Ingestion, elapsed_us(ingestion_started));
    observations.git_ingestion = Some(refresh.git.clone());
    observations.accepted_files = u64::try_from(history_plan.len()).unwrap_or(u64::MAX);
    observations.visited_files = observations.accepted_files;
    let artifacts = stage_git_history_plan(
        root,
        sources,
        history_plan,
        &refresh,
        reused_snapshot,
        semantic,
        previous_vectors,
        None,
        None,
        vector_checkpoint_path,
        started,
        observations,
        progress,
    )?;
    Ok(IncrementalGitHistoryBuildSummary {
        artifacts,
        refresh,
        reused_snapshot,
    })
}

/// Build a complete-history snapshot by reconciling a persisted predecessor.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git inspection, chunking, embedding, or
/// staging fails.
#[allow(clippy::too_many_lines)] // One fallback tree preserves owned provider state.
pub fn build_git_history_artifacts_from_previous(
    root: &Path,
    sources: &[GitCorpusSource],
    previous: Option<PreviousGitHistoryArtifact>,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    vector_checkpoint_path: Option<&Path>,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<IncrementalGitHistoryBuildSummary, LifecycleError> {
    let started = Instant::now();
    if let Some(stage) = load_git_history_lexical_stage(root, sources)? {
        let reconciled_vectors = previous
            .as_ref()
            .and_then(|previous| reusable_previous_vector_stage(previous, semantic.as_ref()));
        return resume_git_history_lexical_stage(
            root,
            sources,
            &stage,
            semantic,
            None,
            reconciled_vectors,
            vector_checkpoint_path,
            started,
            progress,
        );
    }
    let Some(previous) = previous else {
        return build_git_history_cold(root, sources, semantic, vector_checkpoint_path, progress);
    };
    let reconciled_vectors = reusable_previous_vector_stage(&previous, semantic.as_ref());
    if !previous.lexical_format_compatible {
        return build_git_history_reindexing(
            root,
            sources,
            reconciled_vectors,
            semantic,
            vector_checkpoint_path,
            progress,
        );
    }

    if !matches!(
        LexicalIndex::corpus_inventory(&previous.lexical_path),
        Ok((_documents, _chunks, digest)) if digest == previous.corpus_digest
    ) {
        return build_git_history_cold(root, sources, semantic, vector_checkpoint_path, progress);
    }
    let Some(projection) = LexicalIndex::read_history_projection(&previous.lexical_path)? else {
        // Legacy artifacts have no compact history projection. Rebuild them
        // instead of hydrating every stored Tantivy document just to recover
        // the reconciliation metadata.
        return build_git_history_cold(root, sources, semantic, vector_checkpoint_path, progress);
    };
    let (cached_history, membership) = projection.into_parts();
    let ingestion_started = Instant::now();
    let mut previous_contains =
        |key: &ContentDigest, revision: &gix::ObjectId, removed_document_ids: &BTreeSet<String>| {
            Ok(membership.contains_retained(key, revision, removed_document_ids))
        };
    let incremental = plan_git_history_fast_forward_delta(
        sources,
        &previous.tips,
        cached_history,
        &mut previous_contains,
    );
    let Some((delta, refresh)) = incremental? else {
        return build_git_history_cold(root, sources, semantic, vector_checkpoint_path, progress);
    };
    let mut observations = BuildObservations::default();
    observations.record_duration(BuildPhase::Ingestion, elapsed_us(ingestion_started));
    observations.git_ingestion = Some(refresh.git.clone());
    observations.accepted_files = u64::try_from(delta.len()).unwrap_or(u64::MAX);
    observations.visited_files = observations.accepted_files;
    let incremental = IncrementalStage {
        lexical_path: previous.lexical_path,
    };
    let artifacts = stage_git_history_plan(
        root,
        sources,
        delta,
        &refresh,
        true,
        semantic,
        None,
        reconciled_vectors,
        Some(&incremental),
        vector_checkpoint_path,
        started,
        observations,
        progress,
    )?;
    Ok(IncrementalGitHistoryBuildSummary {
        artifacts,
        refresh,
        reused_snapshot: true,
    })
}

fn reusable_previous_vector_stage(
    previous: &PreviousGitHistoryArtifact,
    semantic: Option<&(&mut dyn EmbeddingProvider, &str, &str)>,
) -> Option<ReconciledVectorStage> {
    let (provider, model_id, model_revision) = semantic?;
    let vector_path = previous.vector_path.as_ref()?;
    let vector_checksum = previous.vector_checksum.as_ref()?;
    VectorArtifact::validate_reusable_artifact(
        vector_path,
        vector_checksum,
        &previous.corpus_digest,
        model_id,
        model_revision,
        provider.embedding_dimension(),
    )
    .ok()?;
    Some(ReconciledVectorStage {
        lexical_path: previous.lexical_path.clone(),
        path: vector_path.clone(),
        checksum: vector_checksum.clone(),
        corpus_digest: previous.corpus_digest.clone(),
    })
}

fn reusable_vector_stage(
    previous: &PreviousVectorArtifact,
    semantic: Option<&(&mut dyn EmbeddingProvider, &str, &str)>,
) -> Option<ReconciledVectorStage> {
    let (provider, model_id, model_revision) = semantic?;
    previous
        .matches(model_id, model_revision, provider.embedding_dimension())
        .then_some(())?;
    Some(ReconciledVectorStage {
        lexical_path: previous.lexical_path.clone(),
        path: previous.path.clone(),
        checksum: previous.checksum.clone(),
        corpus_digest: previous.corpus_digest.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn resume_git_history_lexical_stage(
    root: &Path,
    sources: &[GitCorpusSource],
    stage: &GitHistoryLexicalStage,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<VectorArtifact>,
    reconciled_vectors: Option<ReconciledVectorStage>,
    vector_checkpoint_path: Option<&Path>,
    started: Instant,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<IncrementalGitHistoryBuildSummary, LifecycleError> {
    let refresh = stage.marker.refresh.to_refresh()?;
    let staged_reuse = stage.marker.reused_snapshot;
    let accepted_files = u64::try_from(stage.marker.history_chunks).unwrap_or(u64::MAX);
    let mut observations = BuildObservations {
        visited_files: accepted_files,
        accepted_files,
        documents: stage.marker.corpus.document_count(),
        chunks: stage.marker.corpus.chunk_count(),
        git_ingestion: Some(refresh.git.clone()),
        reused_lexical_stage: true,
        ..BuildObservations::default()
    };
    observations.record_duration(BuildPhase::Ingestion, 0);
    let artifacts = finish_git_history_lexical_stage(
        root,
        sources,
        stage,
        semantic,
        previous_vectors,
        reconciled_vectors,
        vector_checkpoint_path,
        started,
        observations,
        progress,
    )?;
    let reused_snapshot = staged_reuse
        || artifacts
            .observations
            .vector_build
            .as_ref()
            .is_some_and(|vectors| vectors.reused_vectors > 0);
    Ok(IncrementalGitHistoryBuildSummary {
        artifacts,
        refresh,
        reused_snapshot,
    })
}

fn build_git_history_reindexing(
    root: &Path,
    sources: &[GitCorpusSource],
    reconciled_vectors: Option<ReconciledVectorStage>,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    vector_checkpoint_path: Option<&Path>,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<IncrementalGitHistoryBuildSummary, LifecycleError> {
    let started = Instant::now();
    let ingestion_started = Instant::now();
    let (history_plan, refresh) = plan_git_history_fast_forward_owned(sources, &[], Vec::new())?
        .ok_or_else(|| {
            LifecycleError::Contract("cold Git history ingestion was rejected".into())
        })?;
    let mut observations = BuildObservations::default();
    observations.record_duration(BuildPhase::Ingestion, elapsed_us(ingestion_started));
    observations.git_ingestion = Some(refresh.git.clone());
    observations.accepted_files = u64::try_from(history_plan.len()).unwrap_or(u64::MAX);
    observations.visited_files = observations.accepted_files;
    let artifacts = stage_git_history_plan(
        root,
        sources,
        history_plan,
        &refresh,
        false,
        semantic,
        None,
        reconciled_vectors,
        None,
        vector_checkpoint_path,
        started,
        observations,
        progress,
    )?;
    let reused_snapshot = artifacts
        .observations
        .vector_build
        .as_ref()
        .is_some_and(|vectors| vectors.reused_vectors > 0);
    Ok(IncrementalGitHistoryBuildSummary {
        artifacts,
        refresh,
        reused_snapshot,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn stage_git_history_plan(
    root: &Path,
    sources: &[GitCorpusSource],
    history_plan: GitHistoryBuildPlan,
    refresh: &GitHistoryRefreshObservations,
    reused_snapshot: bool,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<VectorArtifact>,
    reconciled_vectors: Option<ReconciledVectorStage>,
    incremental: Option<&IncrementalStage>,
    vector_checkpoint_path: Option<&Path>,
    started: Instant,
    mut observations: BuildObservations,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<BuildSummary, LifecycleError> {
    progress(BuildPhase::Lexical);
    let stage = persist_git_history_lexical_stage(
        root,
        sources,
        history_plan,
        incremental,
        refresh,
        reused_snapshot,
        &mut observations,
    )?;
    finish_git_history_lexical_stage(
        root,
        sources,
        &stage,
        semantic,
        previous_vectors,
        reconciled_vectors,
        vector_checkpoint_path,
        started,
        observations,
        progress,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_git_history_lexical_stage(
    root: &Path,
    sources: &[GitCorpusSource],
    history_plan: GitHistoryBuildPlan,
    incremental: Option<&IncrementalStage>,
    refresh: &GitHistoryRefreshObservations,
    reused_snapshot: bool,
    observations: &mut BuildObservations,
) -> Result<GitHistoryLexicalStage, LifecycleError> {
    if let Some(stage) = load_git_history_lexical_stage(root, sources)? {
        observations.reused_lexical_stage = true;
        observations.chunks = stage.marker.corpus.chunk_count();
        observations.documents = stage.marker.corpus.document_count();
        return Ok(stage);
    }
    let embedded = embedded_catalog_chunks()?;
    let history_chunks = history_plan.len();
    observations.chunks = history_chunks.saturating_add(embedded.len());
    observations.visited_files = observations.visited_files.max(observations.accepted_files);
    fs::create_dir_all(root)?;
    let staging = tempfile::Builder::new()
        .prefix(".tmp-lexical-stage-")
        .tempdir_in(root)?;
    let temp_root = staging.path();
    let lexical_path = temp_root.join("lexical.json");
    let encoding_started = Instant::now();
    let (lexical_checksum, lexical_bytes) = if let Some(previous) = incremental {
        LexicalIndex::write_incremental_git_history_search_artifact_with_digest(
            &previous.lexical_path,
            &history_plan,
            sources,
            &lexical_path,
        )?
    } else {
        LexicalIndex::write_git_history_search_artifact_with_digest(
            &history_plan,
            sources,
            &embedded,
            &lexical_path,
        )?
    };
    let lexical_sidecar_checksum = LexicalIndex::sidecar_checksum(&lexical_path)?;
    observations.record(BuildPhase::Encoding, encoding_started);
    drop(history_plan);

    let corpus_started = Instant::now();
    let corpus_version = CorpusVersion::try_from("git-full-history-v1")
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let (documents, chunks, digest) = LexicalIndex::corpus_inventory(&lexical_path)?;
    let corpus = CorpusManifest::from_inventory(corpus_version, documents, chunks, digest);
    corpus
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    observations.chunks = corpus.chunk_count();
    observations.documents = corpus.document_count();
    observations.record(BuildPhase::Corpus, corpus_started);
    let marker = GitHistoryLexicalStageMarker {
        schema: GIT_HISTORY_LEXICAL_STAGE_SCHEMA,
        contract_digest: git_history_lexical_contract_digest(sources)?,
        lexical_checksum,
        lexical_sidecar_checksum,
        lexical_bytes,
        corpus,
        history_chunks,
        refresh: GitHistoryLexicalStageRefresh::from_refresh(refresh),
        reused_snapshot,
    };
    let marker_path = temp_root.join(GIT_HISTORY_LEXICAL_STAGE_MARKER);
    let marker_file = File::create(marker_path)?;
    serde_json::to_writer(&marker_file, &marker)?;
    marker_file.sync_all()?;
    let temp_root = staging.keep();
    let stage_root = git_history_lexical_stage_path(root);
    fs::rename(temp_root, &stage_root)?;
    File::open(root)?.sync_all()?;
    Ok(GitHistoryLexicalStage {
        root: stage_root,
        marker,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn finish_git_history_lexical_stage(
    root: &Path,
    sources: &[GitCorpusSource],
    stage: &GitHistoryLexicalStage,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<VectorArtifact>,
    reconciled_vectors: Option<ReconciledVectorStage>,
    vector_checkpoint_path: Option<&Path>,
    started: Instant,
    mut observations: BuildObservations,
    progress: &mut dyn FnMut(BuildPhase),
) -> Result<BuildSummary, LifecycleError> {
    let staging = tempfile::Builder::new().prefix(".tmp-").tempdir_in(root)?;
    let temp_root = staging.path();
    let lexical_path = temp_root.join("lexical.json");
    LexicalIndex::clone_search_artifact(&stage.lexical_path(), &lexical_path)?;
    let lexical_checksum = stage.marker.lexical_checksum.clone();
    let lexical_bytes = stage.marker.lexical_bytes;
    let corpus = stage.marker.corpus.clone();
    observations.chunks = corpus.chunk_count();
    observations.documents = corpus.document_count();

    let graph = LexicalIndex::open_git_search_artifact_with_sources(&lexical_path, sources)?;
    let (graph_id_set, graph_id_count) = graph.embedding_chunk_id_set()?;
    let graph = LexicalIndex::graph_from_sidecar(
        &lexical_path,
        corpus.content_digest.clone(),
        |mut chunk| {
            if !graph_id_set.contains(&chunk.chunk_id) {
                return None;
            }
            if chunk
                .next_chunk
                .as_ref()
                .is_some_and(|next| !graph_id_set.contains(next))
            {
                chunk.next_chunk = None;
            }
            Some(chunk)
        },
    )?
    .ok_or_else(|| {
        LifecycleError::Contract("lexical artifact is missing graph projection".into())
    })?;
    if graph.nodes.len() != graph_id_count {
        return Err(LifecycleError::Contract(
            "lexical artifact graph hydration is incomplete".into(),
        ));
    }
    let (graph_checksum, graph_node_count, graph_edge_count) =
        write_graph(&temp_root.join("graph.bin"), &graph)?;

    let (vector_checksum, vector_bytes) = if let Some((provider, model_id, model_revision)) =
        semantic
    {
        progress(BuildPhase::Inference);
        let vector_path = temp_root.join("vectors.bin");
        let index = LexicalIndex::open_git_search_artifact_with_sources(&lexical_path, sources)?;
        let embedding_inputs = LexicalIndex::read_embedding_inputs(&lexical_path)?;
        let embedding_records = embedding_inputs.as_deref().map(|inputs| {
            inputs
                .iter()
                .map(|input| (input.chunk_id().clone(), input))
                .collect::<BTreeMap<_, _>>()
        });
        let ids = index.embedding_chunk_ids()?;
        let mut hydrator = EmbeddingTextHydrator::default();
        let mut embedding_texts = |indices: &[usize]| {
            let requested = indices
                .iter()
                .map(|&index| ids.get(index).cloned().ok_or(EmbeddingError::InvalidHeader))
                .collect::<Result<Vec<_>, _>>()?;
            let result = if let Some(records) = embedding_records.as_ref() {
                index.embedding_texts_by_id_from_record_map(&requested, records, &mut hydrator)
            } else {
                index.embedding_texts_by_id(&requested, &mut hydrator)
            };
            result.map_err(|error| EmbeddingError::Provider(error.to_string()))
        };
        let semantic_started = Instant::now();
        let (checksum, bytes, count, dimension, vector_build) =
            if let Some(previous) = reconciled_vectors {
                VectorArtifact::write_provider_reconciling_ids_artifact_with_observations(
                    provider,
                    &ids,
                    &mut embedding_texts,
                    ReconciledVectorArtifact {
                        model_id,
                        model_revision,
                        corpus_digest: &corpus.content_digest,
                        previous_corpus_digest: &previous.corpus_digest,
                        previous_checksum: &previous.checksum,
                        previous_path: &previous.path,
                        path: &vector_path,
                        checkpoint_path: vector_checkpoint_path,
                    },
                )?
            } else {
                let temporary_checkpoint = temp_root.join("vectors.checkpoint");
                let checkpoint_path = vector_checkpoint_path.unwrap_or(&temporary_checkpoint);
                let result =
                VectorArtifact::write_provider_reusing_checkpoint_ids_artifact_with_observations(
                    provider,
                    &ids,
                    &mut embedding_texts,
                    model_id,
                    model_revision,
                    &corpus.content_digest,
                    previous_vectors,
                    checkpoint_path,
                    &vector_path,
                );
                if vector_checkpoint_path.is_none() && temporary_checkpoint.exists() {
                    fs::remove_file(&temporary_checkpoint)?;
                }
                result?
            };
        observations.embedding_git_blob_loads = hydrator.git_blob_loads();
        observations.embedding_input_bytes = vector_build.input_bytes;
        observations.record_duration(BuildPhase::EmbeddingInput, vector_build.embedding_input_us);
        observations.record_duration(BuildPhase::Inference, vector_build.provider_us);
        observations.record_duration(
            BuildPhase::VectorFinalization,
            vector_build.vector_finalization_us,
        );
        observations.vector_build = Some(vector_build);
        observations.vector_count = count;
        observations.vector_dimension = Some(dimension);
        observations.resolved_batch_size = Some(provider.embedding_batch_size().get());
        let writing_us = elapsed_us(semantic_started)
            .saturating_sub(
                observations
                    .phases_us
                    .get(&BuildPhase::EmbeddingInput)
                    .copied()
                    .unwrap_or(0),
            )
            .saturating_sub(
                observations
                    .phases_us
                    .get(&BuildPhase::Inference)
                    .copied()
                    .unwrap_or(0),
            )
            .saturating_sub(
                observations
                    .phases_us
                    .get(&BuildPhase::VectorFinalization)
                    .copied()
                    .unwrap_or(0),
            );
        observations.record_duration(BuildPhase::Writing, writing_us);
        (Some(checksum), Some(bytes))
    } else {
        (None, None)
    };

    progress(BuildPhase::Activation);
    publish_staged_generation(
        root,
        staging,
        lexical_checksum,
        lexical_bytes,
        vector_checksum,
        vector_bytes,
        Some(graph_checksum),
        Some(graph_node_count),
        Some(graph_edge_count),
        corpus,
        Vec::new(),
        Vec::new(),
        started,
        observations,
    )
}

/// Build an additive immutable Git-tree corpus with an optional embedding provider.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git ingestion, chunking, embedding, or artifact staging fails.
pub fn build_git_artifacts_with_provider(
    root: &Path,
    sources: &[GitCorpusSource],
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<&PreviousVectorArtifact>,
    vector_checkpoint_path: Option<&Path>,
) -> Result<BuildSummary, LifecycleError> {
    let started = Instant::now();
    let mut ingestion_us = 0_u64;
    let mut chunking_us = 0_u64;
    let mut chunks = embedded_catalog_chunks()?;
    let mut rejected = Vec::new();
    let mut inventory = Vec::new();
    let mut visited_files = 0_u64;
    let mut git_ingestion = GitIngestionObservations::default();
    let mut ordered_sources = sources.iter().collect::<Vec<_>>();
    ordered_sources.sort_by(|left, right| {
        left.repository_id
            .cmp(&right.repository_id)
            .then_with(|| left.revision.cmp(&right.revision))
    });
    for source in ordered_sources {
        let ingest_started = Instant::now();
        let report = ingest_git_commit(
            &source.repository_path,
            &source.repository_id,
            &source.revision,
            source.trust_tier,
            &source.license,
            &source.policy,
        )?;
        ingestion_us = ingestion_us.saturating_add(elapsed_us(ingest_started));
        visited_files =
            visited_files.saturating_add(u64::try_from(report.visited_files).unwrap_or(u64::MAX));
        if let Some(report_observations) = report.git_observations.as_ref() {
            git_ingestion.accumulate(report_observations);
        }
        let chunking_started = Instant::now();
        for document in report.documents {
            chunks.extend(chunk_git_document(&document)?);
        }
        chunking_us = chunking_us.saturating_add(elapsed_us(chunking_started));
        let remaining_samples = MAX_REJECTION_SAMPLES.saturating_sub(rejected.len());
        rejected.extend(report.rejected.into_iter().take(remaining_samples));
        inventory.extend(report.sources);
    }
    let mut observations = BuildObservations::default();
    observations.record_duration(BuildPhase::Ingestion, ingestion_us);
    observations.record_duration(BuildPhase::Chunking, chunking_us);
    observations.visited_files = visited_files;
    observations.inventory_count = inventory.len();
    observations.rejection_count =
        usize::try_from(git_ingestion.rejection_count).unwrap_or(usize::MAX);
    observations.rejected_files = git_ingestion.rejection_count;
    observations.accepted_files = inventory
        .iter()
        .filter(|source| source.rejection.is_none())
        .count() as u64;
    observations.accepted_source_bytes = inventory
        .iter()
        .filter(|source| source.rejection.is_none())
        .filter_map(|source| source.byte_count)
        .sum();
    observations.git_ingestion = Some(git_ingestion);
    let reconciled_vectors =
        previous_vectors.and_then(|previous| reusable_vector_stage(previous, semantic.as_ref()));
    let semantic = semantic.map(|(provider, model_id, model_revision)| SemanticBuild {
        provider,
        model_id,
        model_revision,
    });
    stage_chunks(
        root,
        &chunks,
        Some(sources),
        semantic,
        "git-tree-v1",
        rejected,
        inventory,
        started,
        observations,
        reconciled_vectors,
        vector_checkpoint_path,
    )
}

fn chunk_git_document(document: &NormalizedDocument) -> Result<Vec<Chunk>, LifecycleError> {
    let drafts = chunk_document_drafts(document, ChunkingConfig::default())
        .map_err(|error| LifecycleError::Contract(format!("{}: {error}", document.path)))?;
    (0..drafts.len())
        .map(|index| {
            let identifiers = identifier_values(&document.path, drafts.get(index).text());
            drafts
                .materialize(index, Some(identifiers))
                .and_then(Chunk::with_derived_resource_uri)
                .map_err(|error| LifecycleError::Contract(format!("{}: {error}", document.path)))
        })
        .collect()
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn stage_chunks(
    root: &Path,
    chunks: &[crate::Chunk],
    git_sources: Option<&[GitCorpusSource]>,
    semantic: Option<SemanticBuild<'_>>,
    corpus_version: &str,
    diagnostics: Vec<SourceRejection>,
    sources: Vec<SourceInventory>,
    started: Instant,
    mut observations: BuildObservations,
    reconciled_vectors: Option<ReconciledVectorStage>,
    vector_checkpoint_path: Option<&Path>,
) -> Result<BuildSummary, LifecycleError> {
    observations.chunks = chunks.len();
    observations.inventory_count = observations.inventory_count.max(sources.len());
    observations.rejection_count = observations.rejection_count.max(diagnostics.len());
    observations.visited_files = observations.visited_files.max(
        observations
            .accepted_files
            .saturating_add(observations.rejected_files),
    );
    fs::create_dir_all(root)?;
    let staging = tempfile::Builder::new().prefix(".tmp-").tempdir_in(root)?;
    let temp_root = staging.path();
    let lexical_path = temp_root.join("lexical.json");
    let encoding_started = Instant::now();
    let (lexical_checksum, lexical_bytes) = git_sources.map_or_else(
        || LexicalIndex::write_search_artifact_with_digest(chunks.iter(), &lexical_path),
        |sources| {
            LexicalIndex::write_git_search_artifact_with_digest(
                chunks.iter(),
                sources,
                &lexical_path,
            )
        },
    )?;
    observations.record(BuildPhase::Encoding, encoding_started);
    let corpus_started = Instant::now();
    let corpus_version = CorpusVersion::try_from(corpus_version)
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let corpus = if corpus_version.as_ref() == "git-full-history-v1" {
        let (documents, chunks, digest) = LexicalIndex::corpus_inventory(&lexical_path)?;
        CorpusManifest::from_inventory(corpus_version, documents, chunks, digest)
    } else {
        CorpusManifest::new(
            corpus_version,
            chunks
                .iter()
                .map(|chunk| chunk.document_id.clone())
                .collect(),
            chunks.iter().map(|chunk| chunk.chunk_id.clone()).collect(),
        )
    };
    corpus
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    observations.chunks = corpus.chunk_count();
    observations.documents = corpus.document_count();
    observations.record(BuildPhase::Corpus, corpus_started);
    let (graph_checksum, graph_node_count, graph_edge_count) =
        write_graph_artifact(&temp_root.join("graph.bin"), &corpus.content_digest, chunks)?;
    let (vector_checksum, vector_bytes) = if let Some(semantic) = semantic {
        let vector_path = temp_root.join("vectors.bin");
        let (checksum, bytes, count, dimension, vector_build, git_blob_loads) = if let Some(
            previous,
        ) =
            reconciled_vectors
        {
            let sources = git_sources.ok_or_else(|| {
                LifecycleError::Contract(
                    "vector reconciliation requires immutable Git sources".into(),
                )
            })?;
            let index =
                LexicalIndex::open_git_search_artifact_with_sources(&lexical_path, sources)?;
            let embedding_inputs = LexicalIndex::read_embedding_inputs(&lexical_path)?;
            let embedding_records = embedding_inputs.as_deref().map(|inputs| {
                inputs
                    .iter()
                    .map(|input| (input.chunk_id().clone(), input))
                    .collect::<BTreeMap<_, _>>()
            });
            let ids = index.embedding_chunk_ids()?;
            let history_keys = chunks
                .iter()
                .filter_map(|chunk| {
                    crate::corpus::history_content_key_for_chunk(chunk)
                        .map(|key| (chunk.chunk_id.clone(), key))
                })
                .collect::<BTreeMap<_, _>>();
            let wanted_keys = history_keys.values().cloned().collect::<BTreeSet<_>>();
            let matching_ids = LexicalIndex::open_history_content_lookup(&previous.lexical_path)?
                .matching_chunk_ids(&wanted_keys)?;
            let previous_ids = ids
                .iter()
                .map(|id| {
                    history_keys
                        .get(id)
                        .and_then(|key| matching_ids.get(key))
                        .unwrap_or(id)
                        .clone()
                })
                .collect::<Vec<_>>();
            let mut hydrator = EmbeddingTextHydrator::default();
            let mut embedding_texts = |indices: &[usize]| {
                let requested = indices
                    .iter()
                    .map(|&index| ids.get(index).cloned().ok_or(EmbeddingError::InvalidHeader))
                    .collect::<Result<Vec<_>, _>>()?;
                let result = if let Some(records) = embedding_records.as_ref() {
                    index.embedding_texts_by_id_from_record_map(&requested, records, &mut hydrator)
                } else {
                    index.embedding_texts_by_id(&requested, &mut hydrator)
                };
                result.map_err(|error| EmbeddingError::Provider(error.to_string()))
            };
            let temporary_checkpoint = temp_root.join("vectors.checkpoint");
            let checkpoint_path = vector_checkpoint_path.unwrap_or(temporary_checkpoint.as_path());
            let result = if previous_ids == ids {
                VectorArtifact::write_provider_reconciling_ids_artifact_with_observations(
                    semantic.provider,
                    &ids,
                    &mut embedding_texts,
                    ReconciledVectorArtifact {
                        model_id: semantic.model_id,
                        model_revision: semantic.model_revision,
                        corpus_digest: &corpus.content_digest,
                        previous_corpus_digest: &previous.corpus_digest,
                        previous_checksum: &previous.checksum,
                        previous_path: &previous.path,
                        path: &vector_path,
                        checkpoint_path: Some(checkpoint_path),
                    },
                )
            } else {
                VectorArtifact::write_provider_reconciling_aliased_ids_artifact_with_observations(
                    semantic.provider,
                    &ids,
                    &previous_ids,
                    &mut embedding_texts,
                    ReconciledVectorArtifact {
                        model_id: semantic.model_id,
                        model_revision: semantic.model_revision,
                        corpus_digest: &corpus.content_digest,
                        previous_corpus_digest: &previous.corpus_digest,
                        previous_checksum: &previous.checksum,
                        previous_path: &previous.path,
                        path: &vector_path,
                        checkpoint_path: Some(checkpoint_path),
                    },
                )
            };
            if vector_checkpoint_path.is_none() && temporary_checkpoint.exists() {
                fs::remove_file(&temporary_checkpoint)?;
            }
            let (checksum, bytes, count, dimension, vector_build) = result?;
            (
                checksum,
                bytes,
                count,
                dimension,
                vector_build,
                hydrator.git_blob_loads(),
            )
        } else {
            let (vector, vector_build) = VectorArtifact::from_provider_with_observations(
                semantic.provider,
                chunks,
                semantic.model_id,
                semantic.model_revision,
                corpus.content_digest.clone(),
            )?;
            let count = vector.ids.len();
            let dimension = vector.dimension;
            let write_started = Instant::now();
            let (checksum, bytes) = vector.write_artifact_with_digest(&vector_path)?;
            observations.record(BuildPhase::Writing, write_started);
            (checksum, bytes, count, dimension, vector_build, 0)
        };
        observations.embedding_git_blob_loads = git_blob_loads;
        observations.embedding_input_bytes = vector_build.input_bytes;
        observations.record_duration(BuildPhase::EmbeddingInput, vector_build.embedding_input_us);
        observations.record_duration(BuildPhase::Inference, vector_build.provider_us);
        observations.record_duration(
            BuildPhase::VectorFinalization,
            vector_build.vector_finalization_us,
        );
        observations.vector_count = count;
        observations.vector_dimension = Some(dimension);
        observations.resolved_batch_size = Some(semantic.provider.embedding_batch_size().get());
        observations.vector_build = Some(vector_build);
        (Some(checksum), Some(bytes))
    } else {
        (None, None)
    };
    publish_staged_generation(
        root,
        staging,
        lexical_checksum,
        lexical_bytes,
        vector_checksum,
        vector_bytes,
        Some(graph_checksum),
        Some(graph_node_count),
        Some(graph_edge_count),
        corpus,
        diagnostics,
        sources,
        started,
        observations,
    )
}

fn write_graph_artifact(
    path: &Path,
    corpus_digest: &ContentDigest,
    chunks: &[Chunk],
) -> Result<(ContentDigest, u64, u64), LifecycleError> {
    let graph = crate::GraphArtifact::from_chunks(corpus_digest.clone(), chunks)
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    write_graph(path, &graph)
}

fn write_graph(
    path: &Path,
    graph: &crate::GraphArtifact,
) -> Result<(ContentDigest, u64, u64), LifecycleError> {
    let checksum = graph
        .write(path)
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let node_count = u64::try_from(graph.nodes.len()).unwrap_or(u64::MAX);
    let edge_count = u64::try_from(graph.edges.len()).unwrap_or(u64::MAX);
    Ok((checksum, node_count, edge_count))
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::too_many_lines
)] // Owning TempDir keeps cleanup active until atomic publication completes.
fn publish_staged_generation(
    root: &Path,
    staging: tempfile::TempDir,
    lexical_checksum: ContentDigest,
    lexical_bytes: u64,
    vector_checksum: Option<ContentDigest>,
    vector_bytes: Option<u64>,
    graph_checksum: Option<ContentDigest>,
    graph_node_count: Option<u64>,
    graph_edge_count: Option<u64>,
    corpus: CorpusManifest,
    diagnostics: Vec<SourceRejection>,
    sources: Vec<SourceInventory>,
    started: Instant,
    mut observations: BuildObservations,
) -> Result<BuildSummary, LifecycleError> {
    if vector_checksum.is_some() && observations.vector_count != corpus.chunk_count() {
        return Err(LifecycleError::Contract(format!(
            "vector count {} does not match corpus chunk count {}",
            observations.vector_count,
            corpus.chunk_count()
        )));
    }
    let temp_root = staging.path();
    let graph_bytes = graph_checksum
        .as_ref()
        .map(|_| fs::metadata(temp_root.join("graph.bin")).map(|metadata| metadata.len()))
        .transpose()?;
    let manifest = ArtifactManifest {
        schema: crate::corpus::ARTIFACT_SCHEMA_V1,
        corpus,
        chunking: ChunkingConfig::default(),
        component_versions: artifact_component_versions(),
        sources,
        lexical_checksum: Some(lexical_checksum),
        vector_checksum,
        graph_checksum,
        graph_node_count,
        graph_edge_count,
        tool_version: env!("CARGO_PKG_VERSION").into(),
        diagnostics,
    };
    manifest
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let manifest_started = Instant::now();
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    observations.manifest_bytes = u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX);
    observations.active_manifest_bytes = observations.manifest_bytes;
    let manifest_byte_count = u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX);
    fs::write(temp_root.join("manifest.json"), &manifest_bytes)?;
    observations.record(BuildPhase::Manifest, manifest_started);
    let validation_started = Instant::now();
    validate_written_generation(
        temp_root,
        &manifest,
        lexical_bytes,
        vector_bytes,
        graph_bytes,
        manifest_byte_count,
    )?;
    observations.record(BuildPhase::Validation, validation_started);

    let generation_root = root.join("generations");
    fs::create_dir_all(&generation_root)?;
    let base_generation = ContentDigest::of(&manifest_bytes).to_string();
    let generation = (0..=32)
        .find_map(|attempt| {
            let generation = if attempt == 0 {
                base_generation.clone()
            } else {
                ContentDigest::of(format!("{base_generation}:repair:{attempt}").as_bytes())
                    .to_string()
            };
            let candidate = generation_root.join(&generation);
            if candidate.exists() {
                validate_generation(&candidate, &manifest)
                    .is_ok()
                    .then_some(Ok(generation))
            } else {
                Some(fs::rename(temp_root, candidate).map(|()| generation))
            }
        })
        .transpose()?
        .ok_or_else(|| {
            LifecycleError::Contract(
                "could not publish a valid artifact generation after 32 repairs".into(),
            )
        })?;
    let activation_started = Instant::now();
    let active_pointer = ActiveManifestPointer::new(&generation, &manifest_bytes)?;
    let active_bytes = serde_json::to_vec(&active_pointer)?;
    let active_tmp = root.join(format!(".active.tmp-{}", std::process::id()));
    fs::write(&active_tmp, &active_bytes)?;
    fs::rename(active_tmp, root.join("active.json"))?;
    observations.record(BuildPhase::Activation, activation_started);
    observations.active_manifest_bytes = u64::try_from(active_bytes.len()).unwrap_or(u64::MAX);
    observations.artifact_bytes =
        lexical_bytes + vector_bytes.unwrap_or(0) + graph_bytes.unwrap_or(0);
    observations.total_duration_us = elapsed_us(started);

    Ok(BuildSummary {
        generation,
        document_count: manifest.corpus.document_count(),
        chunk_count: manifest.corpus.chunk_count(),
        lexical_bytes,
        vector_bytes,
        graph_bytes,
        build_duration_us: observations.total_duration_us,
        observations,
        manifest,
    })
}

fn git_history_lexical_component_versions() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("chunking".into(), "markdown-structural-v1".into()),
        ("corpus-schema".into(), "1.1".into()),
        (
            "embedded-catalog".into(),
            ContentDigest::of(include_bytes!("../generated/knowledge_index.json")).to_string(),
        ),
        ("git-history-corpus".into(), "2".into()),
        (
            "git-policy".into(),
            crate::corpus::git::GIT_CORPUS_POLICY_VERSION.into(),
        ),
        (
            "lexical-format".into(),
            crate::lexical::LEXICAL_FORMAT_VERSION.into(),
        ),
        ("markdown-parser".into(), "pulldown-cmark-0.13".into()),
    ])
}

/// Whether two artifact version maps describe the same reusable Git-history corpus.
///
/// Lexical and vector encodings are deliberately excluded: they can be rebuilt
/// from the persisted corpus without walking Git history again.
#[must_use]
pub fn git_history_corpus_versions_are_compatible(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> bool {
    const CORPUS_COMPONENTS: [&str; 6] = [
        "chunking",
        "corpus-schema",
        "embedded-catalog",
        "git-history-corpus",
        "git-policy",
        "markdown-parser",
    ];
    CORPUS_COMPONENTS.iter().all(|name| {
        previous
            .get(*name)
            .is_some_and(|value| current.get(*name) == Some(value))
    })
}

/// Version inputs which affect persisted artifact compatibility and identity.
#[must_use]
pub fn artifact_component_versions() -> BTreeMap<String, String> {
    let mut versions = git_history_lexical_component_versions();
    versions.extend([
        (
            "vesc-knowledge-index".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
        ("vector-format".into(), "dense-cosine-v2".into()),
        ("graph-format".into(), "adjacency-csr-v1".into()),
    ]);
    versions
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn validate_generation(root: &Path, expected: &ArtifactManifest) -> Result<(), LifecycleError> {
    let manifest: ArtifactManifest =
        serde_json::from_slice(&fs::read(root.join("manifest.json"))?)?;
    if &manifest != expected {
        return Err(LifecycleError::Contract(
            "generation manifest does not match requested corpus".into(),
        ));
    }
    if let Some(checksum) = &manifest.lexical_checksum {
        let lexical_path = root.join("lexical.json");
        let actual = ContentDigest::try_from(format!(
            "sha256:{}",
            crate::hardware::sha256_file(&lexical_path)?
        ))
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
        if actual != *checksum {
            return Err(LifecycleError::Contract(
                "lexical artifact checksum mismatch".into(),
            ));
        }
        validate_lexical_inventory(&lexical_path, &manifest.corpus)?;
    }
    if let Some(checksum) = &manifest.vector_checksum {
        let vector_path = root.join("vectors.bin");
        VectorArtifact::validate_artifact(
            &vector_path,
            checksum,
            &manifest.corpus.content_digest,
            manifest.corpus.chunk_count(),
        )?;
    }
    if let Some(checksum) = &manifest.graph_checksum {
        let graph = crate::GraphArtifact::validate_path(
            &root.join("graph.bin"),
            checksum,
            &manifest.corpus.content_digest,
        )
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
        validate_graph_counts(manifest.graph_node_count, manifest.graph_edge_count, graph)?;
    }
    Ok(())
}

fn validate_written_generation(
    root: &Path,
    expected: &ArtifactManifest,
    lexical_bytes: u64,
    vector_bytes: Option<u64>,
    graph_bytes: Option<u64>,
    manifest_bytes: u64,
) -> Result<(), LifecycleError> {
    expected
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let mut expected_files = vec![
        ("manifest.json", manifest_bytes),
        ("lexical.json", lexical_bytes),
    ];
    if let Some(vector_bytes) = vector_bytes {
        expected_files.push(("vectors.bin", vector_bytes));
    }
    if let Some(graph_bytes) = graph_bytes {
        expected_files.push(("graph.bin", graph_bytes));
    }
    for (name, expected_bytes) in expected_files {
        let path = root.join(name);
        let actual_bytes = fs::metadata(&path)?.len();
        if actual_bytes != expected_bytes {
            return Err(LifecycleError::Contract(format!(
                "fresh artifact {name} has {actual_bytes} bytes, expected {expected_bytes}"
            )));
        }
    }
    validate_lexical_inventory(&root.join("lexical.json"), &expected.corpus)?;
    if let Some(graph_checksum) = &expected.graph_checksum {
        let graph = crate::GraphArtifact::validate_path(
            &root.join("graph.bin"),
            graph_checksum,
            &expected.corpus.content_digest,
        )
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
        validate_graph_counts(expected.graph_node_count, expected.graph_edge_count, graph)?;
    }
    Ok(())
}

fn validate_lexical_inventory(
    lexical_path: &Path,
    corpus: &CorpusManifest,
) -> Result<(), LifecycleError> {
    LexicalIndex::open_search_artifact(lexical_path)?;
    if corpus.corpus_version.as_ref() == "git-full-history-v1" {
        let (documents, chunks, digest) = LexicalIndex::corpus_inventory(lexical_path)?;
        if documents != corpus.document_count()
            || chunks != corpus.chunk_count()
            || digest != corpus.content_digest
        {
            return Err(LifecycleError::Contract(
                "lexical sidecar does not match the corpus inventory".into(),
            ));
        }
    }
    Ok(())
}

/// Read and validate an artifact manifest without activating it.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the file is absent, malformed, or invalid.
pub fn inspect_manifest(path: &Path) -> Result<ArtifactManifest, LifecycleError> {
    let (_, bytes) = read_selected_manifest(path)?;
    let manifest: ArtifactManifest = serde_json::from_slice(&bytes)?;
    manifest
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    Ok(manifest)
}

/// Read only the bounded metadata needed to reuse a predecessor artifact.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the active pointer or manifest is invalid.
pub fn inspect_previous_artifact(path: &Path) -> Result<PreviousArtifactSummary, LifecycleError> {
    let (generation, bytes) = read_selected_manifest(path)?;
    previous_summary(serde_json::from_slice(&bytes)?, generation)
}

fn previous_summary(
    manifest: PreviousArtifactProjection,
    generation: ContentDigest,
) -> Result<PreviousArtifactSummary, LifecycleError> {
    manifest
        .schema
        .ensure_major(ARTIFACT_SCHEMA_V1, "artifact")
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    manifest
        .corpus
        .schema
        .ensure_major(crate::corpus::CORPUS_SCHEMA_V1, "corpus")
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    Ok(PreviousArtifactSummary {
        generation,
        corpus_version: manifest.corpus.corpus_version,
        corpus_digest: manifest.corpus.content_digest,
        document_count: manifest
            .corpus
            .document_count
            .max(manifest.corpus.documents.len()),
        chunk_count: manifest
            .corpus
            .chunk_count
            .max(manifest.corpus.chunks.len()),
        source_count: manifest.sources.len(),
        diagnostic_count: manifest.diagnostics.len(),
        component_versions: manifest.component_versions,
        lexical_checksum: manifest.lexical_checksum,
        vector_checksum: manifest.vector_checksum,
        graph_checksum: manifest.graph_checksum,
        graph_node_count: manifest.graph_node_count,
        graph_edge_count: manifest.graph_edge_count,
    })
}

/// Return the conventional active manifest selector path for an artifact root.
///
/// The file contains a checksum-verified pointer to an immutable generation.
#[must_use]
pub fn active_manifest_path(root: &Path) -> PathBuf {
    root.join("active.json")
}

/// Resolve the immutable generation selected by an artifact root.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the active selector is missing or malformed.
pub fn active_generation_path(root: &Path) -> Result<PathBuf, LifecycleError> {
    let path = active_manifest_path(root);
    let pointer: ActiveManifestPointer =
        serde_json::from_reader(BufReader::new(File::open(path)?))?;
    pointer.validate()?;
    Ok(root
        .join("generations")
        .join(pointer.generation.to_string()))
}

/// Load the graph selected by the active immutable generation.
///
/// A graph-free legacy generation returns `Ok(None)`; a declared graph is
/// fully decoded and checked against its manifest before it is returned.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the active generation or declared graph is
/// missing, corrupt, incompatible, or inconsistent with its manifest.
pub fn load_active_graph(root: &Path) -> Result<Option<crate::GraphArtifact>, LifecycleError> {
    let artifact = inspect_previous_artifact(&active_manifest_path(root))?;
    let Some(expected_checksum) = artifact.graph_checksum else {
        return Ok(None);
    };
    let generation_root = active_generation_path(root)?;
    let graph = crate::GraphArtifact::open(&generation_root.join("graph.bin"))?;
    let encoded = graph.encode()?;
    if graph.corpus_digest != artifact.corpus_digest {
        return Err(
            crate::GraphArtifactError::Contract("graph artifact corpus mismatch".into()).into(),
        );
    }
    if ContentDigest::of(&encoded) != expected_checksum {
        return Err(
            crate::GraphArtifactError::Contract("graph artifact checksum mismatch".into()).into(),
        );
    }
    validate_graph_counts(
        artifact.graph_node_count,
        artifact.graph_edge_count,
        crate::GraphArtifactSummary {
            bytes: u64::try_from(encoded.len()).unwrap_or(u64::MAX),
            node_count: u64::try_from(graph.nodes.len()).unwrap_or(u64::MAX),
            edge_count: u64::try_from(graph.edges.len()).unwrap_or(u64::MAX),
        },
    )?;
    Ok(Some(graph))
}

fn read_selected_manifest(pointer_path: &Path) -> Result<(ContentDigest, Vec<u8>), LifecycleError> {
    let pointer: ActiveManifestPointer =
        serde_json::from_reader(BufReader::new(File::open(pointer_path)?))?;
    pointer.validate()?;
    let root = pointer_path
        .parent()
        .ok_or_else(|| LifecycleError::Contract("active manifest has no root".into()))?;
    let manifest_path = root
        .join("generations")
        .join(pointer.generation.to_string())
        .join("manifest.json");
    let bytes = fs::read(manifest_path)?;
    if ContentDigest::of(&bytes) != pointer.manifest_checksum {
        return Err(LifecycleError::Contract(
            "active manifest checksum mismatch".into(),
        ));
    }
    Ok((pointer.generation, bytes))
}

/// Validate the complete immutable generation selected by an artifact root.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the selector, manifest, lexical index, or
/// vector artifact is absent, corrupt, or inconsistent.
pub fn validate_active_generation(root: &Path) -> Result<PreviousArtifactSummary, LifecycleError> {
    let artifact = inspect_previous_artifact(&active_manifest_path(root))?;
    let generation_root = root
        .join("generations")
        .join(artifact.generation.to_string());
    let lexical_path = generation_root.join("lexical.json");
    let expected_lexical = artifact.lexical_checksum.as_ref().ok_or_else(|| {
        LifecycleError::Contract("managed artifact has no lexical checksum".into())
    })?;
    let actual_lexical = ContentDigest::try_from(format!(
        "sha256:{}",
        crate::hardware::sha256_file(&lexical_path)?
    ))
    .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    if &actual_lexical != expected_lexical {
        return Err(LifecycleError::Contract(
            "lexical artifact checksum mismatch".into(),
        ));
    }
    let (_, chunk_count, corpus_digest) = LexicalIndex::corpus_inventory(&lexical_path)?;
    if corpus_digest != artifact.corpus_digest {
        return Err(LifecycleError::Contract(
            "lexical sidecar does not match the corpus inventory".into(),
        ));
    }
    if let Some(vector_checksum) = &artifact.vector_checksum {
        VectorArtifact::validate_artifact(
            &generation_root.join("vectors.bin"),
            vector_checksum,
            &artifact.corpus_digest,
            chunk_count,
        )?;
    }
    if let Some(graph_checksum) = &artifact.graph_checksum {
        let graph = crate::GraphArtifact::validate_path(
            &generation_root.join("graph.bin"),
            graph_checksum,
            &artifact.corpus_digest,
        )
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
        validate_graph_counts(artifact.graph_node_count, artifact.graph_edge_count, graph)?;
    }
    Ok(artifact)
}

fn validate_graph_counts(
    node_count: Option<u64>,
    edge_count: Option<u64>,
    graph: crate::GraphArtifactSummary,
) -> Result<(), LifecycleError> {
    if node_count != Some(graph.node_count) || edge_count != Some(graph.edge_count) {
        return Err(LifecycleError::Contract(
            "graph artifact counts do not match manifest".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_stage_contract_includes_git_corpus_limits() {
        let mut source = GitCorpusSource {
            repository_path: PathBuf::from("fixture.git"),
            repository_id: RepositoryId::try_from("fixture").expect("repository"),
            revision: Revision::try_from("0".repeat(40)).expect("revision"),
            history_tips: vec![Revision::try_from("0".repeat(40)).expect("history tip")],
            trust_tier: crate::TrustTier::CuratedUpstream,
            license: crate::LicenseStatus::ReferenceOnly,
            policy: crate::corpus::git::GitCorpusPolicy::default(),
        };
        source.policy.limits =
            crate::corpus::git::GitCorpusLimits::new(1_024, 10, 4_096).expect("limits");
        let baseline = git_history_lexical_contract_digest(std::slice::from_ref(&source))
            .expect("baseline contract");

        for limits in [
            crate::corpus::git::GitCorpusLimits::new(2_048, 10, 4_096).expect("file limit"),
            crate::corpus::git::GitCorpusLimits::new(1_024, 20, 4_096).expect("file count"),
            crate::corpus::git::GitCorpusLimits::new(1_024, 10, 8_192).expect("total bytes"),
        ] {
            source.policy.limits = limits;
            assert_ne!(
                baseline,
                git_history_lexical_contract_digest(std::slice::from_ref(&source))
                    .expect("changed contract")
            );
        }
    }

    #[test]
    fn lexical_stage_contract_includes_every_history_tip() {
        let mut source = GitCorpusSource {
            repository_path: PathBuf::from("fixture.git"),
            repository_id: RepositoryId::try_from("fixture").expect("repository"),
            revision: Revision::try_from("0".repeat(40)).expect("revision"),
            history_tips: vec![Revision::try_from("0".repeat(40)).expect("history tip")],
            trust_tier: crate::TrustTier::CuratedUpstream,
            license: crate::LicenseStatus::ReferenceOnly,
            policy: crate::corpus::git::GitCorpusPolicy::default(),
        };
        let baseline = git_history_lexical_contract_digest(std::slice::from_ref(&source))
            .expect("baseline contract");

        source
            .history_tips
            .push(Revision::try_from("1".repeat(40)).expect("second history tip"));

        assert_ne!(
            baseline,
            git_history_lexical_contract_digest(&[source]).expect("changed contract")
        );
    }

    #[test]
    fn lexical_contract_versions_exclude_vector_only_state() {
        let versions = git_history_lexical_component_versions();

        assert!(!versions.contains_key("vector-format"));
        assert!(!versions.contains_key("vesc-knowledge-index"));
        assert!(versions.contains_key("chunking"));
        assert!(versions.contains_key("embedded-catalog"));
    }

    #[test]
    fn staged_build_and_inspect_are_portable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = build_embedded_artifacts(temp.path()).expect("build");
        let manifest = inspect_manifest(&active_manifest_path(temp.path())).expect("inspect");
        assert_eq!(manifest, summary.manifest);
        assert!(summary.document_count > 0);
        assert!(summary.chunk_count > 0);
        assert!(summary.build_duration_us > 0);
        assert!(
            !temp
                .path()
                .join("generations")
                .join(&summary.generation)
                .join("corpus.json")
                .exists()
        );
        assert!(summary.observations.manifest_bytes > 0);
        assert!(summary.observations.active_manifest_bytes > 0);
        assert!(summary.observations.active_manifest_bytes < summary.observations.manifest_bytes);
        assert_eq!(
            summary.observations.provenance_bytes(),
            summary.observations.manifest_bytes + summary.observations.active_manifest_bytes
        );
        assert!(summary.observations.provenance_overhead_percent().is_some());
        assert_eq!(
            summary.observations.provenance_overhead_is_material(),
            summary
                .observations
                .provenance_overhead_percent()
                .is_some_and(|percent| percent >= PROVENANCE_OVERHEAD_THRESHOLD_PERCENT)
        );
        assert_eq!(
            summary.observations.total_duration_us,
            summary.build_duration_us
        );
        assert!(
            summary
                .observations
                .phases_us
                .contains_key(&BuildPhase::Ingestion)
        );
        assert!(
            summary
                .observations
                .phases_us
                .contains_key(&BuildPhase::Activation)
        );
        assert!(!summary.manifest.component_versions.is_empty());
        assert!(summary.vector_bytes.is_none());
        assert!(summary.graph_bytes.is_some_and(|bytes| bytes > 0));
        assert!(summary.manifest.graph_checksum.is_some());
        assert!(summary.manifest.graph_node_count.unwrap_or(0) > 0);
        assert!(summary.manifest.graph_edge_count.is_some());
        validate_active_generation(temp.path()).expect("graph-backed generation validates");
        let text = fs::read_to_string(active_manifest_path(temp.path())).expect("manifest");
        assert!(!text.contains(temp.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn active_pointer_is_deterministic_and_checksums_generation_manifest() {
        let first_root = tempfile::tempdir().expect("first artifact root");
        let second_root = tempfile::tempdir().expect("second artifact root");
        let first = build_embedded_artifacts(first_root.path()).expect("first build");
        let second = build_embedded_artifacts(second_root.path()).expect("second build");
        let first_bytes = fs::read(active_manifest_path(first_root.path())).expect("first active");
        let second_bytes =
            fs::read(active_manifest_path(second_root.path())).expect("second active");
        assert_eq!(first_bytes, second_bytes);
        assert!(first_bytes.len() <= 256);

        let pointer: ActiveManifestPointer =
            serde_json::from_slice(&first_bytes).expect("active pointer");
        assert_eq!(pointer.generation.to_string(), first.generation);
        let generation_manifest = first_root
            .path()
            .join("generations")
            .join(&first.generation)
            .join("manifest.json");
        assert_eq!(
            pointer.manifest_checksum,
            ContentDigest::of(&fs::read(generation_manifest).expect("generation manifest"))
        );
        let previous = inspect_previous_artifact(&active_manifest_path(first_root.path()))
            .expect("bounded predecessor metadata");
        assert_eq!(previous.corpus_digest, first.manifest.corpus.content_digest);
        assert_eq!(previous.vector_checksum, first.manifest.vector_checksum);
        assert_eq!(first.generation, second.generation);
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(first.lexical_bytes, second.lexical_bytes);
    }

    #[test]
    fn active_selector_apis_reject_a_full_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = build_embedded_artifacts(temp.path()).expect("build");
        let active_path = active_manifest_path(temp.path());
        fs::write(
            &active_path,
            serde_json::to_vec(&summary.manifest).expect("full manifest"),
        )
        .expect("write full manifest");

        inspect_manifest(&active_path).expect_err("manifest inspection requires a pointer");
        inspect_previous_artifact(&active_path)
            .expect_err("previous-artifact inspection requires a pointer");
        active_generation_path(temp.path()).expect_err("generation lookup requires a pointer");
    }

    #[test]
    fn inspect_manifest_rejects_corrupt_active_pointer_checksum() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = build_embedded_artifacts(temp.path()).expect("build");
        let active_path = active_manifest_path(temp.path());
        let mut pointer: ActiveManifestPointer =
            serde_json::from_slice(&fs::read(&active_path).expect("active pointer"))
                .expect("pointer");
        pointer.manifest_checksum = ContentDigest::of(b"corrupt");
        fs::write(
            &active_path,
            serde_json::to_vec(&pointer).expect("corrupt pointer"),
        )
        .expect("write corrupt pointer");

        let error = inspect_manifest(&active_path).expect_err("corrupt pointer rejected");
        assert!(error.to_string().contains("checksum"));
        assert!(summary.observations.active_manifest_bytes > 0);
    }

    #[test]
    fn corrupt_existing_generation_is_rebuilt_at_a_new_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = build_embedded_artifacts(temp.path()).expect("initial build");
        let lexical = temp
            .path()
            .join("generations")
            .join(&first.generation)
            .join("lexical.json");
        fs::write(&lexical, b"corrupt").expect("corrupt generation");

        let rebuilt = build_embedded_artifacts(temp.path()).expect("rebuild corrupt generation");
        let active = inspect_manifest(&active_manifest_path(temp.path())).expect("active");
        assert_eq!(active, rebuilt.manifest);
        assert_ne!(rebuilt.generation, first.generation);
        assert_eq!(
            fs::read(&lexical).expect("preserve corrupt evidence"),
            b"corrupt"
        );
    }

    #[test]
    fn adding_vectors_activates_a_new_generation() {
        let temp = tempfile::tempdir().expect("artifact root");
        let lexical = build_embedded_artifacts(temp.path()).expect("lexical build");
        let mut provider = crate::FakeEmbeddingProvider::new(4);

        let semantic = build_embedded_artifacts_with_provider(
            temp.path(),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("semantic upgrade");

        assert_ne!(semantic.generation, lexical.generation);
        assert!(
            temp.path()
                .join("generations")
                .join(&lexical.generation)
                .is_dir()
        );
        assert!(
            temp.path()
                .join("generations")
                .join(&semantic.generation)
                .join("vectors.bin")
                .is_file()
        );
    }

    #[test]
    fn missing_vector_in_an_existing_generation_is_rebuilt_at_a_new_path() {
        let temp = tempfile::tempdir().expect("artifact root");
        let mut provider = crate::FakeEmbeddingProvider::new(4);
        let first = build_embedded_artifacts_with_provider(
            temp.path(),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("semantic build");
        let vector = temp
            .path()
            .join("generations")
            .join(&first.generation)
            .join("vectors.bin");
        fs::remove_file(&vector).expect("remove vector");

        let repaired = build_embedded_artifacts_with_provider(
            temp.path(),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("repair vector");

        assert_ne!(repaired.generation, first.generation);
        assert!(!vector.exists());
        assert!(
            temp.path()
                .join("generations")
                .join(repaired.generation)
                .join("vectors.bin")
                .is_file()
        );
    }

    #[test]
    fn active_generation_validation_rejects_a_truncated_vector() {
        let temp = tempfile::tempdir().expect("artifact root");
        let mut provider = crate::FakeEmbeddingProvider::new(4);
        let built = build_embedded_artifacts_with_provider(
            temp.path(),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("semantic build");
        let vector = temp
            .path()
            .join("generations")
            .join(built.generation)
            .join("vectors.bin");
        fs::write(vector, b"truncated").expect("truncate vector");

        let error = validate_active_generation(temp.path()).expect_err("reject corrupt vector");

        assert!(error.to_string().contains("vector"));
    }

    #[test]
    fn active_generation_validates_a_declared_graph_artifact() {
        let temp = tempfile::tempdir().expect("artifact root");
        let built = build_embedded_artifacts(temp.path()).expect("artifact build");
        let generation_root = temp.path().join("generations").join(&built.generation);
        let graph = crate::GraphArtifact::new(
            built.manifest.corpus.content_digest.clone(),
            vec![crate::GraphNode::new(
                "fixture",
                "revision",
                "src/lib.rs",
                (1, 1, 0, 4),
                "fixture",
                "declaration",
            )],
            Vec::new(),
        )
        .expect("graph");
        let graph_checksum = graph
            .write(&generation_root.join("graph.bin"))
            .expect("write graph");
        let mut manifest = built.manifest;
        manifest.graph_checksum = Some(graph_checksum);
        manifest.graph_node_count = Some(1);
        manifest.graph_edge_count = Some(0);
        let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest JSON");
        fs::write(generation_root.join("manifest.json"), &manifest_bytes).expect("manifest");
        let pointer =
            ActiveManifestPointer::new(&built.generation, &manifest_bytes).expect("active pointer");
        fs::write(
            active_manifest_path(temp.path()),
            serde_json::to_vec(&pointer).expect("pointer JSON"),
        )
        .expect("pointer");

        validate_active_generation(temp.path()).expect("valid graph");
        fs::write(generation_root.join("graph.bin"), b"corrupt").expect("corrupt graph");
        let error = validate_active_generation(temp.path()).expect_err("reject graph");
        assert!(error.to_string().contains("graph"));
    }

    #[test]
    fn load_active_graph_checks_manifest_identity_and_returns_graph() {
        let temp = tempfile::tempdir().expect("artifact root");
        let built = build_embedded_artifacts(temp.path()).expect("artifact build");
        let graph = load_active_graph(temp.path())
            .expect("load graph")
            .expect("graph is declared");
        assert_eq!(
            graph.nodes.len(),
            usize::try_from(built.manifest.graph_node_count.unwrap()).unwrap()
        );
        assert_eq!(
            graph.edges.len(),
            usize::try_from(built.manifest.graph_edge_count.unwrap()).unwrap()
        );

        let generation_root = temp.path().join("generations").join(&built.generation);
        fs::write(generation_root.join("graph.bin"), b"corrupt").expect("corrupt graph");
        let error = load_active_graph(temp.path()).expect_err("reject corrupt graph");
        assert!(matches!(error, LifecycleError::Graph(_)));
    }

    #[test]
    fn active_generation_validation_requires_a_lexical_checksum() {
        let temp = tempfile::tempdir().expect("artifact root");
        let built = build_embedded_artifacts(temp.path()).expect("artifact build");
        let generation_root = temp.path().join("generations").join(&built.generation);
        let mut manifest = built.manifest;
        manifest.lexical_checksum = None;
        let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest JSON");
        fs::write(generation_root.join("manifest.json"), &manifest_bytes)
            .expect("rewrite manifest");
        let pointer =
            ActiveManifestPointer::new(&built.generation, &manifest_bytes).expect("active pointer");
        fs::write(
            active_manifest_path(temp.path()),
            serde_json::to_vec(&pointer).expect("pointer JSON"),
        )
        .expect("rewrite active pointer");

        let error = validate_active_generation(temp.path()).expect_err("reject missing checksum");

        assert!(error.to_string().contains("no lexical checksum"));
    }

    #[test]
    fn failed_build_removes_partial_staging_generation() {
        struct FailingProvider;

        impl EmbeddingProvider for FailingProvider {
            fn embedding_dimension(&self) -> Option<usize> {
                Some(4)
            }

            fn embed_documents(
                &mut self,
                _texts: &[String],
            ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                Err(EmbeddingError::Provider("expected test failure".into()))
            }

            fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
                unreachable!("artifact builds do not embed queries")
            }
        }

        let root = tempfile::tempdir().expect("artifact root");
        let error = build_embedded_artifacts_with_provider(
            root.path(),
            &mut FailingProvider,
            "fake",
            "test-revision",
        )
        .expect_err("provider failure");

        assert!(error.to_string().contains("expected test failure"));
        assert!(
            fs::read_dir(root.path())
                .expect("artifact root entries")
                .all(|entry| !entry
                    .expect("artifact root entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tmp-"))
        );
    }

    #[test]
    fn provider_build_stages_vector_artifact_with_manifest_checksum() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut provider = crate::FakeEmbeddingProvider::new(4);
        let summary = build_embedded_artifacts_with_provider(
            temp.path(),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("semantic build");

        assert!(summary.vector_bytes.is_some_and(|bytes| bytes > 0));
        assert!(summary.manifest.vector_checksum.is_some());
        assert_eq!(summary.observations.resolved_batch_size, Some(8));
        assert_eq!(summary.observations.vector_count, summary.chunk_count);
        let vector_path = temp
            .path()
            .join("generations")
            .join(summary.generation)
            .join("vectors.bin");
        let artifact = VectorArtifact::open_artifact(&vector_path).expect("vector artifact");
        assert_eq!(artifact.model_id, "fake");
        assert_eq!(artifact.model_revision, "test-revision");
    }

    #[test]
    fn allowlisted_build_persists_optional_source_diagnostics() {
        let source = tempfile::tempdir().expect("source tempdir");
        let output = tempfile::tempdir().expect("output tempdir");
        let spec = SourceSpec {
            relative_path: "missing.md".into(),
            title: "Optional missing source".into(),
            media_type: "text/markdown".into(),
            source_kind: crate::SourceKind::Markdown,
            trust_tier: crate::TrustTier::FirstParty,
            license: crate::LicenseStatus::InRepo,
            required: false,
            max_bytes: 1024,
            source_repository: None,
            source_revision: None,
        };
        let summary = build_allowlisted_artifacts(
            output.path(),
            source.path(),
            &RepositoryId::try_from("repo").expect("repo"),
            &Revision::try_from("rev").expect("revision"),
            &[spec],
        )
        .expect("build with optional rejection");

        assert_eq!(summary.manifest.diagnostics.len(), 1);
        assert_eq!(summary.manifest.diagnostics[0].code, "missing");
        assert_eq!(
            inspect_manifest(&active_manifest_path(output.path()))
                .expect("inspect")
                .diagnostics,
            summary.manifest.diagnostics
        );
    }
}
