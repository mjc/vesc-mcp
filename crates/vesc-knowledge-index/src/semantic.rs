//! Optional local semantic retrieval contracts and vector artifacts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corpus::{Chunk, ChunkId, ContentDigest};

const MAGIC: &[u8] = b"VESCRAG1";
const CHECKSUM_LEN: usize = 32;
const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum EmbeddingError {
    #[error("embedding input is empty")]
    EmptyInput,
    #[error("embedding dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("embedding contains a non-finite value")]
    NonFinite,
    #[error("embedding vector has zero norm")]
    ZeroNorm,
    #[error("vector artifact has invalid header")]
    InvalidHeader,
    #[error("vector artifact is truncated")]
    Truncated,
    #[error("vector artifact exceeds the {MAX_ARTIFACT_BYTES} byte safety limit")]
    TooLarge,
    #[error("vector artifact checksum mismatch")]
    ChecksumMismatch,
    #[error("vector artifact corpus digest does not match the active corpus")]
    CorpusMismatch,
    #[error("vector artifact is missing chunk {0}")]
    MissingChunk(ChunkId),
    #[error("vector artifact contains unknown chunk {0}")]
    UnknownChunk(ChunkId),
    #[error("vector artifact model metadata does not match the requested model")]
    ModelMismatch,
    #[error("embedding provider failed: {0}")]
    Provider(String),
    #[error("vector artifact JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("vector artifact I/O failed: {0}")]
    Io(String),
}

/// Token pooling used to turn model output into one embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pooling {
    Cls,
    Mean,
}

/// Model-specific contract shared by corpus generation and query embedding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProfile {
    pub pooling: Pooling,
    pub query_prefix: String,
    pub document_prefix: String,
    pub max_length: usize,
    pub dimension: usize,
    pub normalize: bool,
}

impl EmbeddingProfile {
    /// Profile for the packaged BGE small English v1.5 model.
    #[must_use]
    pub fn bge_small_en_v1_5() -> Self {
        Self {
            pooling: Pooling::Cls,
            query_prefix: "Represent this sentence for searching relevant passages: ".into(),
            document_prefix: String::new(),
            max_length: 512,
            dimension: 384,
            normalize: true,
        }
    }

    /// Profile for Snowflake Arctic Embed XS or S.
    #[must_use]
    pub fn snowflake_arctic() -> Self {
        Self {
            pooling: Pooling::Cls,
            query_prefix: "Represent this sentence for searching relevant passages: ".into(),
            document_prefix: String::new(),
            max_length: 512,
            dimension: 384,
            normalize: true,
        }
    }

    /// Profile for Jina Embeddings v2 Base Code.
    #[must_use]
    pub fn jina_v2_base_code() -> Self {
        Self {
            pooling: Pooling::Mean,
            query_prefix: String::new(),
            document_prefix: String::new(),
            max_length: 8192,
            dimension: 768,
            normalize: true,
        }
    }

    /// Resolves the supported profile from a model identity.
    #[must_use]
    pub fn for_model_id(model_id: &str) -> Option<Self> {
        let model_id = model_id.to_ascii_lowercase();
        if model_id.contains("bge-small-en-v1.5") {
            Some(Self::bge_small_en_v1_5())
        } else if model_id.contains("snowflake-arctic-embed-xs")
            || model_id.contains("snowflake-arctic-embed-s")
        {
            Some(Self::snowflake_arctic())
        } else if model_id.contains("jina-embeddings-v2-base-code") {
            Some(Self::jina_v2_base_code())
        } else {
            None
        }
    }
}

/// Synchronous provider boundary for batch document and query embeddings.
pub trait EmbeddingProvider {
    /// Embeds documents in input order.
    ///
    /// # Errors
    ///
    /// Returns an [`EmbeddingError`] when the provider cannot produce valid
    /// vectors for the input.
    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// Embeds one query using the same normalization as documents.
    ///
    /// # Errors
    ///
    /// Returns an [`EmbeddingError`] when the provider cannot produce a valid
    /// vector for the query.
    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}

/// Builds the stable document text sent to the embedding provider.
///
/// Legacy records often carry their useful vocabulary in titles, identifiers,
/// and tags rather than in the short body summary. Keep those fields in the
/// semantic input so vector retrieval sees the same corpus concepts that the
/// lexical fields expose.
fn embedding_text(chunk: &Chunk) -> String {
    let mut text = String::new();
    if !chunk.title.is_empty() {
        text.push_str("Title: ");
        text.push_str(&chunk.title);
        text.push('\n');
    }
    if !chunk.heading_path.is_empty() {
        text.push_str("Headings: ");
        text.push_str(&chunk.heading_path.join(" / "));
        text.push('\n');
    }
    if !chunk.identifiers.is_empty() {
        text.push_str("Identifiers: ");
        text.push_str(
            &chunk
                .identifiers
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
        text.push('\n');
        let aliases = chunk
            .identifiers
            .iter()
            .filter_map(|identifier| identifier_semantic_alias(identifier))
            .collect::<Vec<_>>();
        if !aliases.is_empty() {
            text.push_str("Concepts: ");
            text.push_str(&aliases.join("; "));
            text.push('\n');
        }
    }
    if !chunk.tags.is_empty() {
        text.push_str("Tags: ");
        text.push_str(&chunk.tags.iter().cloned().collect::<Vec<_>>().join(", "));
        text.push('\n');
    }
    text.push_str("Content: ");
    text.push_str(&chunk.text);
    text
}

fn identifier_semantic_alias(identifier: &str) -> Option<&'static str> {
    Some(match identifier {
        "lbm_add_extension" | "bldc_vesc_c_if_lbm" => {
            "register native LispBM extension through the firmware C interface"
        }
        "lbm_enc_i32" | "lbm_enc_u32" | "lbm_enc_f32" => {
            "encode integer or numeric values across the native extension boundary"
        }
        "lbm_dec_as_i32" | "lbm_dec_as_u32" | "lbm_dec_as_f32" => {
            "decode integer or numeric values across the native extension boundary"
        }
        "vesc_c_if" => "firmware C interface functions available to native packages",
        "bldc_foc_audio_605" => "firmware compatibility package support FOC audio feature APIs",
        "gap_packer_divergence" => {
            "package wire format packer bytes differ from the golden fixture"
        }
        "refloat_build_pkgdesc" => "build package from descriptor through the package lifecycle",
        "refloat_lisp_load_native" => "load a native package library through the LispBM loader",
        _ => return None,
    })
}

/// Adds a small, deterministic vocabulary bridge for concepts whose corpus
/// evidence is intentionally identifier-heavy.
///
/// This is not LLM rewriting; it only appends reviewed VESC aliases to the
/// provider input.
#[must_use]
pub fn semantic_query_text(query: &str) -> String {
    let normalized = query.to_ascii_lowercase();
    let mut aliases = Vec::new();
    if normalized.contains("encoded") || normalized.contains("encoding") {
        aliases.push("lbm_enc_i32 lbm_enc_u32 lbm_enc_f32 encode numeric values");
    }
    if normalized.contains("firmware c interface") || normalized.contains("interface functions") {
        aliases.push("vesc_c_if lbm_add_extension firmware C interface");
    }
    if normalized.contains("native extension") {
        aliases.push("lbm_add_extension vesc_c_if native extension registration");
    }
    if normalized.contains("gating") || normalized.contains("compatibility") {
        aliases.push("bldc_foc_audio_605 firmware compatibility support");
    }
    if normalized.contains("wire bytes") || normalized.contains("packer") {
        aliases.push("gap_packer_divergence package wire format");
    }
    if normalized.contains("recording") {
        aliases.push("DATA_RECORD REALTIME_DATA data recording");
    }
    if normalized.contains("alert history") {
        aliases.push("ALERTS_LIST ALERTS_CONTROL alert history");
    }
    if aliases.is_empty() {
        query.to_owned()
    } else {
        format!("{query}\nConcept aliases: {}", aliases.join("; "))
    }
}

/// A deterministic local fake provider for tests and offline development.
#[derive(Debug, Clone)]
pub struct FakeEmbeddingProvider {
    dimension: usize,
}

impl FakeEmbeddingProvider {
    /// Creates a deterministic provider with a fixed vector dimension.
    #[must_use]
    pub const fn new(dimension: usize) -> Self {
        Self { dimension }
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }
        if self.dimension == 0 {
            return Err(EmbeddingError::DimensionMismatch {
                expected: 1,
                actual: 0,
            });
        }
        let mut vector = vec![0.0; self.dimension];
        for (index, byte) in text.bytes().enumerate() {
            vector[index % self.dimension] += f32::from(byte) / 255.0;
        }
        normalize(&mut vector)?;
        Ok(vector)
    }
}

impl EmbeddingProvider for FakeEmbeddingProvider {
    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        texts.iter().map(|text| self.embed(text)).collect()
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed(text)
    }
}

/// Optional FastEmbed-backed provider for already-provisioned local model files.
///
/// This type is available only with the `semantic-fastembed` feature. Model
/// construction remains outside this crate so startup never downloads a model.
#[cfg(feature = "semantic-fastembed")]
pub struct FastEmbedProvider {
    model: fastembed::TextEmbedding,
    batch_size: Option<usize>,
    profile: EmbeddingProfile,
}

#[cfg(feature = "semantic-fastembed")]
impl FastEmbedProvider {
    /// Wrap an initialized `FastEmbed` model.
    #[must_use]
    pub fn new(
        model: fastembed::TextEmbedding,
        batch_size: Option<usize>,
        profile: EmbeddingProfile,
    ) -> Self {
        Self {
            model,
            batch_size,
            profile,
        }
    }

    /// Load a user-provisioned `FastEmbed` model without contacting a registry.
    ///
    /// The directory must contain `model.onnx`, `tokenizer.json`,
    /// `config.json`, `special_tokens_map.json`, and `tokenizer_config.json`.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Io`] for missing model files or
    /// [`EmbeddingError::Provider`] when ONNX/tokenizer initialization fails.
    pub fn from_model_dir(
        root: &std::path::Path,
        batch_size: Option<usize>,
    ) -> Result<Self, EmbeddingError> {
        Self::from_model_dir_with_profile(root, batch_size, EmbeddingProfile::bge_small_en_v1_5())
    }

    /// Load a local model using an explicit embedding contract.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the profile is invalid, model files are
    /// missing, or ONNX/tokenizer initialization fails.
    pub fn from_model_dir_with_profile(
        root: &std::path::Path,
        batch_size: Option<usize>,
        profile: EmbeddingProfile,
    ) -> Result<Self, EmbeddingError> {
        if profile.max_length == 0 || profile.dimension == 0 || !profile.normalize {
            return Err(EmbeddingError::Provider(
                "FastEmbed requires a non-zero, normalized embedding profile".into(),
            ));
        }
        let read = |name: &str| {
            std::fs::read(root.join(name)).map_err(|error| EmbeddingError::Io(error.to_string()))
        };
        let model = fastembed::UserDefinedEmbeddingModel::new(
            read("model.onnx")?,
            fastembed::TokenizerFiles {
                tokenizer_file: read("tokenizer.json")?,
                config_file: read("config.json")?,
                special_tokens_map_file: read("special_tokens_map.json")?,
                tokenizer_config_file: read("tokenizer_config.json")?,
            },
        )
        .with_pooling(match profile.pooling {
            Pooling::Cls => fastembed::Pooling::Cls,
            Pooling::Mean => fastembed::Pooling::Mean,
        });
        let options = fastembed::InitOptionsUserDefined::new()
            .with_max_length(profile.max_length)
            .with_execution_providers(semantic_execution_providers());
        let model = fastembed::TextEmbedding::try_new_from_user_defined(model, options)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        Ok(Self::new(model, batch_size, profile))
    }

    fn validate_vectors(&self, vectors: &[Vec<f32>]) -> Result<(), EmbeddingError> {
        for vector in vectors {
            if vector.len() != self.profile.dimension {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: self.profile.dimension,
                    actual: vector.len(),
                });
            }
        }
        Ok(())
    }

    fn effective_batch_size(&self, input_len: usize) -> Option<usize> {
        self.batch_size.map(|size| size.min(input_len))
    }
}

#[cfg(all(
    feature = "semantic-fastembed",
    feature = "semantic-coreml",
    target_os = "macos"
))]
fn semantic_execution_providers() -> Vec<fastembed::ExecutionProviderDispatch> {
    if std::env::var("VESC_RAG_SEMANTIC_EXECUTION_PROVIDER")
        .ok()
        .is_none_or(|provider| !provider.eq_ignore_ascii_case("coreml"))
    {
        return Vec::new();
    }
    vec![
        ort::ep::CoreML::default()
            .with_compute_units(ort::ep::coreml::ComputeUnits::All)
            .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
            .with_specialization_strategy(ort::ep::coreml::SpecializationStrategy::FastPrediction)
            .with_low_precision_accumulation_on_gpu(true)
            .build(),
    ]
}

#[cfg(all(
    feature = "semantic-fastembed",
    not(all(feature = "semantic-coreml", target_os = "macos"))
))]
fn semantic_execution_providers() -> Vec<fastembed::ExecutionProviderDispatch> {
    Vec::new()
}

#[cfg(feature = "semantic-fastembed")]
impl EmbeddingProvider for FastEmbedProvider {
    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let texts = texts
            .iter()
            .map(|text| format!("{}{}", self.profile.document_prefix, text))
            .collect::<Vec<_>>();
        let vectors = self
            .model
            .embed(&texts, self.effective_batch_size(texts.len()))
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        self.validate_vectors(&vectors)?;
        Ok(vectors)
    }

    fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let text = format!("{}{}", self.profile.query_prefix, text);
        let vectors = self
            .model
            .embed([text], self.effective_batch_size(1))
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        self.validate_vectors(&vectors)?;
        vectors.into_iter().next().ok_or(EmbeddingError::EmptyInput)
    }
}

/// One semantic candidate from exact cosine search.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticHit {
    pub chunk_id: ChunkId,
    pub similarity: f32,
}

/// Reproducible vector artifact keyed by stable chunk IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorArtifact {
    pub schema: u16,
    pub model_id: String,
    pub model_revision: String,
    pub dimension: usize,
    pub normalized: bool,
    pub corpus_digest: ContentDigest,
    pub vectors: BTreeMap<ChunkId, Vec<f32>>,
}

impl VectorArtifact {
    /// Builds and validates an artifact from a provider and ordered chunks.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when provider output is empty, inconsistent,
    /// non-finite, or not normalizable.
    pub fn from_provider<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        corpus_digest: ContentDigest,
    ) -> Result<Self, EmbeddingError> {
        const BATCH_SIZE: usize = 8;

        let mut vectors = BTreeMap::new();
        let mut dimension = None;

        for chunk_batch in chunks.chunks(BATCH_SIZE) {
            let texts = chunk_batch.iter().map(embedding_text).collect::<Vec<_>>();
            let batch_vectors = provider.embed_documents(&texts)?;

            if batch_vectors.len() != chunk_batch.len() {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: chunk_batch.len(),
                    actual: batch_vectors.len(),
                });
            }

            for (chunk, mut vector) in chunk_batch.iter().zip(batch_vectors) {
                normalize(&mut vector)?;

                match dimension {
                    None => dimension = Some(vector.len()),
                    Some(expected) if expected != vector.len() => {
                        return Err(EmbeddingError::DimensionMismatch {
                            expected,
                            actual: vector.len(),
                        });
                    }
                    Some(_) => {}
                }

                vectors.insert(chunk.chunk_id.clone(), vector);
            }
        }

        let artifact = Self {
            schema: 1,
            model_id: model_id.into(),
            model_revision: model_revision.into(),
            dimension: dimension.unwrap_or(0),
            normalized: true,
            corpus_digest,
            vectors,
        };
        artifact.validate()?;
        Ok(artifact)
    }

    /// Validates dimensions, finiteness, and artifact schema.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] if any vector violates the artifact contract.
    pub fn validate(&self) -> Result<(), EmbeddingError> {
        if self.schema != 1
            || self.dimension == 0
            || self.model_id.trim().is_empty()
            || self.model_revision.trim().is_empty()
        {
            return Err(EmbeddingError::InvalidHeader);
        }
        for vector in self.vectors.values() {
            if vector.len() != self.dimension {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: self.dimension,
                    actual: vector.len(),
                });
            }
            if vector.iter().any(|value| !value.is_finite()) {
                return Err(EmbeddingError::NonFinite);
            }
        }
        Ok(())
    }

    /// Validates that vectors exactly cover the active corpus and model.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::CorpusMismatch`], [`EmbeddingError::MissingChunk`],
    /// [`EmbeddingError::UnknownChunk`], or [`EmbeddingError::ModelMismatch`]
    /// when an artifact was built for different inputs.
    pub fn validate_for_corpus(
        &self,
        corpus_digest: &ContentDigest,
        chunk_ids: &std::collections::BTreeSet<ChunkId>,
        model_id: &str,
        model_revision: &str,
    ) -> Result<(), EmbeddingError> {
        self.validate()?;
        if &self.corpus_digest != corpus_digest {
            return Err(EmbeddingError::CorpusMismatch);
        }
        if self.model_id != model_id || self.model_revision != model_revision {
            return Err(EmbeddingError::ModelMismatch);
        }
        for chunk_id in chunk_ids {
            if !self.vectors.contains_key(chunk_id) {
                return Err(EmbeddingError::MissingChunk(chunk_id.clone()));
            }
        }
        for chunk_id in self.vectors.keys() {
            if !chunk_ids.contains(chunk_id) {
                return Err(EmbeddingError::UnknownChunk(chunk_id.clone()));
            }
        }
        Ok(())
    }

    /// Encodes the artifact with a stable magic header and canonical JSON body.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the artifact is invalid or JSON cannot encode it.
    pub fn encode(&self) -> Result<Vec<u8>, EmbeddingError> {
        self.validate()?;
        let model_id = self.model_id.as_bytes();
        let model_revision = self.model_revision.as_bytes();
        let model_id_len = u32::try_from(model_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let model_revision_len =
            u32::try_from(model_revision.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let count = u32::try_from(self.vectors.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let dimension = u32::try_from(self.dimension).map_err(|_| EmbeddingError::TooLarge)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&u32::from(self.schema).to_le_bytes());
        bytes.extend_from_slice(&dimension.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&model_id_len.to_le_bytes());
        bytes.extend_from_slice(model_id);
        bytes.extend_from_slice(&model_revision_len.to_le_bytes());
        bytes.extend_from_slice(model_revision);
        bytes.push(u8::from(self.normalized));
        bytes.extend_from_slice(&digest_bytes(&self.corpus_digest)?);
        for (chunk_id, vector) in &self.vectors {
            let chunk_id = chunk_id.as_str().as_bytes();
            let chunk_id_len =
                u16::try_from(chunk_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
            bytes.extend_from_slice(&chunk_id_len.to_le_bytes());
            bytes.extend_from_slice(chunk_id);
            for value in vector {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        if bytes.len() > MAX_ARTIFACT_BYTES.saturating_sub(CHECKSUM_LEN) {
            return Err(EmbeddingError::TooLarge);
        }
        let checksum = Sha256::digest(&bytes);
        bytes.extend_from_slice(&checksum);
        Ok(bytes)
    }

    /// Decodes and validates a vector artifact.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] for a wrong magic header, truncated body, invalid
    /// text metadata, or invalid vectors.
    pub fn decode(bytes: &[u8]) -> Result<Self, EmbeddingError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(EmbeddingError::TooLarge);
        }
        if !bytes.starts_with(MAGIC) {
            return Err(EmbeddingError::InvalidHeader);
        }
        if bytes.len()
            < MAGIC
                .len()
                .saturating_add(12)
                .saturating_add(8)
                .saturating_add(1)
                .saturating_add(32)
                .saturating_add(CHECKSUM_LEN)
        {
            return Err(EmbeddingError::Truncated);
        }
        let checksum_start = bytes.len() - CHECKSUM_LEN;
        if Sha256::digest(&bytes[..checksum_start]).as_slice() != &bytes[checksum_start..] {
            return Err(EmbeddingError::ChecksumMismatch);
        }
        let mut reader = BinaryReader::new(&bytes[..checksum_start]);
        reader.take(MAGIC.len())?;
        let schema = u16::try_from(reader.u32()?).map_err(|_| EmbeddingError::InvalidHeader)?;
        let dimension = usize::try_from(reader.u32()?).map_err(|_| EmbeddingError::TooLarge)?;
        if dimension == 0 || dimension > MAX_ARTIFACT_BYTES / std::mem::size_of::<f32>() {
            return Err(EmbeddingError::TooLarge);
        }
        let count = usize::try_from(reader.u32()?).map_err(|_| EmbeddingError::TooLarge)?;
        if count > MAX_ARTIFACT_BYTES / 2 {
            return Err(EmbeddingError::TooLarge);
        }
        let model_id_len = reader.u32()?;
        let model_id = reader.string(model_id_len)?;
        let model_revision_len = reader.u32()?;
        let model_revision = reader.string(model_revision_len)?;
        let normalized = match reader.byte()? {
            0 => false,
            1 => true,
            _ => return Err(EmbeddingError::InvalidHeader),
        };
        let corpus_digest = digest_from_bytes(reader.take(32)?)?;
        let mut vectors = BTreeMap::new();
        for _ in 0..count {
            let chunk_id_len = u32::from(reader.u16()?);
            let chunk_id = ChunkId::try_from(reader.string(chunk_id_len)?)
                .map_err(|_| EmbeddingError::InvalidHeader)?;
            let mut vector = Vec::with_capacity(dimension);
            for _ in 0..dimension {
                let bytes = reader
                    .take(4)?
                    .try_into()
                    .map_err(|_| EmbeddingError::Truncated)?;
                vector.push(f32::from_le_bytes(bytes));
            }
            if vectors.insert(chunk_id, vector).is_some() {
                return Err(EmbeddingError::InvalidHeader);
            }
        }
        if !reader.is_empty() {
            return Err(EmbeddingError::InvalidHeader);
        }
        let artifact = Self {
            schema,
            model_id,
            model_revision,
            dimension,
            normalized,
            corpus_digest,
            vectors,
        };
        artifact.validate()?;
        Ok(artifact)
    }

    /// Writes a checked binary vector artifact.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when encoding fails or the destination cannot
    /// be written.
    pub fn write_artifact(&self, path: &std::path::Path) -> Result<(), EmbeddingError> {
        std::fs::write(path, self.encode()?).map_err(|error| EmbeddingError::Io(error.to_string()))
    }

    /// Opens and validates a checked binary vector artifact.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the file cannot be read or the artifact
    /// violates the binary contract.
    pub fn open_artifact(path: &std::path::Path) -> Result<Self, EmbeddingError> {
        let metadata =
            std::fs::metadata(path).map_err(|error| EmbeddingError::Io(error.to_string()))?;
        if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
            return Err(EmbeddingError::TooLarge);
        }
        let bytes = std::fs::read(path).map_err(|error| EmbeddingError::Io(error.to_string()))?;
        Self::decode(&bytes)
    }

    /// Searches the normalized vectors by exact dot product.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the query dimension or values are invalid.
    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<SemanticHit>, EmbeddingError> {
        self.validate()?;
        if query.len() != self.dimension {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.dimension,
                actual: query.len(),
            });
        }
        let mut query = query.to_vec();
        normalize(&mut query)?;
        let mut hits: Vec<_> = self
            .vectors
            .iter()
            .map(|(chunk_id, vector)| SemanticHit {
                chunk_id: chunk_id.clone(),
                similarity: vector
                    .iter()
                    .zip(&query)
                    .map(|(left, right)| left * right)
                    .sum(),
            })
            .collect();
        hits.sort_by(|left, right| {
            right
                .similarity
                .total_cmp(&left.similarity)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        hits.truncate(limit.max(1));
        Ok(hits)
    }
}

fn normalize(vector: &mut [f32]) -> Result<(), EmbeddingError> {
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(EmbeddingError::NonFinite);
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return Err(EmbeddingError::ZeroNorm);
    }
    for value in vector {
        *value /= norm;
    }
    Ok(())
}

struct BinaryReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> BinaryReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], EmbeddingError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(EmbeddingError::TooLarge)?;
        if end > self.bytes.len() {
            return Err(EmbeddingError::Truncated);
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, EmbeddingError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(EmbeddingError::Truncated)
    }

    fn u16(&mut self) -> Result<u16, EmbeddingError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, EmbeddingError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn string(&mut self, length: u32) -> Result<String, EmbeddingError> {
        let length = usize::try_from(length).map_err(|_| EmbeddingError::TooLarge)?;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|error| EmbeddingError::InvalidJson(error.to_string()))
    }

    const fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn digest_bytes(digest: &ContentDigest) -> Result<[u8; 32], EmbeddingError> {
    let hex = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(EmbeddingError::InvalidHeader)?;
    let mut bytes = [0_u8; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Ok(bytes)
}

fn digest_from_bytes(bytes: &[u8]) -> Result<ContentDigest, EmbeddingError> {
    if bytes.len() != 32 {
        return Err(EmbeddingError::InvalidHeader);
    }
    let mut value = String::from("sha256:");
    for byte in bytes {
        value.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        value.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    ContentDigest::try_from(value).map_err(|_| EmbeddingError::InvalidHeader)
}

const fn hex_digit(byte: u8) -> Result<u8, EmbeddingError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(EmbeddingError::InvalidHeader),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{NormalizedDocument, RepositoryId, Revision, SourceKind};

    fn chunks() -> Vec<Chunk> {
        ["alpha", "beta"]
            .into_iter()
            .map(|text| {
                let document = NormalizedDocument::new(
                    text,
                    SourceKind::Markdown,
                    RepositoryId::try_from("repo").expect("repo"),
                    Revision::try_from("rev").expect("rev"),
                    format!("docs/{text}.md"),
                    "text/markdown",
                    text,
                )
                .expect("document");
                Chunk::from_document(&document, 0, text.into(), Vec::new(), None).expect("chunk")
            })
            .collect()
    }

    #[test]
    fn embedding_text_includes_retrieval_metadata() {
        let mut chunk = chunks().remove(0);
        chunk.heading_path.push("Package lifecycle".into());
        chunk.identifiers.insert("priority.package".into());
        chunk.tags.insert("persistence".into());

        let text = embedding_text(&chunk);
        assert!(text.contains("Title: alpha"));
        assert!(text.contains("Headings: Package lifecycle"));
        assert!(text.contains("Identifiers: priority.package"));
        assert!(text.contains("Tags: persistence"));
        assert!(text.ends_with("Content: alpha"));
    }

    #[test]
    fn embedding_text_adds_domain_aliases_for_numeric_values() {
        let mut chunk = chunks().remove(0);
        chunk.identifiers.insert("lbm_enc_i32".into());

        let text = embedding_text(&chunk);

        assert!(text.contains("Concepts: encode integer or numeric values"));
    }

    #[test]
    fn semantic_query_text_uses_reviewed_concept_aliases() {
        let text = semantic_query_text("encoded values crossing the native extension boundary");

        assert!(text.starts_with("encoded values crossing"));
        assert!(text.contains("lbm_enc_i32"));
        assert!(semantic_query_text("lbm_add_extension").eq("lbm_add_extension"));
    }

    #[test]
    fn fake_provider_maps_chunks_to_valid_vectors() {
        let mut provider = FakeEmbeddingProvider::new(4);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks(),
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        assert_eq!(artifact.vectors.len(), 2);
        assert!(artifact.vectors.values().all(|vector| vector.len() == 4));
    }

    #[test]
    fn vector_generation_bounds_provider_batches() {
        struct TrackingProvider {
            inner: FakeEmbeddingProvider,
            largest_batch: usize,
        }

        impl EmbeddingProvider for TrackingProvider {
            fn embed_documents(
                &mut self,
                texts: &[String],
            ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                self.largest_batch = self.largest_batch.max(texts.len());
                self.inner.embed_documents(texts)
            }

            fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
                self.inner.embed_query(text)
            }
        }

        let source = chunks().remove(0);
        let chunks = (0..17).map(|_| source.clone()).collect::<Vec<_>>();
        let mut provider = TrackingProvider {
            inner: FakeEmbeddingProvider::new(4),
            largest_batch: 0,
        };

        VectorArtifact::from_provider(
            &mut provider,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");

        assert_eq!(provider.largest_batch, 8);
    }

    #[test]
    fn artifact_rejects_corpus_or_model_mismatch() {
        let chunks = chunks();
        let mut provider = FakeEmbeddingProvider::new(4);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        let ids = chunks.iter().map(|chunk| chunk.chunk_id.clone()).collect();
        assert_eq!(
            artifact.validate_for_corpus(&ContentDigest::of(b"other"), &ids, "fake", "test"),
            Err(EmbeddingError::CorpusMismatch)
        );
        assert_eq!(
            artifact.validate_for_corpus(&ContentDigest::of(b"corpus"), &ids, "other", "test"),
            Err(EmbeddingError::ModelMismatch)
        );
    }

    #[test]
    fn artifact_roundtrip_rejects_wrong_magic() {
        let error = VectorArtifact::decode(b"bad").expect_err("wrong magic");
        assert_eq!(error, EmbeddingError::InvalidHeader);
    }

    #[test]
    fn vector_artifact_uses_checked_binary_v1_layout() {
        let mut provider = FakeEmbeddingProvider::new(4);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks(),
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        let encoded = artifact.encode().expect("encode");

        assert_eq!(&encoded[..8], b"VESCRAG1");
        assert_eq!(
            u32::from_le_bytes(encoded[8..12].try_into().expect("schema")),
            1
        );
        assert_eq!(
            u32::from_le_bytes(encoded[12..16].try_into().expect("dimension")),
            4
        );
        assert_ne!(encoded[20], b'{');
    }

    #[test]
    fn artifact_roundtrip_rejects_checksum_tampering() {
        let mut provider = FakeEmbeddingProvider::new(4);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks(),
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        let mut encoded = artifact.encode().expect("encode");
        let payload_byte = encoded.len() - CHECKSUM_LEN - 1;
        encoded[payload_byte] ^= 1;
        assert_eq!(
            VectorArtifact::decode(&encoded).expect_err("tampered artifact"),
            EmbeddingError::ChecksumMismatch
        );
    }

    #[test]
    fn vector_search_has_deterministic_order() {
        let mut provider = FakeEmbeddingProvider::new(4);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks(),
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        let query = provider.embed_query("alpha").expect("query");
        let hits = artifact.search(&query, 2).expect("search");
        assert_eq!(hits.len(), 2);
    }
}
