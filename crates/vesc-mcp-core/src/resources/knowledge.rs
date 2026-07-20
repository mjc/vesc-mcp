//! Normalized knowledge passage MCP resources.

use serde_json::to_string_pretty;
use std::path::{Path, PathBuf};

use vesc_knowledge_index::{
    ChunkId, DocumentId, LexicalIndex, NormalizedDocument, active_manifest_path, embedded_entries,
    inspect_manifest,
};

use super::{
    KnowledgeChunkUri, KnowledgeDocumentUri, ParsedResourceUri, ResourceReadError,
    ResourceReadHandler,
};

/// Read a normalized embedded knowledge chunk by stable ID.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the ID is malformed, absent, or the
/// normalized chunk cannot be serialized.
pub fn read_knowledge_chunk(
    uri: &str,
    chunk: &KnowledgeChunkUri,
) -> Result<String, ResourceReadError> {
    let requested = ChunkId::try_from(chunk.id.as_str())
        .map_err(|_| ResourceReadError::NotFound { uri: uri.into() })?;
    for entry in embedded_entries() {
        let Ok(document) = NormalizedDocument::from_legacy(entry) else {
            continue;
        };
        let Ok(candidate) = document.legacy_chunk() else {
            continue;
        };
        if candidate.chunk_id == requested {
            return to_string_pretty(&candidate).map_err(|error| ResourceReadError::ReadFailed {
                uri: uri.into(),
                message: format!("serialize knowledge chunk: {error}"),
            });
        }
    }
    Err(ResourceReadError::NotFound { uri: uri.into() })
}

fn read_knowledge_chunk_from_artifact(
    uri: &str,
    chunk: &KnowledgeChunkUri,
    artifact_root: &Path,
) -> Result<String, ResourceReadError> {
    let requested = ChunkId::try_from(chunk.id.as_str())
        .map_err(|_| ResourceReadError::NotFound { uri: uri.into() })?;
    let lexical_path = if artifact_root.is_file() {
        artifact_root.to_owned()
    } else {
        let manifest = inspect_manifest(&active_manifest_path(artifact_root)).map_err(|error| {
            ResourceReadError::ReadFailed {
                uri: uri.into(),
                message: format!("read knowledge artifact manifest: {error}"),
            }
        })?;
        artifact_root
            .join("generations")
            .join(manifest.corpus.content_digest.to_string())
            .join("lexical.json")
    };
    let index = LexicalIndex::open_artifact(&lexical_path).map_err(|error| {
        ResourceReadError::ReadFailed {
            uri: uri.into(),
            message: format!("read knowledge lexical artifact: {error}"),
        }
    })?;
    let chunk = index
        .chunks()
        .get(&requested)
        .ok_or_else(|| ResourceReadError::NotFound { uri: uri.into() })?;
    to_string_pretty(chunk).map_err(|error| ResourceReadError::ReadFailed {
        uri: uri.into(),
        message: format!("serialize knowledge chunk: {error}"),
    })
}

/// Read a normalized embedded knowledge document by stable ID.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the ID is malformed, absent, or the
/// normalized document cannot be serialized.
pub fn read_knowledge_document(
    uri: &str,
    document: &KnowledgeDocumentUri,
) -> Result<String, ResourceReadError> {
    let requested = DocumentId::try_from(document.id.as_str())
        .map_err(|_| ResourceReadError::NotFound { uri: uri.into() })?;
    for entry in embedded_entries() {
        let Ok(candidate) = NormalizedDocument::from_legacy(entry) else {
            continue;
        };
        if candidate.document_id == requested {
            return to_string_pretty(&candidate).map_err(|error| ResourceReadError::ReadFailed {
                uri: uri.into(),
                message: format!("serialize knowledge document: {error}"),
            });
        }
    }
    Err(ResourceReadError::NotFound { uri: uri.into() })
}

fn read_knowledge_document_from_artifact(
    uri: &str,
    document: &KnowledgeDocumentUri,
    artifact_root: &Path,
) -> Result<String, ResourceReadError> {
    let requested = DocumentId::try_from(document.id.as_str())
        .map_err(|_| ResourceReadError::NotFound { uri: uri.into() })?;
    let lexical_path = artifact_lexical_path(artifact_root, uri)?;
    let index = LexicalIndex::open_artifact(&lexical_path).map_err(|error| {
        ResourceReadError::ReadFailed {
            uri: uri.into(),
            message: format!("read knowledge lexical artifact: {error}"),
        }
    })?;
    let mut chunks: Vec<_> = index
        .chunks()
        .values()
        .filter(|chunk| chunk.document_id == requested)
        .collect();
    chunks.sort_by_key(|chunk| chunk.ordinal);
    let Some(first) = chunks.first() else {
        return Err(ResourceReadError::NotFound { uri: uri.into() });
    };
    let text = chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let resource = serde_json::json!({
        "document_id": first.document_id,
        "title": first.title,
        "source_kind": first.source_kind,
        "repository": first.repository,
        "revision": first.revision,
        "path": first.path,
        "category": first.category,
        "tags": first.tags,
        "identifiers": first.identifiers,
        "trust_tier": first.trust_tier,
        "text": text,
        "chunks": chunks.iter().map(|chunk| serde_json::json!({
            "chunk_id": chunk.chunk_id,
            "ordinal": chunk.ordinal,
            "heading_path": chunk.heading_path,
            "source_span": chunk.source_span,
        })).collect::<Vec<_>>(),
    });
    to_string_pretty(&resource).map_err(|error| ResourceReadError::ReadFailed {
        uri: uri.into(),
        message: format!("serialize knowledge document: {error}"),
    })
}

fn artifact_lexical_path(artifact_root: &Path, uri: &str) -> Result<PathBuf, ResourceReadError> {
    if artifact_root.is_file() {
        return Ok(artifact_root.to_owned());
    }
    let manifest = inspect_manifest(&active_manifest_path(artifact_root)).map_err(|error| {
        ResourceReadError::ReadFailed {
            uri: uri.into(),
            message: format!("read knowledge artifact manifest: {error}"),
        }
    })?;
    Ok(artifact_root
        .join("generations")
        .join(manifest.corpus.content_digest.to_string())
        .join("lexical.json"))
}

/// Handler for normalized embedded knowledge documents.
#[derive(Debug, Clone, Copy, Default)]
pub struct KnowledgeDocumentResourceHandler;

/// Handler for normalized knowledge documents, optionally backed by an artifact.
#[derive(Debug, Clone, Default)]
pub struct ConfiguredKnowledgeDocumentResourceHandler {
    knowledge: crate::config::KnowledgeConfig,
}

impl ConfiguredKnowledgeDocumentResourceHandler {
    #[must_use]
    pub fn new(artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            knowledge: crate::config::KnowledgeConfig {
                artifact_path: Some(artifact_root.into()),
                ..crate::config::KnowledgeConfig::default()
            },
        }
    }

    #[must_use]
    pub fn from_config() -> Self {
        Self {
            knowledge: crate::config::McpConfig::load().knowledge.clone(),
        }
    }

    #[must_use]
    pub const fn with_config(knowledge: crate::config::KnowledgeConfig) -> Self {
        Self { knowledge }
    }
}

impl ResourceReadHandler for KnowledgeDocumentResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(uri, ParsedResourceUri::KnowledgeDocument(_))
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        let ParsedResourceUri::KnowledgeDocument(document) = uri else {
            return Err(ResourceReadError::NotFound { uri: uri.to_uri() });
        };
        read_knowledge_document(&uri.to_uri(), document)
    }
}

impl ResourceReadHandler for ConfiguredKnowledgeDocumentResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::KnowledgeDocument(_)
                | ParsedResourceUri::SnapshotKnowledgeDocument(_)
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        match uri {
            ParsedResourceUri::KnowledgeDocument(document) => self
                .knowledge
                .resolved_artifact_path()
                .as_ref()
                .map_or_else(
                    || read_knowledge_document(&uri.to_uri(), document),
                    |root| read_knowledge_document_from_artifact(&uri.to_uri(), document, root),
                ),
            ParsedResourceUri::SnapshotKnowledgeDocument(document) => {
                let root = snapshot_artifact(&self.knowledge, &document.snapshot, &uri.to_uri())?;
                read_knowledge_document_from_artifact(
                    &uri.to_uri(),
                    &KnowledgeDocumentUri {
                        id: document.id.clone(),
                    },
                    &root,
                )
            }
            _ => Err(ResourceReadError::NotFound { uri: uri.to_uri() }),
        }
    }
}

/// Handler for normalized embedded knowledge passage resources.
#[derive(Debug, Clone, Copy, Default)]
pub struct KnowledgeChunkResourceHandler;

/// Handler for normalized knowledge passages, optionally backed by a staged artifact.
#[derive(Debug, Clone, Default)]
pub struct ConfiguredKnowledgeChunkResourceHandler {
    knowledge: crate::config::KnowledgeConfig,
}

impl ConfiguredKnowledgeChunkResourceHandler {
    #[must_use]
    pub fn new(artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            knowledge: crate::config::KnowledgeConfig {
                artifact_path: Some(artifact_root.into()),
                ..crate::config::KnowledgeConfig::default()
            },
        }
    }

    #[must_use]
    pub fn from_config() -> Self {
        Self {
            knowledge: crate::config::McpConfig::load().knowledge.clone(),
        }
    }

    #[must_use]
    pub const fn with_config(knowledge: crate::config::KnowledgeConfig) -> Self {
        Self { knowledge }
    }
}

impl ResourceReadHandler for KnowledgeChunkResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(uri, ParsedResourceUri::KnowledgeChunk(_))
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        let ParsedResourceUri::KnowledgeChunk(chunk) = uri else {
            return Err(ResourceReadError::NotFound { uri: uri.to_uri() });
        };
        read_knowledge_chunk(&uri.to_uri(), chunk)
    }
}

impl ResourceReadHandler for ConfiguredKnowledgeChunkResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::KnowledgeChunk(_) | ParsedResourceUri::SnapshotKnowledgeChunk(_)
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        match uri {
            ParsedResourceUri::KnowledgeChunk(chunk) => self
                .knowledge
                .resolved_artifact_path()
                .as_ref()
                .map_or_else(
                    || read_knowledge_chunk(&uri.to_uri(), chunk),
                    |root| read_knowledge_chunk_from_artifact(&uri.to_uri(), chunk, root),
                ),
            ParsedResourceUri::SnapshotKnowledgeChunk(chunk) => {
                let root = snapshot_artifact(&self.knowledge, &chunk.snapshot, &uri.to_uri())?;
                read_knowledge_chunk_from_artifact(
                    &uri.to_uri(),
                    &KnowledgeChunkUri {
                        id: chunk.id.clone(),
                    },
                    &root,
                )
            }
            _ => Err(ResourceReadError::NotFound { uri: uri.to_uri() }),
        }
    }
}

fn snapshot_artifact(
    knowledge: &crate::config::KnowledgeConfig,
    snapshot: &str,
    uri: &str,
) -> Result<PathBuf, ResourceReadError> {
    knowledge
        .resolved_snapshot(snapshot)
        .map(|resolved| resolved.path)
        .ok_or_else(|| ResourceReadError::NotFound { uri: uri.into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesc_knowledge_index::build_embedded_artifacts;

    #[test]
    fn configured_handler_reads_staged_artifact_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = build_embedded_artifacts(temp.path()).expect("build artifact");
        let chunk_id = summary
            .manifest
            .corpus
            .chunks
            .first()
            .expect("chunk id")
            .to_string();
        let parsed = ParsedResourceUri::KnowledgeChunk(KnowledgeChunkUri { id: chunk_id });
        let body = ConfiguredKnowledgeChunkResourceHandler::new(temp.path())
            .read(&parsed)
            .expect("read staged chunk");
        assert!(body.contains("chunk_id"));
    }

    #[test]
    fn configured_handler_reads_staged_artifact_documents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary = build_embedded_artifacts(temp.path()).expect("build artifact");
        let document_id = summary
            .manifest
            .corpus
            .documents
            .first()
            .expect("document id")
            .to_string();
        let parsed = ParsedResourceUri::KnowledgeDocument(KnowledgeDocumentUri { id: document_id });
        let body = ConfiguredKnowledgeDocumentResourceHandler::new(temp.path())
            .read(&parsed)
            .expect("read staged document");
        assert!(body.contains("document_id"));
        assert!(body.contains("\"chunks\""));
        assert!(body.contains("\"text\""));
    }

    #[test]
    fn configured_handler_reads_an_immutable_snapshot_uri() {
        let temp = tempfile::tempdir().expect("tempdir");
        let snapshot = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let artifact = temp.path().join("artifacts").join(snapshot);
        let summary = build_embedded_artifacts(&artifact).expect("build artifact");
        std::fs::create_dir_all(temp.path().join("snapshots")).expect("snapshot directory");
        std::fs::write(
            temp.path()
                .join("snapshots")
                .join(format!("{snapshot}.json")),
            serde_json::to_vec(&serde_json::json!({
                "id": snapshot,
                "profile": "selected_trees",
                "repositories": []
            }))
            .expect("snapshot manifest"),
        )
        .expect("write snapshot");
        let chunk_id = summary.manifest.corpus.chunks[0].to_string();
        let parsed = crate::resources::parse_resource_uri(&format!(
            "vesc://knowledge/snapshot/{snapshot}/chunk/{chunk_id}"
        ))
        .expect("snapshot URI");
        let config = crate::config::KnowledgeConfig {
            data_root: Some(
                crate::managed_repositories::DataRoot::new(temp.path().to_path_buf())
                    .expect("data root"),
            ),
            ..crate::config::KnowledgeConfig::default()
        };

        let body = ConfiguredKnowledgeChunkResourceHandler::with_config(config)
            .read(&parsed)
            .expect("read versioned chunk");

        assert!(body.contains("chunk_id"));
    }
}
