//! Versioned, provenance-preserving corpus contracts for retrieval artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Category, IndexEntry};

pub mod chunking;
pub mod full_history;
pub mod git;
pub mod ingest;

pub(crate) use self::full_history::history_content_key_for_chunk;
use self::ingest::{SourceInventory, SourceRejection};

/// The first corpus schema supported by this crate.
pub const CORPUS_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 1, minor: 0 };
/// Corpus-manifest schema with compact inventory counts.
pub const CORPUS_MANIFEST_SCHEMA_V2: SchemaVersion = SchemaVersion { major: 1, minor: 1 };
/// The first artifact schema supported by this crate.
pub const ARTIFACT_SCHEMA_V1: SchemaVersion = SchemaVersion { major: 1, minor: 0 };

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CorpusError {
    #[error("{kind} must not be empty")]
    EmptyValue { kind: &'static str },
    #[error("invalid {kind}: {value:?}")]
    InvalidValue { kind: &'static str, value: String },
    #[error("unsupported {kind} schema major {major}")]
    UnsupportedSchema { kind: &'static str, major: u16 },
    #[error("source span end precedes start")]
    ReverseSourceSpan,
    #[error("content digest does not match the stored content")]
    DigestMismatch,
    #[error("invalid chunk adjacency: {0}")]
    InvalidAdjacency(String),
    #[error("field {kind} exceeds the {max} byte bound")]
    OversizedField { kind: &'static str, max: usize },
}

macro_rules! string_id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = CorpusError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if value.trim().is_empty() {
                    return Err(CorpusError::EmptyValue { kind: $kind });
                }
                Ok(Self(value))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = CorpusError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from(value).map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

string_id!(DocumentId, "document id");
string_id!(ChunkId, "chunk id");
string_id!(CorpusVersion, "corpus version");
string_id!(RepositoryId, "repository id");
string_id!(Revision, "revision");

/// A SHA-256 digest with an explicit algorithm prefix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest(String);

impl ContentDigest {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(format!("sha256:{}", hex_bytes(digest.as_ref())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = CorpusError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(CorpusError::InvalidValue {
                kind: "content digest",
                value,
            });
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CorpusError::InvalidValue {
                kind: "content digest",
                value,
            });
        }
        Ok(Self(value))
    }
}

impl TryFrom<&str> for ContentDigest {
    type Error = CorpusError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

impl AsRef<str> for ContentDigest {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for ContentDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// A stable URI that identifies a readable evidence resource.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceUri(String);

impl ResourceUri {
    /// Creates a URI after checking that it has a non-empty scheme.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError::InvalidValue`] for malformed or whitespace-containing URIs.
    pub fn try_new(value: impl Into<String>) -> Result<Self, CorpusError> {
        let value = value.into();
        let Some((scheme, _)) = value.split_once("://") else {
            return Err(CorpusError::InvalidValue {
                kind: "resource URI",
                value,
            });
        };
        if scheme.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(CorpusError::InvalidValue {
                kind: "resource URI",
                value,
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ResourceUri {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ResourceUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<String> for ResourceUri {
    type Error = CorpusError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for ResourceUri {
    type Error = CorpusError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

/// Major/minor version for an on-disk contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaVersion {
    pub major: u16,
    pub minor: u16,
}

impl SchemaVersion {
    /// Rejects unknown major versions while allowing compatible minor versions.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError::UnsupportedSchema`] when the major versions differ.
    pub const fn ensure_major(
        self,
        expected: Self,
        kind: &'static str,
    ) -> Result<Self, CorpusError> {
        if self.major != expected.major {
            return Err(CorpusError::UnsupportedSchema {
                kind,
                major: self.major,
            });
        }
        Ok(self)
    }
}

/// Source family used to apply allowlists and trust policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceKind {
    LegacyIndex,
    Markdown,
    CatalogYaml,
    CatalogJson,
    Fixture,
    VendorFile,
    GitBlob,
    ModelFeedback,
}

/// Trust classification retained with every document and chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrustTier {
    FirstParty,
    CuratedUpstream,
    Fixture,
    UnverifiedModelFeedback,
}

/// License/attribution decision for source content.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "value")]
#[non_exhaustive]
pub enum LicenseStatus {
    InRepo,
    Redistributable { spdx: String },
    ReferenceOnly,
}

/// Inclusive source span. Byte offsets are optional when a parser cannot provide them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: Option<u64>,
    pub end_byte: Option<u64>,
}

impl SourceSpan {
    /// Creates an inclusive span after checking that both ranges are ordered.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError::ReverseSourceSpan`] when an end precedes its start.
    pub const fn new(
        start_line: u32,
        end_line: u32,
        start_byte: Option<u64>,
        end_byte: Option<u64>,
    ) -> Result<Self, CorpusError> {
        if end_line < start_line
            || matches!((start_byte, end_byte), (Some(start), Some(end)) if end < start)
        {
            return Err(CorpusError::ReverseSourceSpan);
        }
        Ok(Self {
            start_line,
            end_line,
            start_byte,
            end_byte,
        })
    }
}

/// A normalized source document before passage chunking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedDocument {
    pub schema: SchemaVersion,
    pub document_id: DocumentId,
    pub title: String,
    pub source_kind: SourceKind,
    pub repository: RepositoryId,
    pub revision: Revision,
    pub path: String,
    pub media_type: String,
    pub canonical_uri: Option<ResourceUri>,
    pub category: Option<Category>,
    pub tags: BTreeSet<String>,
    pub identifiers: BTreeSet<String>,
    pub trust_tier: TrustTier,
    pub license: LicenseStatus,
    pub content: String,
    pub content_digest: ContentDigest,
    pub source_span: Option<SourceSpan>,
    pub adapter_schema: SchemaVersion,
    #[serde(default)]
    pub legacy_ids: Vec<String>,
}

impl NormalizedDocument {
    /// Creates a document with an ID derived from portable source identity and content.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] when a required field is empty or the path escapes its repository root.
    pub fn new(
        title: impl Into<String>,
        source_kind: SourceKind,
        repository: RepositoryId,
        revision: Revision,
        path: impl Into<String>,
        media_type: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self, CorpusError> {
        let title = title.into();
        let path = path.into();
        let media_type = media_type.into();
        let content = content.into();
        if title.trim().is_empty() || path.trim().is_empty() || media_type.trim().is_empty() {
            return Err(CorpusError::EmptyValue {
                kind: "document field",
            });
        }
        if path.starts_with('/') || path.split('/').any(|part| part == "..") {
            return Err(CorpusError::InvalidValue {
                kind: "repo-relative path",
                value: path,
            });
        }
        let content_digest = ContentDigest::of(content.as_bytes());
        let document_id = DocumentId::from_identity(&repository, &revision, &path, &content_digest);
        Ok(Self {
            schema: CORPUS_SCHEMA_V1,
            document_id,
            title,
            source_kind,
            repository,
            revision,
            path,
            media_type,
            canonical_uri: None,
            category: None,
            tags: BTreeSet::new(),
            identifiers: BTreeSet::new(),
            trust_tier: TrustTier::FirstParty,
            license: LicenseStatus::InRepo,
            content,
            content_digest,
            source_span: None,
            adapter_schema: CORPUS_SCHEMA_V1,
            legacy_ids: Vec::new(),
        })
    }

    /// Migrates one legacy summary-sized entry without losing its identifier.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] when the legacy source metadata cannot form a valid document.
    pub fn from_legacy(entry: &IndexEntry) -> Result<Self, CorpusError> {
        let repository = RepositoryId::try_from(entry.source.repo.as_str())?;
        let revision = Revision::try_from("legacy")?;
        let mut document = Self::new(
            entry.name.clone(),
            SourceKind::LegacyIndex,
            repository,
            revision,
            entry.source.path.clone(),
            "text/plain",
            entry.summary.clone(),
        )?;
        document.category = Some(entry.category);
        document.tags = entry.keywords.iter().cloned().collect();
        document.identifiers.insert(entry.name.clone());
        document.identifiers.insert(entry.id.clone());
        document.source_span = Some(SourceSpan::new(
            entry.source.line,
            entry.source.line,
            None,
            None,
        )?);
        document.legacy_ids.push(entry.id.clone());
        Ok(document)
    }

    /// Produces the one-chunk compatibility representation for a legacy entry.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] when the legacy summary is empty or its span is invalid.
    pub fn legacy_chunk(&self) -> Result<Chunk, CorpusError> {
        Chunk::from_document(self, 0, self.content.clone(), Vec::new(), self.source_span)
    }
}

/// A bounded retrieval passage with stable adjacency and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub schema: SchemaVersion,
    pub chunk_id: ChunkId,
    pub document_id: DocumentId,
    pub ordinal: u32,
    pub title: String,
    pub source_kind: SourceKind,
    pub repository: RepositoryId,
    pub revision: Revision,
    pub path: String,
    pub heading_path: Vec<String>,
    pub text: String,
    pub source_span: Option<SourceSpan>,
    pub char_count: u32,
    pub byte_count: u64,
    pub category: Option<Category>,
    pub tags: BTreeSet<String>,
    pub identifiers: Vec<CompactString>,
    #[serde(default)]
    pub legacy_ids: Vec<String>,
    pub trust_tier: TrustTier,
    pub resource_uri: Option<ResourceUri>,
    pub previous_chunk: Option<ChunkId>,
    pub next_chunk: Option<ChunkId>,
    pub content_digest: ContentDigest,
}

impl Chunk {
    /// Builds a stable chunk from normalized document metadata and passage text.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] when the passage is empty or its character count exceeds `u32`.
    pub fn from_document(
        document: &NormalizedDocument,
        ordinal: u32,
        text: String,
        heading_path: Vec<String>,
        source_span: Option<SourceSpan>,
    ) -> Result<Self, CorpusError> {
        if text.trim().is_empty() {
            return Err(CorpusError::EmptyValue { kind: "chunk text" });
        }
        let content_digest = ContentDigest::of(text.as_bytes());
        let anchor = heading_path.join("/");
        let chunk_id =
            ChunkId::from_identity(&document.document_id, ordinal, &anchor, &content_digest);
        let resource_uri = ResourceUri::try_from(format!("vesc://knowledge/chunk/{chunk_id}"))?;
        Ok(Self {
            schema: CORPUS_SCHEMA_V1,
            chunk_id,
            document_id: document.document_id.clone(),
            ordinal,
            title: document.title.clone(),
            source_kind: document.source_kind,
            repository: document.repository.clone(),
            revision: document.revision.clone(),
            path: document.path.clone(),
            heading_path,
            char_count: u32::try_from(text.chars().count()).map_err(|_| {
                CorpusError::InvalidValue {
                    kind: "chunk character count",
                    value: text.chars().count().to_string(),
                }
            })?,
            byte_count: text.len() as u64,
            text,
            source_span,
            category: document.category,
            tags: document.tags.clone(),
            identifiers: document
                .identifiers
                .iter()
                .map(CompactString::from)
                .collect(),
            legacy_ids: document.legacy_ids.clone(),
            trust_tier: document.trust_tier,
            resource_uri: Some(resource_uri),
            previous_chunk: None,
            next_chunk: None,
            content_digest,
        })
    }

    /// Validates stored counts and the passage digest before artifact use.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError::DigestMismatch`] when serialized content was
    /// changed without rebuilding the contract fields.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.text.trim().is_empty()
            || self.char_count != u32::try_from(self.text.chars().count()).unwrap_or(u32::MAX)
            || self.byte_count != self.text.len() as u64
            || self.content_digest != ContentDigest::of(self.text.as_bytes())
        {
            return Err(CorpusError::DigestMismatch);
        }
        Ok(())
    }
}

/// Validates reciprocal adjacency handles for a set of chunks.
///
/// # Errors
///
/// Returns [`CorpusError`] when a chunk is invalid or an adjacency handle is
/// missing, self-referential, or not reciprocal.
pub fn validate_chunk_adjacency(chunks: &[Chunk]) -> Result<(), CorpusError> {
    let by_id: std::collections::BTreeMap<_, _> = chunks
        .iter()
        .map(|chunk| (chunk.chunk_id.clone(), chunk))
        .collect();
    for chunk in chunks {
        chunk.validate()?;
        if chunk.previous_chunk.as_ref() == Some(&chunk.chunk_id)
            || chunk.next_chunk.as_ref() == Some(&chunk.chunk_id)
        {
            return Err(CorpusError::InvalidAdjacency(chunk.chunk_id.to_string()));
        }
        if let Some(previous) = &chunk.previous_chunk {
            let Some(previous_chunk) = by_id.get(previous) else {
                return Err(CorpusError::InvalidAdjacency(previous.to_string()));
            };
            if previous_chunk.next_chunk.as_ref() != Some(&chunk.chunk_id) {
                return Err(CorpusError::InvalidAdjacency(chunk.chunk_id.to_string()));
            }
        }
        if let Some(next) = &chunk.next_chunk {
            let Some(next_chunk) = by_id.get(next) else {
                return Err(CorpusError::InvalidAdjacency(next.to_string()));
            };
            if next_chunk.previous_chunk.as_ref() != Some(&chunk.chunk_id) {
                return Err(CorpusError::InvalidAdjacency(chunk.chunk_id.to_string()));
            }
        }
    }
    Ok(())
}

/// Deterministic manifest for a normalized corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    pub schema: SchemaVersion,
    pub corpus_version: CorpusVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<DocumentId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<ChunkId>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub document_count: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub chunk_count: usize,
    pub content_digest: ContentDigest,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip callback receives a reference.
const fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl CorpusManifest {
    /// Creates a sorted manifest and computes its content digest.
    #[must_use]
    pub fn new(
        corpus_version: CorpusVersion,
        mut documents: Vec<DocumentId>,
        mut chunks: Vec<ChunkId>,
    ) -> Self {
        documents.sort_unstable();
        chunks.sort_unstable();
        let mut digest_input = Sha256::new();
        for id in &documents {
            digest_input.update(id.as_ref().as_bytes());
            digest_input.update([0]);
        }
        for id in &chunks {
            digest_input.update(id.as_ref().as_bytes());
            digest_input.update([0]);
        }
        let digest = digest_input.finalize();
        let content_digest = ContentDigest(format!("sha256:{}", hex_bytes(digest.as_ref())));
        Self {
            schema: CORPUS_SCHEMA_V1,
            corpus_version,
            documents,
            chunks,
            document_count: 0,
            chunk_count: 0,
            content_digest,
        }
    }

    /// Creates a compact manifest from a streamed index inventory.
    #[must_use]
    pub const fn from_inventory(
        corpus_version: CorpusVersion,
        document_count: usize,
        chunk_count: usize,
        content_digest: ContentDigest,
    ) -> Self {
        Self {
            schema: CORPUS_MANIFEST_SCHEMA_V2,
            corpus_version,
            documents: Vec::new(),
            chunks: Vec::new(),
            document_count,
            chunk_count,
            content_digest,
        }
    }

    /// Returns the number of distinct documents represented by the corpus.
    #[must_use]
    pub fn document_count(&self) -> usize {
        self.document_count.max(self.documents.len())
    }

    /// Returns the number of distinct chunks represented by the corpus.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunk_count.max(self.chunks.len())
    }

    /// Validates uniqueness and the supported corpus schema before activation.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] for an unsupported schema or duplicate document/chunk IDs.
    pub fn validate(&self) -> Result<(), CorpusError> {
        self.schema.ensure_major(CORPUS_SCHEMA_V1, "corpus")?;
        if self.documents.windows(2).any(|pair| pair[0] == pair[1])
            || self.chunks.windows(2).any(|pair| pair[0] == pair[1])
            || (!self.documents.is_empty()
                && self.document_count != 0
                && self.document_count != self.documents.len())
            || (!self.chunks.is_empty()
                && self.chunk_count != 0
                && self.chunk_count != self.chunks.len())
        {
            return Err(CorpusError::InvalidValue {
                kind: "manifest duplicate id",
                value: self.content_digest.to_string(),
            });
        }
        Ok(())
    }

    /// Serializes the manifest using stable struct/array ordering.
    ///
    /// # Errors
    ///
    /// Returns the underlying JSON serialization error.
    pub fn canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// Manifest for generated lexical/vector artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub schema: SchemaVersion,
    pub corpus: CorpusManifest,
    #[serde(default)]
    pub chunking: chunking::ChunkingConfig,
    #[serde(default = "default_component_versions")]
    pub component_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub sources: Vec<SourceInventory>,
    pub lexical_checksum: Option<ContentDigest>,
    pub vector_checksum: Option<ContentDigest>,
    pub tool_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<SourceRejection>,
}

fn default_component_versions() -> BTreeMap<String, String> {
    BTreeMap::from([
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
    ])
}

impl ArtifactManifest {
    /// Validates every nested schema and required artifact metadata.
    ///
    /// # Errors
    ///
    /// Returns [`CorpusError`] when a nested manifest is incompatible or the
    /// tool version is empty.
    pub fn validate(&self) -> Result<(), CorpusError> {
        self.schema.ensure_major(ARTIFACT_SCHEMA_V1, "artifact")?;
        self.corpus.validate()?;
        if self.tool_version.trim().is_empty() {
            return Err(CorpusError::EmptyValue {
                kind: "artifact tool version",
            });
        }
        if self.component_versions.is_empty()
            || self
                .component_versions
                .values()
                .any(|version| version.trim().is_empty())
        {
            return Err(CorpusError::EmptyValue {
                kind: "artifact component version",
            });
        }
        for source in &self.sources {
            if source.relative_path.is_absolute()
                || source
                    .relative_path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(CorpusError::InvalidValue {
                    kind: "artifact source path",
                    value: source.relative_path.display().to_string(),
                });
            }
            if source.rejection.is_none()
                && (source.byte_count.is_none() || source.content_digest.is_none())
            {
                return Err(CorpusError::InvalidValue {
                    kind: "accepted source digest",
                    value: source.relative_path.display().to_string(),
                });
            }
        }
        Ok(())
    }
}

impl DocumentId {
    #[must_use]
    fn from_identity(
        repository: &RepositoryId,
        revision: &Revision,
        path: &str,
        digest: &ContentDigest,
    ) -> Self {
        Self(format!(
            "doc-{}",
            digest_hex(
                b"vesc-mcp/document/v1",
                &[
                    repository.as_ref(),
                    revision.as_ref(),
                    path,
                    digest.as_ref()
                ]
            )
        ))
    }
}

impl ChunkId {
    #[must_use]
    fn from_identity(
        document: &DocumentId,
        ordinal: u32,
        anchor: &str,
        digest: &ContentDigest,
    ) -> Self {
        Self(format!(
            "chunk-{}",
            digest_hex(
                b"vesc-mcp/chunk/v1",
                &[
                    document.as_ref(),
                    &ordinal.to_string(),
                    anchor,
                    digest.as_ref()
                ],
            )
        ))
    }
}

fn digest_hex(domain: &[u8], parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    hex_bytes(digest.as_ref())
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_id_ignores_checkout_root() {
        let first = NormalizedDocument::new(
            "Title",
            SourceKind::Markdown,
            RepositoryId::try_from("vesc-mcp").expect("repo"),
            Revision::try_from("abc").expect("revision"),
            "docs/example.md",
            "text/markdown",
            "content",
        )
        .expect("document");
        let second = NormalizedDocument::new(
            "Title",
            SourceKind::Markdown,
            RepositoryId::try_from("vesc-mcp").expect("repo"),
            Revision::try_from("abc").expect("revision"),
            "docs/example.md",
            "text/markdown",
            "content",
        )
        .expect("document");
        assert_eq!(first.document_id, second.document_id);
    }

    #[test]
    fn source_span_rejects_reverse_range() {
        let error = SourceSpan::new(4, 3, None, None).expect_err("reverse range");
        assert_eq!(error, CorpusError::ReverseSourceSpan);
    }
}
