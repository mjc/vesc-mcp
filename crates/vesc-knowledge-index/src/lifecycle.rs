//! Reproducible corpus and lexical artifact lifecycle helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::corpus::chunking::{ChunkingConfig, chunk_document};
#[cfg(feature = "git-corpus")]
use crate::corpus::git::{GitCorpusSource, GitIngestionError, ingest_git_commit};
use crate::corpus::ingest::{SourceInventory, SourceRejection, SourceSpec, ingest_allowlisted};
use crate::corpus::{
    ArtifactManifest, ContentDigest, CorpusManifest, CorpusVersion, NormalizedDocument,
    RepositoryId, Revision,
};
use crate::{
    EmbeddingError, EmbeddingProvider, LexicalError, LexicalIndex, VectorArtifact, embedded_entries,
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
    #[cfg(feature = "git-corpus")]
    #[error("Git corpus ingestion failed: {0}")]
    Git(#[from] GitIngestionError),
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
    pub manifest: ArtifactManifest,
}

/// Build and atomically activate the embedded corpus generation under `root`.
///
/// The serialized manifest contains only portable IDs and checksums. Files are
/// written beneath a same-filesystem temporary directory before activation.
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

fn build_artifacts(
    root: &Path,
    semantic: Option<SemanticBuild<'_>>,
) -> Result<BuildSummary, LifecycleError> {
    let chunks = legacy_chunks()?;
    stage_chunks(
        root,
        &chunks,
        semantic,
        "embedded-legacy-v1",
        Vec::new(),
        Vec::new(),
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
    let report = ingest_allowlisted(source_root, repository, revision, specs)
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    let crate::corpus::ingest::IngestionReport {
        documents,
        rejected,
        sources,
    } = report;
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
    let semantic = semantic.map(|(provider, model_id, model_revision)| SemanticBuild {
        provider,
        model_id,
        model_revision,
    });
    stage_chunks(root, &chunks, semantic, "allowlisted-v1", rejected, sources)
}

/// Build an additive corpus from the compatibility baseline and immutable Git trees.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git ingestion, chunking, or artifact staging fails.
#[cfg(feature = "git-corpus")]
pub fn build_git_artifacts(
    root: &Path,
    sources: &[GitCorpusSource],
) -> Result<BuildSummary, LifecycleError> {
    build_git_artifacts_with_provider(root, sources, None)
}

/// Build an additive immutable Git-tree corpus with an optional embedding provider.
///
/// # Errors
///
/// Returns [`LifecycleError`] when Git ingestion, chunking, embedding, or artifact staging fails.
#[cfg(feature = "git-corpus")]
pub fn build_git_artifacts_with_provider(
    root: &Path,
    sources: &[GitCorpusSource],
    semantic: Option<(&mut dyn EmbeddingProvider, &str, &str)>,
) -> Result<BuildSummary, LifecycleError> {
    let mut chunks = legacy_chunks()?;
    let mut rejected = Vec::new();
    let mut inventory = Vec::new();
    let mut ordered_sources = sources.iter().collect::<Vec<_>>();
    ordered_sources.sort_by(|left, right| {
        left.repository_id
            .cmp(&right.repository_id)
            .then_with(|| left.revision.cmp(&right.revision))
    });
    for source in ordered_sources {
        let report = ingest_git_commit(
            &source.repository_path,
            &source.repository_id,
            &source.revision,
            source.trust_tier,
            &source.license,
            &source.policy,
        )?;
        for document in report.documents {
            chunks.extend(
                chunk_document(&document, ChunkingConfig::default()).map_err(|error| {
                    LifecycleError::Contract(format!("{}: {error}", document.path))
                })?,
            );
        }
        rejected.extend(report.rejected);
        inventory.extend(report.sources);
    }
    let semantic = semantic.map(|(provider, model_id, model_revision)| SemanticBuild {
        provider,
        model_id,
        model_revision,
    });
    stage_chunks(root, &chunks, semantic, "git-tree-v1", rejected, inventory)
}

fn stage_chunks(
    root: &Path,
    chunks: &[crate::Chunk],
    semantic: Option<SemanticBuild<'_>>,
    corpus_version: &str,
    diagnostics: Vec<SourceRejection>,
    sources: Vec<SourceInventory>,
) -> Result<BuildSummary, LifecycleError> {
    let started = Instant::now();
    let documents = chunks
        .iter()
        .map(|chunk| chunk.document_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id.clone())
        .collect::<Vec<_>>();
    let corpus = CorpusManifest::new(
        CorpusVersion::try_from(corpus_version)
            .map_err(|error| LifecycleError::Contract(error.to_string()))?,
        documents,
        chunk_ids,
    );
    corpus
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;

    let lexical = LexicalIndex::build(chunks)?;
    fs::create_dir_all(root)?;
    let temp_root = unique_temp_root(root)?;
    let generation_root = root.join("generations");
    fs::create_dir_all(&generation_root)?;
    let lexical_path = temp_root.join("lexical.json");
    lexical.write_artifact(&lexical_path)?;
    let lexical_bytes = fs::read(&lexical_path)?;
    let (vector_checksum, vector_bytes) = if let Some(semantic) = semantic {
        let vector = VectorArtifact::from_provider(
            semantic.provider,
            chunks,
            semantic.model_id,
            semantic.model_revision,
            corpus.content_digest.clone(),
        )?;
        let vector_path = temp_root.join("vectors.bin");
        vector.write_artifact(&vector_path)?;
        let bytes = fs::read(&vector_path)?;
        (Some(ContentDigest::of(&bytes)), Some(bytes.len() as u64))
    } else {
        (None, None)
    };
    let manifest = ArtifactManifest {
        schema: crate::corpus::ARTIFACT_SCHEMA_V1,
        corpus,
        chunking: ChunkingConfig::default(),
        component_versions: component_versions(),
        sources,
        lexical_checksum: Some(ContentDigest::of(&lexical_bytes)),
        vector_checksum,
        tool_version: env!("CARGO_PKG_VERSION").into(),
        diagnostics,
    };
    manifest
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    fs::write(
        temp_root.join("corpus.json"),
        serde_json::to_vec(&manifest.corpus)?,
    )?;
    fs::write(
        temp_root.join("manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    validate_generation(&temp_root, &manifest)?;

    let generation = manifest.corpus.content_digest.to_string();
    let final_root = generation_root.join(&generation);
    if final_root.exists() {
        validate_generation(&final_root, &manifest)?;
        fs::remove_dir_all(&temp_root)?;
    } else {
        fs::rename(&temp_root, &final_root)?;
    }
    let active_tmp = root.join(format!(".active.tmp-{}", std::process::id()));
    fs::write(&active_tmp, serde_json::to_vec(&manifest)?)?;
    fs::rename(active_tmp, root.join("active.json"))?;

    Ok(BuildSummary {
        generation,
        document_count: manifest.corpus.documents.len(),
        chunk_count: manifest.corpus.chunks.len(),
        lexical_bytes: lexical_bytes.len() as u64,
        vector_bytes,
        build_duration_us: elapsed_us(started),
        manifest,
    })
}

fn component_versions() -> BTreeMap<String, String> {
    let versions = BTreeMap::from([
        (
            "vesc-knowledge-index".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
        ("corpus-schema".into(), "1.0".into()),
        ("lexical-format".into(), "tantivy-0.26".into()),
        ("markdown-parser".into(), "pulldown-cmark-0.13".into()),
        ("vector-format".into(), "dense-cosine-v1".into()),
    ]);
    #[cfg(feature = "git-corpus")]
    {
        let mut versions = versions;
        versions.insert(
            "git-corpus-policy".into(),
            crate::corpus::git::GIT_CORPUS_POLICY_VERSION.into(),
        );
        versions
    }
    #[cfg(not(feature = "git-corpus"))]
    {
        versions
    }
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn unique_temp_root(root: &Path) -> Result<PathBuf, std::io::Error> {
    for suffix in 0..100_u32 {
        let candidate = root.join(format!(".tmp-{}-{suffix}", std::process::id()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique artifact staging directory",
    ))
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
        LexicalIndex::open_artifact(&lexical_path)?;
    }
    if let Some(checksum) = &manifest.vector_checksum {
        let vector_path = root.join("vectors.bin");
        let vector_bytes = fs::read(&vector_path)?;
        if ContentDigest::of(&vector_bytes) != *checksum {
            return Err(LifecycleError::Contract(
                "vector artifact checksum mismatch".into(),
            ));
        }
        VectorArtifact::decode(&vector_bytes)?;
    }
    Ok(())
}

/// Read and validate an artifact manifest without activating it.
///
/// # Errors
///
/// Returns [`LifecycleError`] when the file is absent, malformed, or invalid.
pub fn inspect_manifest(path: &Path) -> Result<ArtifactManifest, LifecycleError> {
    let manifest: ArtifactManifest = serde_json::from_slice(&fs::read(path)?)?;
    manifest
        .validate()
        .map_err(|error| LifecycleError::Contract(error.to_string()))?;
    Ok(manifest)
}

/// Return the conventional active manifest path for an artifact root.
#[must_use]
pub fn active_manifest_path(root: &Path) -> PathBuf {
    root.join("active.json")
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
        assert!(!summary.manifest.component_versions.is_empty());
        assert!(summary.vector_bytes.is_none());
        let text = fs::read_to_string(active_manifest_path(temp.path())).expect("manifest");
        assert!(!text.contains(temp.path().to_string_lossy().as_ref()));
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
