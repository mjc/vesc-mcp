//! Reproducible corpus and lexical artifact lifecycle helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::corpus::chunking::{ChunkingConfig, chunk_document};
use crate::corpus::full_history::{
    GitHistoryError, GitHistoryRefreshObservations, GitHistoryTip,
    ingest_git_history_fast_forward_owned,
};
use crate::corpus::git::{
    GitCorpusSource, GitIngestionError, GitIngestionObservations, ingest_git_commit,
};
use crate::corpus::ingest::{SourceInventory, SourceRejection, SourceSpec, ingest_allowlisted};
use crate::corpus::{
    ARTIFACT_SCHEMA_V1, ArtifactManifest, ContentDigest, CorpusManifest, CorpusVersion,
    NormalizedDocument, RepositoryId, Revision, SchemaVersion,
};
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
            checkpoint_path: None,
        }),
    )
}

struct SemanticBuild<'a> {
    provider: &'a mut dyn EmbeddingProvider,
    model_id: &'a str,
    model_revision: &'a str,
    checkpoint_path: Option<&'a Path>,
}

fn build_artifacts(
    root: &Path,
    semantic: Option<SemanticBuild<'_>>,
) -> Result<BuildSummary, LifecycleError> {
    let started = Instant::now();
    let ingest_started = Instant::now();
    let chunking_started = Instant::now();
    let chunks = legacy_chunks()?;
    let mut observations = BuildObservations::default();
    observations.record(BuildPhase::Ingestion, ingest_started);
    observations.record(BuildPhase::Chunking, chunking_started);
    stage_chunks(
        root,
        &chunks,
        semantic,
        None,
        "embedded-legacy-v1",
        Vec::new(),
        Vec::new(),
        started,
        observations,
    )
}

fn legacy_chunks() -> Result<Vec<crate::Chunk>, LifecycleError> {
    embedded_entries()
        .iter()
        .map(|entry| {
            NormalizedDocument::from_legacy(entry)
                .and_then(|document| document.legacy_chunk())
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
    let mut chunks = legacy_chunks()?;
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
        checkpoint_path: None,
    });
    stage_chunks(
        root,
        &chunks,
        semantic,
        None,
        "allowlisted-v1",
        rejected,
        sources,
        started,
        observations,
    )
}

/// Build an additive corpus from the compatibility baseline and immutable Git trees.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git ingestion, chunking, or artifact staging fails.
pub fn build_git_artifacts(
    root: &Path,
    sources: &[GitCorpusSource],
) -> Result<BuildSummary, LifecycleError> {
    build_git_artifacts_with_provider(root, sources, None)
}

/// Build complete Git history, reusing cached chunks for a verified fast-forward.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git inspection, chunking, embedding, or staging fails.
pub fn build_git_history_artifacts_incrementally(
    root: &Path,
    sources: &[GitCorpusSource],
    previous_tips: Option<Vec<GitHistoryTip>>,
    previous_chunks: Option<Vec<crate::Chunk>>,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<VectorArtifact>,
    vector_checkpoint_path: Option<&Path>,
) -> Result<IncrementalGitHistoryBuildSummary, LifecycleError> {
    let started = Instant::now();
    let ingestion_started = Instant::now();
    let incremental = previous_tips
        .zip(previous_chunks)
        .map_or(Ok(None), |(tips, chunks)| {
            ingest_git_history_fast_forward_owned(sources, &tips, chunks)
        })?;
    let (history_chunks, refresh, reused_snapshot) = if let Some((chunks, refresh)) = incremental {
        (chunks, refresh, true)
    } else {
        let (chunks, refresh) = ingest_git_history_fast_forward_owned(sources, &[], Vec::new())?
            .ok_or_else(|| {
                LifecycleError::Contract("cold Git history ingestion was rejected".into())
            })?;
        (chunks, refresh, false)
    };
    let mut observations = BuildObservations::default();
    observations.record_duration(BuildPhase::Ingestion, elapsed_us(ingestion_started));
    observations.git_ingestion = Some(refresh.git.clone());
    observations.accepted_files = u64::try_from(history_chunks.len()).unwrap_or(u64::MAX);
    observations.visited_files = observations.accepted_files;
    let artifacts = stage_git_history_chunks(
        root,
        history_chunks,
        semantic,
        previous_vectors,
        vector_checkpoint_path,
        started,
        observations,
    )?;
    Ok(IncrementalGitHistoryBuildSummary {
        artifacts,
        refresh,
        reused_snapshot,
    })
}

fn stage_git_history_chunks(
    root: &Path,
    history_chunks: Vec<crate::Chunk>,
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
    previous_vectors: Option<VectorArtifact>,
    vector_checkpoint_path: Option<&Path>,
    started: Instant,
    observations: BuildObservations,
) -> Result<BuildSummary, LifecycleError> {
    let mut chunks = legacy_chunks()?;
    chunks.extend(history_chunks);
    let semantic = semantic.map(|(provider, model_id, model_revision)| SemanticBuild {
        provider,
        model_id,
        model_revision,
        checkpoint_path: vector_checkpoint_path,
    });
    stage_chunks(
        root,
        &chunks,
        semantic,
        previous_vectors,
        "git-full-history-v1",
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
) -> Result<BuildSummary, LifecycleError> {
    let started = Instant::now();
    let mut ingestion_us = 0_u64;
    let mut chunking_us = 0_u64;
    let mut chunks = legacy_chunks()?;
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
            chunks.extend(
                chunk_document(&document, ChunkingConfig::default()).map_err(|error| {
                    LifecycleError::Contract(format!("{}: {error}", document.path))
                })?,
            );
        }
        chunking_us = chunking_us.saturating_add(elapsed_us(chunking_started));
        rejected.extend(report.rejected);
        inventory.extend(report.sources);
    }
    let mut observations = BuildObservations::default();
    observations.record_duration(BuildPhase::Ingestion, ingestion_us);
    observations.record_duration(BuildPhase::Chunking, chunking_us);
    observations.visited_files = visited_files;
    observations.inventory_count = inventory.len();
    observations.rejection_count = rejected.len();
    observations.rejected_files = rejected.len() as u64;
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
    let semantic = semantic.map(|(provider, model_id, model_revision)| SemanticBuild {
        provider,
        model_id,
        model_revision,
        checkpoint_path: None,
    });
    stage_chunks(
        root,
        &chunks,
        semantic,
        None,
        "git-tree-v1",
        rejected,
        inventory,
        started,
        observations,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn stage_chunks(
    root: &Path,
    chunks: &[crate::Chunk],
    semantic: Option<SemanticBuild<'_>>,
    previous_vectors: Option<VectorArtifact>,
    corpus_version: &str,
    diagnostics: Vec<SourceRejection>,
    sources: Vec<SourceInventory>,
    started: Instant,
    mut observations: BuildObservations,
) -> Result<BuildSummary, LifecycleError> {
    observations.chunks = chunks.len();
    observations.inventory_count = observations.inventory_count.max(sources.len());
    observations.rejection_count = observations.rejection_count.max(diagnostics.len());
    observations.visited_files = observations.visited_files.max(
        observations
            .accepted_files
            .saturating_add(observations.rejected_files),
    );
    let documents = chunks
        .iter()
        .map(|chunk| chunk.document_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    observations.documents = documents.len();
    let chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id.clone())
        .collect::<Vec<_>>();
    let corpus_started = Instant::now();
    let corpus = CorpusManifest::new(
        CorpusVersion::try_from(corpus_version)
            .map_err(|error| LifecycleError::Contract(error.to_string()))?,
        documents,
        chunk_ids,
    );
    corpus
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    observations.record(BuildPhase::Corpus, corpus_started);

    fs::create_dir_all(root)?;
    let staging = tempfile::Builder::new().prefix(".tmp-").tempdir_in(root)?;
    let temp_root = staging.path();
    let generation_root = root.join("generations");
    fs::create_dir_all(&generation_root)?;
    let lexical_path = temp_root.join("lexical.json");
    let encoding_started = Instant::now();
    let (lexical_checksum, lexical_bytes) =
        LexicalIndex::write_search_artifact_with_digest(chunks.iter(), &lexical_path)?;
    observations.record(BuildPhase::Encoding, encoding_started);
    let (vector_checksum, vector_bytes) = if let Some(semantic) = semantic {
        let (vector, vector_build) = if let Some(checkpoint_path) = semantic.checkpoint_path {
            VectorArtifact::from_provider_reusing_with_checkpoint_observations(
                semantic.provider,
                chunks,
                semantic.model_id,
                semantic.model_revision,
                corpus.content_digest.clone(),
                previous_vectors,
                checkpoint_path,
            )?
        } else {
            VectorArtifact::from_provider_reusing_owned_with_observations(
                semantic.provider,
                chunks,
                semantic.model_id,
                semantic.model_revision,
                corpus.content_digest.clone(),
                previous_vectors,
            )?
        };
        observations.embedding_input_bytes = vector_build.input_bytes;
        observations.record_duration(BuildPhase::EmbeddingInput, vector_build.embedding_input_us);
        observations.record_duration(BuildPhase::Inference, vector_build.provider_us);
        observations.record_duration(
            BuildPhase::VectorFinalization,
            vector_build.vector_finalization_us,
        );
        observations.vector_build = Some(vector_build);
        observations.vector_count = vector.ids.len();
        observations.vector_dimension = Some(vector.dimension);
        observations.resolved_batch_size = Some(semantic.provider.embedding_batch_size().get());
        let vector_path = temp_root.join("vectors.bin");
        let write_started = Instant::now();
        let (checksum, bytes) = vector.write_artifact_with_digest(&vector_path)?;
        observations.record(BuildPhase::Writing, write_started);
        (Some(checksum), Some(bytes))
    } else {
        (None, None)
    };
    let manifest = ArtifactManifest {
        schema: crate::corpus::ARTIFACT_SCHEMA_V1,
        corpus,
        chunking: ChunkingConfig::default(),
        component_versions: artifact_component_versions(),
        sources,
        lexical_checksum: Some(lexical_checksum),
        vector_checksum,
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
        manifest_byte_count,
    )?;
    observations.record(BuildPhase::Validation, validation_started);

    let generation = ContentDigest::of(&manifest_bytes).to_string();
    let final_root = generation_root.join(&generation);
    if final_root.exists() {
        if let Err(error) = validate_generation(&final_root, &manifest) {
            if !repair_vector_file(temp_root, &final_root, &manifest)? {
                return Err(error);
            }
            validate_generation(&final_root, &manifest)?;
        }
    } else {
        fs::rename(temp_root, &final_root)?;
    }
    let activation_started = Instant::now();
    let active_pointer = ActiveManifestPointer::new(&generation, &manifest_bytes)?;
    let active_bytes = serde_json::to_vec(&active_pointer)?;
    let active_tmp = root.join(format!(".active.tmp-{}", std::process::id()));
    fs::write(&active_tmp, &active_bytes)?;
    fs::rename(active_tmp, root.join("active.json"))?;
    observations.record(BuildPhase::Activation, activation_started);
    observations.active_manifest_bytes = u64::try_from(active_bytes.len()).unwrap_or(u64::MAX);
    observations.artifact_bytes = lexical_bytes + vector_bytes.unwrap_or(0);
    observations.total_duration_us = elapsed_us(started);

    Ok(BuildSummary {
        generation,
        document_count: manifest.corpus.documents.len(),
        chunk_count: manifest.corpus.chunks.len(),
        lexical_bytes,
        vector_bytes,
        build_duration_us: observations.total_duration_us,
        observations,
        manifest,
    })
}

/// Version inputs which affect persisted artifact compatibility and identity.
#[must_use]
pub fn artifact_component_versions() -> BTreeMap<String, String> {
    let mut versions = BTreeMap::from([
        (
            "vesc-knowledge-index".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
        ("corpus-schema".into(), "1.0".into()),
        (
            "lexical-format".into(),
            crate::lexical::LEXICAL_FORMAT_VERSION.into(),
        ),
        ("markdown-parser".into(), "pulldown-cmark-0.13".into()),
        ("vector-format".into(), "dense-cosine-v2".into()),
    ]);
    versions.insert(
        "git-policy".into(),
        crate::corpus::git::GIT_CORPUS_POLICY_VERSION.into(),
    );
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
        let lexical_bytes = fs::read(&lexical_path)?;
        if ContentDigest::of(&lexical_bytes) != *checksum {
            return Err(LifecycleError::Contract(
                "lexical artifact checksum mismatch".into(),
            ));
        }
        LexicalIndex::open_search_artifact(&lexical_path)?;
    }
    if let Some(checksum) = &manifest.vector_checksum {
        let vector_path = root.join("vectors.bin");
        let (_, actual_checksum) = VectorArtifact::open_artifact_with_digest(&vector_path)?;
        if actual_checksum != *checksum {
            return Err(LifecycleError::Contract(
                "vector artifact checksum mismatch".into(),
            ));
        }
    }
    Ok(())
}

fn repair_vector_file(
    staged: &Path,
    existing: &Path,
    expected: &ArtifactManifest,
) -> Result<bool, LifecycleError> {
    if expected.vector_checksum.is_none() {
        return Ok(false);
    }
    let manifest: ArtifactManifest =
        serde_json::from_slice(&fs::read(existing.join("manifest.json"))?)?;
    if manifest != *expected {
        return Ok(false);
    }
    let lexical_path = existing.join("lexical.json");
    let lexical_bytes = fs::read(&lexical_path)?;
    if manifest.lexical_checksum.as_ref() != Some(&ContentDigest::of(&lexical_bytes)) {
        return Ok(false);
    }
    LexicalIndex::open_search_artifact(&lexical_path)?;
    fs::rename(staged.join("vectors.bin"), existing.join("vectors.bin"))?;
    Ok(true)
}

fn validate_written_generation(
    root: &Path,
    expected: &ArtifactManifest,
    lexical_bytes: u64,
    vector_bytes: Option<u64>,
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
    for (name, expected_bytes) in expected_files {
        let path = root.join(name);
        let actual_bytes = fs::metadata(&path)?.len();
        if actual_bytes != expected_bytes {
            return Err(LifecycleError::Contract(format!(
                "fresh artifact {name} has {actual_bytes} bytes, expected {expected_bytes}"
            )));
        }
    }
    LexicalIndex::open_search_artifact(&root.join("lexical.json"))?;
    Ok(())
}

/// Read and validate an artifact manifest without activating it.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the file is absent, malformed, or invalid.
pub fn inspect_manifest(path: &Path) -> Result<ArtifactManifest, LifecycleError> {
    let bytes = fs::read(path)?;
    let manifest = match serde_json::from_slice::<ArtifactManifest>(&bytes) {
        Ok(manifest) => manifest,
        Err(legacy_error) => {
            let pointer = serde_json::from_slice::<ActiveManifestPointer>(&bytes)
                .map_err(|_| legacy_error)?;
            pointer.validate()?;
            let root = path
                .parent()
                .ok_or_else(|| LifecycleError::Contract("active manifest has no root".into()))?;
            let generation_path = root
                .join("generations")
                .join(pointer.generation.as_str())
                .join("manifest.json");
            let generation_bytes = fs::read(generation_path)?;
            if ContentDigest::of(&generation_bytes) != pointer.manifest_checksum {
                return Err(LifecycleError::Contract(
                    "active manifest checksum mismatch".into(),
                ));
            }
            let manifest: ArtifactManifest = serde_json::from_slice(&generation_bytes)?;
            manifest
        }
    };
    manifest
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    Ok(manifest)
}

/// Return the conventional active manifest selector path for an artifact root.
///
/// [`inspect_manifest`] accepts both the current checksum-verified selector and
/// legacy full-manifest files at this path.
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
    let bytes = fs::read(active_manifest_path(root))?;
    if let Ok(manifest) = serde_json::from_slice::<ArtifactManifest>(&bytes) {
        manifest
            .validate()
            .map_err(|error| LifecycleError::Contract(error.to_string()))?;
        return Ok(root
            .join("generations")
            .join(manifest.corpus.content_digest.to_string()));
    }
    let pointer: ActiveManifestPointer = serde_json::from_slice(&bytes)?;
    pointer.validate()?;
    Ok(root.join("generations").join(pointer.generation.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(pointer.generation.as_str(), first.generation);
        let generation_manifest = first_root
            .path()
            .join("generations")
            .join(&first.generation)
            .join("manifest.json");
        assert_eq!(
            pointer.manifest_checksum,
            ContentDigest::of(&fs::read(generation_manifest).expect("generation manifest"))
        );
        assert_eq!(first.generation, second.generation);
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(first.lexical_bytes, second.lexical_bytes);
    }

    #[test]
    fn inspect_manifest_accepts_legacy_full_active_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = build_embedded_artifacts(temp.path()).expect("build");
        fs::write(
            active_manifest_path(temp.path()),
            serde_json::to_vec(&summary.manifest).expect("legacy manifest"),
        )
        .expect("write legacy active manifest");

        assert_eq!(
            inspect_manifest(&active_manifest_path(temp.path())).expect("inspect legacy"),
            summary.manifest
        );
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
    fn corrupt_existing_generation_does_not_replace_active_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = build_embedded_artifacts(temp.path()).expect("initial build");
        let lexical = temp
            .path()
            .join("generations")
            .join(first.generation)
            .join("lexical.json");
        fs::write(&lexical, b"corrupt").expect("corrupt generation");

        assert!(build_embedded_artifacts(temp.path()).is_err());
        let active = inspect_manifest(&active_manifest_path(temp.path())).expect("active");
        assert_eq!(active, first.manifest);
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
    fn missing_vector_in_an_existing_generation_is_repaired() {
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

        assert_eq!(repaired.generation, first.generation);
        assert!(vector.is_file());
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
