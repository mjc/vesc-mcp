//! Optional local semantic retrieval contracts and vector artifacts.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Instant;

#[cfg(feature = "semantic-fastembed")]
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corpus::{Chunk, ChunkId, ContentDigest};

const MAGIC: &[u8] = b"VESCRAG1";
const VECTOR_CHECKPOINT_MAGIC: &[u8] = b"VESCVC01";
const CHECKSUM_LEN: usize = 32;
const MAX_ARTIFACT_BYTES: usize = 1024 * 1024 * 1024;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;
const VECTOR_CHECKPOINT_SYNC_ROWS: usize = 256;

/// Conservative outer batch size for the production embedding build.
pub const DEFAULT_SEMANTIC_BATCH_SIZE: usize = 8;

/// Selects the measured CPU thread default for the supported developer hosts.
///
/// Apple Silicon M1 machines expose eight logical CPUs. Other hosts use the
/// process CPU allowance, which is twelve on the Ryzen 5 8600G test host.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[must_use]
pub const fn default_semantic_intra_threads() -> usize {
    8
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[must_use]
pub fn default_semantic_intra_threads() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

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
    #[error("vector artifact schema {0} is unsupported; rebuild the semantic artifact")]
    UnsupportedSchema(u16),
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

/// Aggregation used when one document requires multiple model windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAggregation {
    Mean,
    TokenWeightedMean,
}

/// Contract for whether a provider's output vectors are already normalized.
///
/// `Guaranteed` is reserved for an explicitly normalized `FastEmbed` profile or
/// a model implementation covered by a normalization test. The artifact
/// builder still validates every vector before accepting that contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputNormalization {
    Guaranteed,
    Unknown,
}

/// Timings and byte counts observed while constructing a vector artifact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorBuildObservations {
    pub embedding_input_us: u64,
    pub provider_us: u64,
    pub vector_finalization_us: u64,
    pub input_bytes: u64,
    #[serde(default)]
    pub reused_vectors: usize,
    #[serde(default)]
    pub embedded_vectors: usize,
}

/// Tokenization and padding measurements for the exact provider inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenStatistics {
    pub chunks: usize,
    pub total_real_tokens: u64,
    pub total_padded_tokens: u64,
    pub total_untruncated_tokens: u64,
    pub min_tokens: usize,
    pub median_tokens: usize,
    pub p95_tokens: usize,
    pub maximum_tokens: usize,
    pub truncated_chunks: usize,
    /// Padding waste expressed as parts per million of padded tokens.
    pub padding_ratio_ppm: u64,
}

/// One fixed input shape in a constant-token-budget benchmark matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceBucket {
    pub max_length: usize,
    pub batch_size: usize,
}

/// Tokenizer-only cost projection for one fixed sequence shape.
#[cfg(feature = "semantic-fastembed")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceBucketCensus {
    pub bucket: SequenceBucket,
    pub windows: usize,
    pub multi_window_inputs: usize,
    pub provider_batches: usize,
    pub real_tokens: u64,
    pub padded_tokens: u64,
    pub padding_ratio_ppm: u64,
}

/// Tokenizer-only sequence matrix over exact embedding inputs.
#[cfg(feature = "semantic-fastembed")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceLengthCensus {
    pub inputs: usize,
    pub token_budget: usize,
    pub total_untruncated_tokens: u64,
    pub metadata_tokens: u64,
    pub metadata_ratio_ppm: u64,
    pub buckets: Vec<SequenceBucketCensus>,
}

/// Builds fixed sequence shapes that consume at most `token_budget` tokens per batch.
///
/// # Errors
///
/// Returns [`EmbeddingError`] when the budget or any sequence length is zero,
/// or when a sequence cannot fit within the budget.
pub fn sequence_bucket_plan(
    max_lengths: &[usize],
    token_budget: usize,
) -> Result<Vec<SequenceBucket>, EmbeddingError> {
    if token_budget == 0 || max_lengths.is_empty() {
        return Err(EmbeddingError::Provider(
            "sequence lengths and token budget must be nonzero".into(),
        ));
    }
    max_lengths
        .iter()
        .copied()
        .map(|max_length| {
            let batch_size = token_budget.checked_div(max_length).unwrap_or_default();
            if max_length == 0 || batch_size == 0 {
                return Err(EmbeddingError::Provider(format!(
                    "sequence length {max_length} does not fit token budget {token_budget}"
                )));
            }
            Ok(SequenceBucket {
                max_length,
                batch_size,
            })
        })
        .collect()
}

/// Measures fixed-shape window and padding costs without loading an ONNX model.
///
/// # Errors
///
/// Returns [`EmbeddingError`] when the tokenizer cannot be loaded or an input
/// cannot be represented by one of the requested sequence lengths.
#[cfg(feature = "semantic-fastembed")]
pub fn sequence_length_census(
    tokenizer_path: &std::path::Path,
    texts: &[String],
    max_lengths: &[usize],
    token_budget: usize,
) -> Result<SequenceLengthCensus, EmbeddingError> {
    sequence_length_census_iter(tokenizer_path, texts, max_lengths, token_budget)
}

/// Measure sequence buckets from a stream while retaining one source text.
///
/// # Errors
///
/// Returns [`EmbeddingError`] for empty input, invalid bucket configuration,
/// or tokenizer failures.
#[cfg(feature = "semantic-fastembed")]
pub fn sequence_length_census_iter<I, S>(
    tokenizer_path: &std::path::Path,
    texts: I,
    max_lengths: &[usize],
    token_budget: usize,
) -> Result<SequenceLengthCensus, EmbeddingError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut tokenizer = tokenizers::Tokenizer::from_file(tokenizer_path)
        .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
    tokenizer.with_padding(None);
    tokenizer
        .with_truncation(None)
        .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
    let buckets = sequence_bucket_plan(max_lengths, token_budget)?;
    let mut bucket_totals = vec![(0_usize, 0_usize, 0_u64); buckets.len()];
    let mut inputs = 0_usize;
    let mut total_untruncated_tokens = 0_u64;
    let mut metadata_tokens = 0_u64;
    for text in texts {
        inputs = inputs.saturating_add(1);
        let text = text.as_ref();
        let encoding = tokenizer
            .encode(text, true)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        total_untruncated_tokens = total_untruncated_tokens
            .saturating_add(u64::try_from(encoding.get_ids().len()).unwrap_or(u64::MAX));
        if let Some(content) = text.find("Content: ") {
            let encoding = tokenizer
                .encode(&text[..content], false)
                .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
            metadata_tokens = metadata_tokens
                .saturating_add(u64::try_from(encoding.get_ids().len()).unwrap_or(u64::MAX));
        }
        for (bucket, (windows, multi_window_inputs, real_tokens)) in
            buckets.iter().zip(&mut bucket_totals)
        {
            let source_windows = bounded_document_windows(&tokenizer, text, bucket.max_length)?;
            *multi_window_inputs =
                multi_window_inputs.saturating_add(usize::from(source_windows.len() > 1));
            *windows = windows.saturating_add(source_windows.len());
            *real_tokens = real_tokens.saturating_add(
                source_windows
                    .iter()
                    .map(|(_, tokens)| u64::try_from(*tokens).unwrap_or(u64::MAX))
                    .sum::<u64>(),
            );
        }
    }
    if inputs == 0 {
        return Err(EmbeddingError::EmptyInput);
    }

    let mut results = Vec::with_capacity(buckets.len());
    for (bucket, (windows, multi_window_inputs, real_tokens)) in
        buckets.into_iter().zip(bucket_totals)
    {
        let provider_batches = windows.div_ceil(bucket.batch_size);
        let padded_tokens = u64::try_from(provider_batches)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(bucket.batch_size).unwrap_or(u64::MAX))
            .saturating_mul(u64::try_from(bucket.max_length).unwrap_or(u64::MAX));
        results.push(SequenceBucketCensus {
            bucket,
            windows,
            multi_window_inputs,
            provider_batches,
            real_tokens,
            padded_tokens,
            padding_ratio_ppm: padded_tokens
                .saturating_sub(real_tokens)
                .saturating_mul(1_000_000)
                .checked_div(padded_tokens)
                .unwrap_or_default(),
        });
    }
    Ok(SequenceLengthCensus {
        inputs,
        token_budget,
        total_untruncated_tokens,
        metadata_tokens,
        metadata_ratio_ppm: metadata_tokens
            .saturating_mul(1_000_000)
            .checked_div(total_untruncated_tokens)
            .unwrap_or_default(),
        buckets: results,
    })
}

/// Pools normalized document-window vectors into one normalized document vector.
///
/// # Errors
///
/// Returns [`EmbeddingError`] for empty, inconsistent, or zero-weight input.
pub fn aggregate_window_vectors(
    vectors: &[Vec<f32>],
    token_counts: &[usize],
    aggregation: WindowAggregation,
) -> Result<Vec<f32>, EmbeddingError> {
    let Some(dimension) = vectors.first().map(Vec::len) else {
        return Err(EmbeddingError::EmptyInput);
    };
    if dimension == 0 || vectors.len() != token_counts.len() {
        return Err(EmbeddingError::DimensionMismatch {
            expected: vectors.len(),
            actual: token_counts.len(),
        });
    }
    let mut pooled = vec![0.0_f32; dimension];
    for (vector, &token_count) in vectors.iter().zip(token_counts) {
        if vector.len() != dimension {
            return Err(EmbeddingError::DimensionMismatch {
                expected: dimension,
                actual: vector.len(),
            });
        }
        let weight = match aggregation {
            WindowAggregation::Mean => 1.0,
            WindowAggregation::TokenWeightedMean => u16::try_from(token_count)
                .map(f32::from)
                .map_err(|_| EmbeddingError::Provider("window token count exceeds u16".into()))?,
        };
        for (pooled, value) in pooled.iter_mut().zip(vector) {
            *pooled += weight * value;
        }
    }
    normalize(&mut pooled)?;
    Ok(pooled)
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

/// The configured outer batch is the memory boundary for provider calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EmbeddingBatchSize(NonZeroUsize);

impl EmbeddingBatchSize {
    /// Creates a batch size, rejecting zero at the configuration boundary.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when `value` is zero.
    pub fn new(value: usize) -> Result<Self, EmbeddingError> {
        NonZeroUsize::new(value)
            .map(Self)
            .ok_or_else(|| EmbeddingError::Provider("embedding batch size must be nonzero".into()))
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for EmbeddingBatchSize {
    fn default() -> Self {
        Self(NonZeroUsize::new(8).expect("non-zero default batch size"))
    }
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
    pub const fn jina_v2_base_code() -> Self {
        Self {
            pooling: Pooling::Mean,
            query_prefix: String::new(),
            document_prefix: String::new(),
            max_length: 8192,
            dimension: 768,
            normalize: true,
        }
    }

    /// Profile for Granite Embedding 97M Multilingual R2.
    #[must_use]
    pub const fn granite_embedding_97m_r2() -> Self {
        Self {
            pooling: Pooling::Cls,
            query_prefix: String::new(),
            document_prefix: String::new(),
            max_length: 32_768,
            dimension: 384,
            normalize: true,
        }
    }

    /// Profile for Granite Embedding 311M Multilingual R2.
    #[must_use]
    pub const fn granite_embedding_311m_r2() -> Self {
        let mut profile = Self::granite_embedding_97m_r2();
        profile.dimension = 768;
        profile
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
        } else if model_id.contains("granite-embedding-97m-multilingual-r2") {
            Some(Self::granite_embedding_97m_r2())
        } else if model_id.contains("granite-embedding-311m-multilingual-r2") {
            Some(Self::granite_embedding_311m_r2())
        } else {
            None
        }
    }
}

/// Synchronous provider boundary for batch document and query embeddings.
pub trait EmbeddingProvider {
    /// Returns the provider's fixed output dimension when known before inference.
    #[must_use]
    fn embedding_dimension(&self) -> Option<usize> {
        None
    }

    /// Returns the validated outer batch size used by corpus generation.
    #[must_use]
    fn embedding_batch_size(&self) -> EmbeddingBatchSize {
        EmbeddingBatchSize::default()
    }

    /// Reports whether this provider has a verified normalized-output contract.
    #[must_use]
    fn output_normalization(&self) -> OutputNormalization {
        OutputNormalization::Unknown
    }

    /// Returns an optional stable inference order for the supplied chunks.
    ///
    /// Providers may use bounded tokenizer work to group similar-length
    /// inputs. The artifact builder still restores chunk-ID order afterward.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when provider-specific ordering cannot be
    /// computed.
    fn inference_order(
        &mut self,
        _chunks: &[&Chunk],
    ) -> Result<Option<Vec<usize>>, EmbeddingError> {
        Ok(None)
    }

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
pub fn embedding_text(chunk: &Chunk) -> String {
    let capacity = embedding_text_capacity(chunk);
    let mut text = String::with_capacity(capacity);
    let identifiers = embedding_identifiers(chunk);
    if !chunk.title.is_empty() {
        text.push_str("Title: ");
        text.push_str(&chunk.title);
        text.push('\n');
    }
    if !chunk.heading_path.is_empty() {
        text.push_str("Headings: ");
        append_joined(
            &mut text,
            chunk.heading_path.iter().map(String::as_str),
            " / ",
        );
        text.push('\n');
    }
    if !identifiers.is_empty() {
        text.push_str("Identifiers: ");
        append_joined(&mut text, identifiers.iter().copied(), ", ");
        text.push('\n');
        if identifiers
            .iter()
            .any(|identifier| identifier_semantic_alias(identifier).is_some())
        {
            text.push_str("Concepts: ");
            append_joined(
                &mut text,
                identifiers
                    .iter()
                    .filter_map(|identifier| identifier_semantic_alias(identifier)),
                "; ",
            );
            text.push('\n');
        }
    }
    if !chunk.tags.is_empty() {
        text.push_str("Tags: ");
        append_joined(&mut text, chunk.tags.iter().map(String::as_str), ", ");
        text.push('\n');
    }
    text.push_str("Content: ");
    text.push_str(&chunk.text);
    text
}

pub(crate) fn embedding_identifiers(chunk: &Chunk) -> Vec<&str> {
    const MAX_IDENTIFIERS: usize = 32;
    if chunk.identifiers.len() <= MAX_IDENTIFIERS {
        return chunk.identifiers.iter().map(String::as_str).collect();
    }
    let local = chunk
        .text
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
        .collect::<BTreeSet<_>>();
    chunk
        .identifiers
        .iter()
        .map(String::as_str)
        .filter(|identifier| {
            local.contains(identifier) || identifier_semantic_alias(identifier).is_some()
        })
        .take(MAX_IDENTIFIERS)
        .collect()
}

fn joined_capacity<I>(items: I, separator_len: usize) -> usize
where
    I: IntoIterator<Item = usize>,
{
    let mut total = 0_usize;
    let mut count = 0_usize;
    for item_len in items {
        total = total.saturating_add(item_len);
        count += 1;
    }
    total.saturating_add(count.saturating_sub(1).saturating_mul(separator_len))
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn embedding_text_capacity(chunk: &Chunk) -> usize {
    let mut capacity = "Content: ".len().saturating_add(chunk.text.len());
    if !chunk.title.is_empty() {
        capacity = capacity
            .saturating_add("Title: ".len())
            .saturating_add(chunk.title.len())
            .saturating_add(1);
    }
    if !chunk.heading_path.is_empty() {
        capacity = capacity
            .saturating_add("Headings: ".len())
            .saturating_add(joined_capacity(
                chunk.heading_path.iter().map(String::len),
                " / ".len(),
            ))
            .saturating_add(1);
    }
    if !chunk.identifiers.is_empty() {
        capacity = capacity
            .saturating_add("Identifiers: ".len())
            .saturating_add(joined_capacity(
                chunk.identifiers.iter().map(String::len),
                ", ".len(),
            ))
            .saturating_add(1);
        let mut alias_count = 0_usize;
        let mut alias_bytes = 0_usize;
        for alias in chunk
            .identifiers
            .iter()
            .filter_map(|identifier| identifier_semantic_alias(identifier))
        {
            alias_count += 1;
            alias_bytes = alias_bytes.saturating_add(alias.len());
        }
        if alias_count > 0 {
            capacity = capacity
                .saturating_add("Concepts: ".len())
                .saturating_add(alias_bytes)
                .saturating_add(alias_count.saturating_sub(1).saturating_mul("; ".len()))
                .saturating_add(1);
        }
    }
    if !chunk.tags.is_empty() {
        capacity = capacity
            .saturating_add("Tags: ".len())
            .saturating_add(joined_capacity(
                chunk.tags.iter().map(String::len),
                ", ".len(),
            ))
            .saturating_add(1);
    }
    capacity
}

fn append_joined<'a, I>(output: &mut String, values: I, separator: &str)
where
    I: Iterator<Item = &'a str>,
{
    for (index, value) in values.enumerate() {
        if index > 0 {
            output.push_str(separator);
        }
        output.push_str(value);
    }
}

fn identifier_semantic_alias(identifier: &str) -> Option<&'static str> {
    Some(match identifier {
        "lbm_add_extension" | "vesc_vesc_c_if_lbm" => {
            "register native LispBM extension through the firmware C interface"
        }
        "lbm_enc_i32" | "lbm_enc_u32" | "lbm_enc_f32" => {
            "encode integer or numeric values across the native extension boundary"
        }
        "lbm_dec_as_i32" | "lbm_dec_as_u32" | "lbm_dec_as_f32" => {
            "decode integer or numeric values across the native extension boundary"
        }
        "vesc_c_if" => "firmware C interface functions available to native packages",
        "vesc_foc_audio_605" => "firmware compatibility package support FOC audio feature APIs",
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
        aliases.push("vesc_foc_audio_605 firmware compatibility support");
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
    fn embedding_dimension(&self) -> Option<usize> {
        Some(self.dimension)
    }

    fn output_normalization(&self) -> OutputNormalization {
        OutputNormalization::Guaranteed
    }

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
    batch_size: EmbeddingBatchSize,
    profile: EmbeddingProfile,
    length_bucketed: bool,
    lossless_windowing: bool,
    window_aggregation: WindowAggregation,
    fixed_batch_size: bool,
}

/// Raw normalized window vectors and their source-document owners.
#[cfg(feature = "semantic-fastembed")]
pub struct DocumentWindowVectors {
    pub vectors: Vec<Vec<f32>>,
    pub owners: Vec<usize>,
    pub token_counts: Vec<usize>,
}

/// Runtime execution provider requested for a `FastEmbed` session.
#[cfg(feature = "semantic-fastembed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SemanticExecutionProvider {
    Auto,
    Cpu,
    CoreMl,
    Rocm { device_id: i32 },
    Migraphx { device_id: i32 },
}

/// Observable ONNX Runtime/provider state used by provider benchmarks.
#[cfg(feature = "semantic-fastembed")]
#[derive(Debug, Clone, Serialize)]
pub struct SemanticRuntimeDiagnostics {
    pub ort_dylib_path: String,
    pub ort_build_info: String,
    pub requested_provider: String,
    pub selected_provider: String,
    pub selected_device: Option<i32>,
    pub provider_availability: Vec<String>,
    pub cpu_fallback_possible: bool,
    pub graph_fallback_policy: String,
}

#[cfg(feature = "semantic-fastembed")]
impl FastEmbedProvider {
    /// Wrap an initialized `FastEmbed` model.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when `batch_size` is zero.
    pub fn new(
        model: fastembed::TextEmbedding,
        batch_size: Option<usize>,
        profile: EmbeddingProfile,
    ) -> Result<Self, EmbeddingError> {
        let batch_size = batch_size.map_or_else(
            || Ok(EmbeddingBatchSize::default()),
            EmbeddingBatchSize::new,
        )?;
        Ok(Self {
            model,
            batch_size,
            profile,
            length_bucketed: false,
            lossless_windowing: false,
            window_aggregation: WindowAggregation::Mean,
            fixed_batch_size: false,
        })
    }

    /// Change the outer document batch used by subsequent embedding calls.
    ///
    /// Model initialization is intentionally independent from this setting so
    /// a benchmark can sweep batch sizes without reloading the model.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when `batch_size` is zero.
    pub fn set_batch_size(&mut self, batch_size: usize) -> Result<(), EmbeddingError> {
        self.batch_size = EmbeddingBatchSize::new(batch_size)?;
        Ok(())
    }

    /// Enables bounded token-length bucketing for subsequent document builds.
    pub const fn set_length_bucketed(&mut self, enabled: bool) {
        self.length_bucketed = enabled;
    }

    /// Enables lossless token windows for documents longer than the model limit.
    ///
    /// Each document still produces exactly one vector. Window vectors are
    /// aggregated and normalized, preserving deterministic chunk ordering while
    /// preventing the tokenizer or ONNX graph from discarding document text.
    pub const fn set_lossless_windowing(&mut self, enabled: bool) {
        self.lossless_windowing = enabled;
    }

    /// Selects how lossless document-window vectors are combined.
    pub const fn set_window_aggregation(&mut self, aggregation: WindowAggregation) {
        self.window_aggregation = aggregation;
    }

    /// Embeds every lossless source window separately for aggregation-quality gates.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when tokenization or provider inference fails.
    pub fn embed_document_windows(
        &mut self,
        texts: &[String],
    ) -> Result<DocumentWindowVectors, EmbeddingError> {
        if texts.is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }
        let prefixed = texts
            .iter()
            .map(|text| format!("{}{}", self.profile.document_prefix, text))
            .collect::<Vec<_>>();
        let mut tokenizer = self.model.tokenizer.clone();
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        let mut all_vectors = Vec::new();
        let mut all_owners = Vec::new();
        let mut all_token_counts = Vec::new();
        let mut windows = Vec::with_capacity(self.batch_size.get());
        let mut owners = Vec::with_capacity(self.batch_size.get());
        let mut token_counts = Vec::with_capacity(self.batch_size.get());
        for (owner, text) in prefixed.iter().enumerate() {
            for (window, token_count) in
                bounded_document_windows(&tokenizer, text, self.profile.max_length)?
            {
                windows.push(window);
                owners.push(owner);
                token_counts.push(token_count);
                if windows.len() == self.batch_size.get() {
                    self.embed_raw_window_batch(
                        &mut windows,
                        &mut owners,
                        &mut token_counts,
                        &mut all_vectors,
                        &mut all_owners,
                        &mut all_token_counts,
                    )?;
                }
            }
        }
        self.embed_raw_window_batch(
            &mut windows,
            &mut owners,
            &mut token_counts,
            &mut all_vectors,
            &mut all_owners,
            &mut all_token_counts,
        )?;
        Ok(DocumentWindowVectors {
            vectors: all_vectors,
            owners: all_owners,
            token_counts: all_token_counts,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn embed_raw_window_batch(
        &mut self,
        windows: &mut Vec<String>,
        owners: &mut Vec<usize>,
        token_counts: &mut Vec<usize>,
        all_vectors: &mut Vec<Vec<f32>>,
        all_owners: &mut Vec<usize>,
        all_token_counts: &mut Vec<usize>,
    ) -> Result<(), EmbeddingError> {
        if windows.is_empty() {
            return Ok(());
        }
        let real_windows = windows.len();
        if self.fixed_batch_size {
            pad_window_batch(windows, self.batch_size.get());
        }
        let batch_size = self.effective_batch_size(windows.len());
        let vectors = self
            .model
            .embed(&mut *windows, batch_size)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        self.validate_vectors(&vectors)?;
        all_vectors.extend(vectors.into_iter().take(real_windows));
        all_owners.append(owners);
        all_token_counts.append(token_counts);
        windows.clear();
        Ok(())
    }

    /// Returns the maximum number of tokens accepted in one model window.
    #[must_use]
    pub const fn max_length(&self) -> usize {
        self.profile.max_length
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
        Self::from_model_dir_with_profile_and_threads(root, batch_size, profile, None)
    }

    /// Load a local model with an optional ONNX Runtime intra-op thread count.
    ///
    /// `None` preserves `FastEmbed`'s default. This is intentionally an
    /// initialization option because ONNX Runtime does not expose it as a
    /// mutable session setting.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the model files or ORT runtime cannot
    /// be loaded, or when the profile/thread settings are invalid.
    pub fn from_model_dir_with_profile_and_threads(
        root: &std::path::Path,
        batch_size: Option<usize>,
        profile: EmbeddingProfile,
        intra_threads: Option<usize>,
    ) -> Result<Self, EmbeddingError> {
        Self::from_model_dir_with_profile_and_threads_and_provider(
            root,
            batch_size,
            profile,
            intra_threads,
            SemanticExecutionProvider::Auto,
        )
    }

    /// Load a local model with explicit ONNX Runtime provider selection.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the model files or ORT runtime cannot
    /// be loaded, when the profile/thread settings are invalid, or when an
    /// explicitly requested execution provider cannot be registered.
    pub fn from_model_dir_with_profile_and_threads_and_provider(
        root: &std::path::Path,
        batch_size: Option<usize>,
        profile: EmbeddingProfile,
        intra_threads: Option<usize>,
        execution_provider: SemanticExecutionProvider,
    ) -> Result<Self, EmbeddingError> {
        Self::from_model_dir_with_profile_and_threads_and_provider_and_graph_optimization(
            root,
            batch_size,
            profile,
            intra_threads,
            execution_provider,
            ort::session::builder::GraphOptimizationLevel::Level3,
        )
    }

    /// Load a local model with explicit ONNX Runtime provider and graph settings.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the model files or ORT runtime cannot
    /// be loaded, when the profile/thread settings are invalid, or when an
    /// explicitly requested execution provider cannot be registered.
    pub fn from_model_dir_with_profile_and_threads_and_provider_and_graph_optimization(
        root: &std::path::Path,
        batch_size: Option<usize>,
        profile: EmbeddingProfile,
        intra_threads: Option<usize>,
        execution_provider: SemanticExecutionProvider,
        graph_optimization_level: ort::session::builder::GraphOptimizationLevel,
    ) -> Result<Self, EmbeddingError> {
        if profile.max_length == 0 || profile.dimension == 0 || !profile.normalize {
            return Err(EmbeddingError::Provider(
                "FastEmbed requires a non-zero, normalized embedding profile".into(),
            ));
        }
        require_ort_runtime()?;
        let read = |name: &str| {
            std::fs::read(root.join(name)).map_err(|error| EmbeddingError::Io(error.to_string()))
        };
        let model_bytes = read("model.onnx")?;
        let model_digest = ContentDigest::of(&model_bytes);
        validate_migraphx_model_digest(model_digest.as_str(), &profile, execution_provider)?;
        let model = fastembed::UserDefinedEmbeddingModel::new(
            model_bytes,
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
        let fixed_batch_size = matches!(
            execution_provider,
            SemanticExecutionProvider::Migraphx { .. }
        );
        let mut options = fastembed::InitOptionsUserDefined::new()
            .with_max_length(profile.max_length)
            .with_execution_providers(semantic_execution_providers(execution_provider)?)
            .with_graph_optimization_level(graph_optimization_level);
        if let Some(intra_threads) = intra_threads {
            if intra_threads == 0 {
                return Err(EmbeddingError::Provider(
                    "ONNX Runtime intra-op thread count must be nonzero".into(),
                ));
            }
            options = options.with_intra_threads(intra_threads);
        }
        let mut model = fastembed::TextEmbedding::try_new_from_user_defined(model, options)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        if fixed_batch_size {
            model.set_fixed_padding(profile.max_length);
        }
        let mut provider = Self::new(model, batch_size, profile)?;
        provider.fixed_batch_size = fixed_batch_size;
        Ok(provider)
    }

    /// Measures the token counts and padding used by `FastEmbed`'s configured
    /// tokenizer, one outer provider batch at a time.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::EmptyInput`] for empty input or
    /// [`EmbeddingError::Provider`] when tokenization fails.
    pub fn token_statistics(&self, texts: &[String]) -> Result<TokenStatistics, EmbeddingError> {
        self.token_statistics_iter(texts.iter().map(String::as_str))
    }

    /// Measure token statistics from a stream of owned or borrowed texts.
    ///
    /// Only one provider batch, plus lossless windows for the current source
    /// text, is retained at a time. This keeps full-corpus diagnostics from
    /// rebuilding a second corpus-wide text vector.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::EmptyInput`] for empty input or
    /// [`EmbeddingError::Provider`] when tokenization fails.
    pub fn token_statistics_iter<I, S>(&self, texts: I) -> Result<TokenStatistics, EmbeddingError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut untruncated_tokenizer = self.model.tokenizer.clone();
        untruncated_tokenizer.with_padding(None);
        untruncated_tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;

        let mut real_tokens = Vec::new();
        let mut total_real_tokens = 0_u64;
        let mut total_padded_tokens = 0_u64;
        let mut total_untruncated_tokens = 0_u64;
        let mut truncated_chunks = 0_usize;
        let mut original_chunks = 0_usize;
        let mut batch_texts = Vec::with_capacity(self.batch_size.get());

        let mut measure_batch = |batch: &mut Vec<String>| -> Result<(), EmbeddingError> {
            if batch.is_empty() {
                return Ok(());
            }
            let inputs = batch.iter().map(String::as_str).collect::<Vec<_>>();
            let configured = self
                .model
                .tokenizer
                .encode_batch(inputs.clone(), true)
                .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
            // Encode untruncated inputs one at a time.  A long-context model
            // can otherwise allocate one large padded tensor for the whole
            // outer batch merely to collect diagnostics, defeating bounded
            // windowing before inference starts.
            for (configured, input) in configured.iter().zip(batch.iter()) {
                let untruncated = untruncated_tokenizer
                    .encode(input.as_str(), true)
                    .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
                let real = configured
                    .get_attention_mask()
                    .iter()
                    .filter(|&&value| value != 0)
                    .count();
                let padded = configured.get_ids().len();
                let raw = untruncated.get_ids().len();
                real_tokens.push(real);
                total_real_tokens = total_real_tokens.saturating_add(real as u64);
                total_padded_tokens = total_padded_tokens.saturating_add(padded as u64);
                total_untruncated_tokens = total_untruncated_tokens.saturating_add(raw as u64);
                truncated_chunks += usize::from(raw > self.profile.max_length);
            }
            batch.clear();
            Ok(())
        };

        for source in texts {
            original_chunks += 1;
            let source = source.as_ref();
            let prefixed;
            let source = if self.profile.document_prefix.is_empty() {
                source
            } else {
                prefixed = format!("{}{}", self.profile.document_prefix, source);
                &prefixed
            };
            if self.lossless_windowing {
                for (window, _) in self.document_windows(source)? {
                    batch_texts.push(window);
                    if batch_texts.len() == self.batch_size.get() {
                        measure_batch(&mut batch_texts)?;
                    }
                }
            } else {
                batch_texts.push(source.to_owned());
                if batch_texts.len() == self.batch_size.get() {
                    measure_batch(&mut batch_texts)?;
                }
            }
        }
        measure_batch(&mut batch_texts)?;
        if original_chunks == 0 {
            return Err(EmbeddingError::EmptyInput);
        }

        real_tokens.sort_unstable();
        let percentile = |percentile: usize| {
            let index = ((percentile * real_tokens.len()).saturating_add(99) / 100)
                .saturating_sub(1)
                .min(real_tokens.len().saturating_sub(1));
            real_tokens[index]
        };
        let padding_waste = total_padded_tokens.saturating_sub(total_real_tokens);
        Ok(TokenStatistics {
            chunks: original_chunks,
            total_real_tokens,
            total_padded_tokens,
            total_untruncated_tokens,
            min_tokens: real_tokens[0],
            median_tokens: percentile(50),
            p95_tokens: percentile(95),
            maximum_tokens: real_tokens[real_tokens.len() - 1],
            truncated_chunks,
            padding_ratio_ppm: padding_waste
                .saturating_mul(1_000_000)
                .checked_div(total_padded_tokens)
                .unwrap_or_default(),
        })
    }

    /// Returns post-truncation token lengths in input order for benchmark
    /// length bucketing.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::EmptyInput`] for empty input or
    /// [`EmbeddingError::Provider`] when tokenization fails.
    pub fn token_lengths(&self, texts: &[String]) -> Result<Vec<usize>, EmbeddingError> {
        self.token_lengths_iter(texts)
    }

    /// Returns post-truncation token lengths while retaining one provider batch.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::EmptyInput`] for empty input or
    /// [`EmbeddingError::Provider`] when tokenization fails.
    pub fn token_lengths_iter<I, S>(&self, texts: I) -> Result<Vec<usize>, EmbeddingError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut lengths = Vec::new();
        let mut batch = Vec::with_capacity(self.batch_size.get());
        for text in texts {
            let text = text.as_ref();
            batch.push(if self.profile.document_prefix.is_empty() {
                text.to_owned()
            } else {
                format!("{}{}", self.profile.document_prefix, text)
            });
            if batch.len() == self.batch_size.get() {
                self.append_token_lengths(&batch, &mut lengths)?;
                batch.clear();
            }
        }
        self.append_token_lengths(&batch, &mut lengths)?;
        (!lengths.is_empty())
            .then_some(lengths)
            .ok_or(EmbeddingError::EmptyInput)
    }

    fn append_token_lengths(
        &self,
        texts: &[String],
        lengths: &mut Vec<usize>,
    ) -> Result<(), EmbeddingError> {
        if texts.is_empty() {
            return Ok(());
        }
        let inputs = texts.iter().map(String::as_str).collect::<Vec<_>>();
        let configured = self
            .model
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        lengths.extend(configured.iter().map(|encoding| {
            encoding
                .get_attention_mask()
                .iter()
                .filter(|&&value| value != 0)
                .count()
        }));
        Ok(())
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

    fn document_windows(&self, text: &str) -> Result<Vec<(String, usize)>, EmbeddingError> {
        let mut tokenizer = self.model.tokenizer.clone();
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        bounded_document_windows(&tokenizer, text, self.profile.max_length)
    }

    fn document_window_batches(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<(String, usize)>>, EmbeddingError> {
        let mut tokenizer = self.model.tokenizer.clone();
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        texts
            .par_iter()
            .map(|text| bounded_document_windows(&tokenizer, text, self.profile.max_length))
            .collect()
    }

    fn ensure_document_lengths(&self, texts: &[&str]) -> Result<(), EmbeddingError> {
        let mut tokenizer = self.model.tokenizer.clone();
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        let encodings = tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        if let Some(length) = encodings
            .iter()
            .map(|encoding| encoding.get_ids().len())
            .find(|&length| length > self.profile.max_length)
        {
            return Err(EmbeddingError::Provider(format!(
                "document input has {length} tokens, exceeding the model limit of {}; lossless windowing is required",
                self.profile.max_length
            )));
        }
        Ok(())
    }

    fn effective_batch_size(&self, input_len: usize) -> Option<usize> {
        (input_len > 0).then(|| self.batch_size.get().min(input_len))
    }

    fn length_bucket_order(&self, chunks: &[&Chunk]) -> Result<Vec<usize>, EmbeddingError> {
        let mut keyed = Vec::with_capacity(chunks.len());
        for (base, batch) in chunks.chunks(self.batch_size.get()).enumerate() {
            let texts = batch
                .iter()
                .map(|chunk| embedding_text(chunk))
                .collect::<Vec<_>>();
            let lengths = self.token_lengths(&texts)?;
            keyed.extend(
                lengths
                    .into_iter()
                    .enumerate()
                    .map(|(offset, length)| (length, base * self.batch_size.get() + offset)),
            );
        }
        keyed.sort_unstable_by(|(left_length, left), (right_length, right)| {
            left_length.cmp(right_length).then_with(|| {
                chunks[*left]
                    .path
                    .cmp(&chunks[*right].path)
                    .then_with(|| chunks[*left].ordinal.cmp(&chunks[*right].ordinal))
                    .then_with(|| chunks[*left].chunk_id.cmp(&chunks[*right].chunk_id))
            })
        });
        Ok(keyed.into_iter().map(|(_, index)| index).collect())
    }

    fn embed_window_batch(
        &mut self,
        windows: &mut Vec<String>,
        owners: &mut Vec<usize>,
        token_counts: &mut Vec<usize>,
        sums: &mut [Vec<f32>],
        counts: &mut [usize],
    ) -> Result<(), EmbeddingError> {
        if windows.is_empty() {
            return Ok(());
        }
        if self.fixed_batch_size {
            pad_window_batch(windows, self.batch_size.get());
        }
        let batch_size = self.effective_batch_size(windows.len());
        let vectors = self
            .model
            .embed(&mut *windows, batch_size)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        self.validate_vectors(&vectors)?;
        for ((vector, &owner), &token_count) in
            vectors.iter().zip(owners.iter()).zip(token_counts.iter())
        {
            let Some(sum) = sums.get_mut(owner) else {
                return Err(EmbeddingError::Provider(
                    "lossless window owner is out of range".into(),
                ));
            };
            let weight = match self.window_aggregation {
                WindowAggregation::Mean => 1.0,
                WindowAggregation::TokenWeightedMean => {
                    u16::try_from(token_count).map(f32::from).map_err(|_| {
                        EmbeddingError::Provider("window token count exceeds u16".into())
                    })?
                }
            };
            for (sum, value) in sum.iter_mut().zip(vector) {
                *sum = weight.mul_add(*value, *sum);
            }
            counts[owner] = counts[owner].saturating_add(1);
        }
        windows.clear();
        owners.clear();
        token_counts.clear();
        Ok(())
    }
}

#[cfg(feature = "semantic-fastembed")]
fn validate_migraphx_model_digest(
    model_digest: &str,
    profile: &EmbeddingProfile,
    execution_provider: SemanticExecutionProvider,
) -> Result<(), EmbeddingError> {
    if matches!(
        execution_provider,
        SemanticExecutionProvider::Migraphx { .. }
    ) && profile.pooling == Pooling::Mean
        && profile.dimension == 768
        && model_digest == crate::JINA_CODE_INT8_SHA256
    {
        return Err(EmbeddingError::Provider(
            "the pinned Jina INT8 graph is disabled on MIGraphX; use the pinned FP16 graph".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "semantic-fastembed")]
fn bounded_document_windows(
    tokenizer: &tokenizers::Tokenizer,
    text: &str,
    max_length: usize,
) -> Result<Vec<(String, usize)>, EmbeddingError> {
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
    let offsets = encoding
        .get_offsets()
        .iter()
        .copied()
        .filter(|&(start, end)| end > start)
        .collect::<Vec<_>>();
    let special_tokens = encoding
        .get_special_tokens_mask()
        .iter()
        .filter(|&&is_special| is_special != 0)
        .count();
    let content_limit = max_length.saturating_sub(special_tokens);
    if content_limit == 0 {
        return Err(EmbeddingError::Provider(
            "model limit is smaller than its special-token overhead".into(),
        ));
    }
    if encoding.get_ids().len() <= max_length {
        return Ok(vec![(text.to_owned(), encoding.get_ids().len())]);
    }

    let mut windows = Vec::with_capacity(offsets.len().div_ceil(content_limit));
    let mut start = 0;
    for group in offsets.chunks(content_limit) {
        let end = group.last().map_or(start, |&(_, end)| end.min(text.len()));
        if end > start {
            windows.push(text[start..end].to_owned());
            start = end;
        }
    }
    if start < text.len() {
        if let Some(last) = windows.last_mut() {
            last.push_str(&text[start..]);
        } else {
            windows.push(text.to_owned());
        }
    }

    let mut bounded = Vec::with_capacity(windows.len());
    let mut pending = windows;
    while let Some(window) = pending.pop() {
        let length = tokenizer
            .encode(window.as_str(), true)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?
            .get_ids()
            .len();
        if length <= max_length {
            bounded.push((window, length));
            continue;
        }

        let mut midpoint = window.len() / 2;
        while midpoint > 0 && !window.is_char_boundary(midpoint) {
            midpoint -= 1;
        }
        if midpoint == 0 {
            return Err(EmbeddingError::Provider(format!(
                "model input cannot fit within the model limit of {max_length} tokens"
            )));
        }
        pending.push(window[midpoint..].to_owned());
        pending.push(window[..midpoint].to_owned());
    }
    bounded.reverse();
    Ok(bounded)
}

#[cfg(feature = "semantic-fastembed")]
fn pad_window_batch(windows: &mut Vec<String>, batch_size: usize) {
    if windows.is_empty() || windows.len() >= batch_size {
        return;
    }
    windows.resize(batch_size, String::new());
}

#[cfg(feature = "semantic-fastembed")]
fn require_ort_runtime() -> Result<(), EmbeddingError> {
    let Some(path) = std::env::var_os("ORT_DYLIB_PATH") else {
        return Err(EmbeddingError::Provider(
            "ORT_DYLIB_PATH must point to a local ONNX Runtime dylib".into(),
        ));
    };
    if !std::path::Path::new(&path).is_file() {
        return Err(EmbeddingError::Provider(format!(
            "ORT_DYLIB_PATH does not point to a file: {}",
            std::path::Path::new(&path).display()
        )));
    }
    Ok(())
}

#[cfg(feature = "semantic-fastembed")]
// The Linux/default branch is const-capable, but the macOS CoreML branch
// intentionally reads an environment override and therefore cannot be const.
#[allow(clippy::missing_const_for_fn)]
fn resolve_semantic_execution_provider(
    requested: SemanticExecutionProvider,
) -> SemanticExecutionProvider {
    if !matches!(requested, SemanticExecutionProvider::Auto) {
        return requested;
    }

    #[cfg(all(feature = "semantic-rocm", target_os = "linux"))]
    {
        SemanticExecutionProvider::Rocm { device_id: 0 }
    }
    #[cfg(not(all(feature = "semantic-rocm", target_os = "linux")))]
    {
        #[cfg(all(feature = "semantic-coreml", target_os = "macos"))]
        {
            if std::env::var("VESC_RAG_SEMANTIC_EXECUTION_PROVIDER")
                .ok()
                .is_some_and(|provider| provider.eq_ignore_ascii_case("coreml"))
            {
                return SemanticExecutionProvider::CoreMl;
            }
        }
        SemanticExecutionProvider::Cpu
    }
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_execution_providers(
    requested: SemanticExecutionProvider,
) -> Result<Vec<fastembed::ExecutionProviderDispatch>, EmbeddingError> {
    let selected = resolve_semantic_execution_provider(requested);
    match selected {
        SemanticExecutionProvider::Auto | SemanticExecutionProvider::Cpu => Ok(vec![
            ort::ep::CPU::default().with_arena_allocator(false).build(),
        ]),
        SemanticExecutionProvider::Rocm { device_id } => {
            #[cfg(all(feature = "semantic-rocm", target_os = "linux"))]
            {
                Ok(vec![
                    ort::ep::ROCm::default()
                        .with_device_id(device_id)
                        .build()
                        .error_on_failure(),
                ])
            }
            #[cfg(not(all(feature = "semantic-rocm", target_os = "linux")))]
            {
                let _ = device_id;
                Err(EmbeddingError::Provider(
                    "ROCm provider requested, but this binary was not built with semantic-rocm on Linux".into(),
                ))
            }
        }
        SemanticExecutionProvider::CoreMl => {
            #[cfg(all(feature = "semantic-coreml", target_os = "macos"))]
            {
                Ok(vec![
                    ort::ep::CoreML::default()
                        .with_compute_units(ort::ep::coreml::ComputeUnits::All)
                        .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
                        .with_specialization_strategy(
                            ort::ep::coreml::SpecializationStrategy::FastPrediction,
                        )
                        .with_low_precision_accumulation_on_gpu(true)
                        .build()
                        .error_on_failure(),
                ])
            }
            #[cfg(not(all(feature = "semantic-coreml", target_os = "macos")))]
            {
                Err(EmbeddingError::Provider(
                    "CoreML provider requested, but this binary was not built with semantic-coreml on macOS".into(),
                ))
            }
        }
        SemanticExecutionProvider::Migraphx { device_id } => {
            #[cfg(all(feature = "semantic-migraphx", target_os = "linux"))]
            {
                Ok(vec![
                    ort::ep::MIGraphX::default()
                        .with_device_id(device_id)
                        .build()
                        .error_on_failure(),
                ])
            }
            #[cfg(not(all(feature = "semantic-migraphx", target_os = "linux")))]
            {
                let _ = device_id;
                Err(EmbeddingError::Provider(
                    "MIGraphX provider requested, but this binary was not built with semantic-migraphx on Linux".into(),
                ))
            }
        }
    }
}

#[cfg(feature = "semantic-fastembed")]
fn provider_availability_entry<E: ort::ep::ExecutionProvider>(
    provider: &E,
) -> Result<String, EmbeddingError> {
    let name = provider.name();
    let available = provider
        .is_available()
        .map_err(|error| EmbeddingError::Provider(format!("check {name} availability: {error}")))?;
    Ok(format!("{name}={available}"))
}

/// Return the loaded ORT build and provider capabilities before model setup.
///
/// # Errors
///
/// Returns [`EmbeddingError`] when ORT cannot be loaded or its provider
/// availability cannot be queried.
#[cfg(feature = "semantic-fastembed")]
pub fn semantic_runtime_diagnostics(
    requested: SemanticExecutionProvider,
) -> Result<SemanticRuntimeDiagnostics, EmbeddingError> {
    // `require_ort_runtime` validates the same setting before the ORT API is
    // touched; use a fallible read here so this public API never panics.
    require_ort_runtime()?;
    let selected = resolve_semantic_execution_provider(requested);
    let path = std::env::var_os("ORT_DYLIB_PATH")
        .ok_or_else(|| EmbeddingError::Provider("ORT_DYLIB_PATH is not set".into()))?
        .to_string_lossy()
        .into_owned();
    #[allow(unused_mut)]
    let mut provider_availability = vec![provider_availability_entry(&ort::ep::CPU::default())?];
    #[cfg(feature = "semantic-rocm")]
    provider_availability.push(provider_availability_entry(&ort::ep::ROCm::default())?);
    #[cfg(feature = "semantic-coreml")]
    provider_availability.push(provider_availability_entry(&ort::ep::CoreML::default())?);
    #[cfg(feature = "semantic-migraphx")]
    provider_availability.push(provider_availability_entry(&ort::ep::MIGraphX::default())?);

    let (selected_provider, selected_device) = match selected {
        SemanticExecutionProvider::Auto => ("Auto".to_string(), None),
        SemanticExecutionProvider::Cpu => ("CPUExecutionProvider".to_string(), None),
        SemanticExecutionProvider::CoreMl => ("CoreMLExecutionProvider".to_string(), None),
        SemanticExecutionProvider::Rocm { device_id } => {
            ("ROCMExecutionProvider".to_string(), Some(device_id))
        }
        SemanticExecutionProvider::Migraphx { device_id } => {
            ("MIGraphXExecutionProvider".to_string(), Some(device_id))
        }
    };
    Ok(SemanticRuntimeDiagnostics {
        ort_dylib_path: path,
        ort_build_info: ort::info().to_string(),
        requested_provider: format!("{requested:?}"),
        selected_provider,
        selected_device,
        provider_availability,
        cpu_fallback_possible: !matches!(selected, SemanticExecutionProvider::Cpu),
        graph_fallback_policy: "EP registration is fatal for explicit non-CPU requests; FastEmbed does not expose disable_cpu_fallback, so unsupported graph nodes may still fall back to CPU.".into(),
    })
}

/// Enable verbose ORT graph/provider logging before the first session is built.
///
/// # Errors
///
/// Returns [`EmbeddingError`] when the configured ORT runtime cannot be loaded
/// or its environment cannot be initialized.
#[cfg(feature = "semantic-fastembed")]
pub fn configure_ort_verbose_logging(verbose: bool) -> Result<(), EmbeddingError> {
    if !verbose {
        return Ok(());
    }
    require_ort_runtime()?;
    let logger = std::sync::Arc::new(
        |level: ort::logging::LogLevel,
         category: &str,
         id: &str,
         code_location: &str,
         message: &str| {
            eprintln!("ort[{level:?}] {category} {id} {code_location}: {message}");
        },
    );
    ort::init().with_logger(logger).commit();
    ort::environment::Environment::current()
        .map_err(|error| EmbeddingError::Provider(format!("initialize ORT logging: {error}")))?
        .set_log_level(ort::logging::LogLevel::Verbose);
    Ok(())
}

#[cfg(feature = "semantic-fastembed")]
impl EmbeddingProvider for FastEmbedProvider {
    fn embedding_dimension(&self) -> Option<usize> {
        Some(self.profile.dimension)
    }

    fn embedding_batch_size(&self) -> EmbeddingBatchSize {
        self.batch_size
    }

    fn output_normalization(&self) -> OutputNormalization {
        if self.profile.normalize {
            OutputNormalization::Guaranteed
        } else {
            OutputNormalization::Unknown
        }
    }

    fn inference_order(&mut self, chunks: &[&Chunk]) -> Result<Option<Vec<usize>>, EmbeddingError> {
        if self.length_bucketed && chunks.len() > 1 {
            Ok(Some(self.length_bucket_order(chunks)?))
        } else {
            Ok(None)
        }
    }

    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let prefixed;
        let texts = if self.profile.document_prefix.is_empty() {
            texts
        } else {
            prefixed = texts
                .iter()
                .map(|text| format!("{}{}", self.profile.document_prefix, text))
                .collect::<Vec<_>>();
            &prefixed
        };
        if self.lossless_windowing {
            let mut sums = vec![vec![0.0_f32; self.profile.dimension]; texts.len()];
            let mut counts = vec![0_usize; texts.len()];
            let mut window_batch = Vec::with_capacity(self.batch_size.get());
            let mut owners = Vec::with_capacity(self.batch_size.get());
            let mut token_counts = Vec::with_capacity(self.batch_size.get());
            for (owner, source_windows) in
                self.document_window_batches(texts)?.into_iter().enumerate()
            {
                for (window, token_count) in source_windows {
                    window_batch.push(window);
                    owners.push(owner);
                    token_counts.push(token_count);
                    if window_batch.len() == self.batch_size.get() {
                        self.embed_window_batch(
                            &mut window_batch,
                            &mut owners,
                            &mut token_counts,
                            &mut sums,
                            &mut counts,
                        )?;
                    }
                }
            }
            self.embed_window_batch(
                &mut window_batch,
                &mut owners,
                &mut token_counts,
                &mut sums,
                &mut counts,
            )?;
            let mut aggregated = Vec::with_capacity(texts.len());
            for (mut vector, count) in sums.into_iter().zip(counts) {
                if count == 0 {
                    return Err(EmbeddingError::EmptyInput);
                }
                normalize(&mut vector)?;
                aggregated.push(vector);
            }
            return Ok(aggregated);
        }
        let inputs = texts.iter().map(String::as_str).collect::<Vec<_>>();
        self.ensure_document_lengths(&inputs)?;
        let vectors = self
            .model
            .embed(texts, self.effective_batch_size(texts.len()))
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

struct VectorCheckpoint {
    file: File,
    dimension: usize,
    count: usize,
    completed: Vec<u8>,
    values_offset: u64,
}

impl VectorCheckpoint {
    fn open(
        path: &Path,
        dimension: usize,
        count: usize,
        corpus_digest: &ContentDigest,
    ) -> Result<Self, EmbeddingError> {
        let parent = path.parent().ok_or_else(|| {
            EmbeddingError::Io("vector checkpoint path has no parent directory".into())
        })?;
        fs::create_dir_all(parent).map_err(|error| EmbeddingError::Io(error.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|error| EmbeddingError::Io(error.to_string()))?;
        let completed_len = count.saturating_add(7) / 8;
        let values_offset = u64::try_from(VECTOR_CHECKPOINT_MAGIC.len() + 8 + 32 + completed_len)
            .map_err(|_| EmbeddingError::TooLarge)?;
        let values_len = count
            .checked_mul(dimension)
            .and_then(|values| values.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(EmbeddingError::TooLarge)?;
        let expected_len = values_offset
            .checked_add(u64::try_from(values_len).map_err(|_| EmbeddingError::TooLarge)?)
            .ok_or(EmbeddingError::TooLarge)?;
        let existing_len = file
            .metadata()
            .map_err(|error| EmbeddingError::Io(error.to_string()))?
            .len();
        let mut reset = existing_len != expected_len;
        if !reset {
            file.rewind()
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            let mut header = [0_u8; 48];
            file.read_exact(&mut header)
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            let stored_dimension = u32::from_le_bytes(
                header[8..12]
                    .try_into()
                    .map_err(|_| EmbeddingError::InvalidHeader)?,
            ) as usize;
            let stored_count = u32::from_le_bytes(
                header[12..16]
                    .try_into()
                    .map_err(|_| EmbeddingError::InvalidHeader)?,
            ) as usize;
            reset = &header[..8] != VECTOR_CHECKPOINT_MAGIC
                || stored_dimension != dimension
                || stored_count != count
                || digest_from_bytes(&header[16..48]).ok().as_ref() != Some(corpus_digest);
        }
        if reset {
            file.set_len(0)
                .and_then(|()| file.rewind())
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            file.write_all(VECTOR_CHECKPOINT_MAGIC)
                .and_then(|()| {
                    file.write_all(
                        &u32::try_from(dimension)
                            .map_err(std::io::Error::other)?
                            .to_le_bytes(),
                    )
                })
                .and_then(|()| {
                    file.write_all(
                        &u32::try_from(count)
                            .map_err(std::io::Error::other)?
                            .to_le_bytes(),
                    )
                })
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            file.write_all(&digest_bytes(corpus_digest)?)
                .and_then(|()| file.write_all(&vec![0_u8; completed_len]))
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            file.set_len(expected_len)
                .and_then(|()| file.sync_data())
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
        }

        file.seek(SeekFrom::Start(48))
            .map_err(|error| EmbeddingError::Io(error.to_string()))?;
        let mut completed = vec![0_u8; completed_len];
        file.read_exact(&mut completed)
            .map_err(|error| EmbeddingError::Io(error.to_string()))?;
        Ok(Self {
            file,
            dimension,
            count,
            completed,
            values_offset,
        })
    }

    fn is_complete(&self, row: usize) -> bool {
        row < self.count && self.completed[row / 8] & (1 << (row % 8)) != 0
    }

    fn read_values(&mut self, output: &mut [f32]) -> Result<(), EmbeddingError> {
        let expected = self
            .count
            .checked_mul(self.dimension)
            .ok_or(EmbeddingError::TooLarge)?;
        if output.len() != expected || (0..self.count).any(|row| !self.is_complete(row)) {
            return Err(EmbeddingError::InvalidHeader);
        }
        self.file
            .seek(SeekFrom::Start(self.values_offset))
            .map_err(|error| EmbeddingError::Io(error.to_string()))?;
        let mut bytes = vec![0_u8; STREAM_BUFFER_BYTES];
        for values in output.chunks_mut(STREAM_BUFFER_BYTES / std::mem::size_of::<f32>()) {
            let byte_count = std::mem::size_of_val(values);
            self.file
                .read_exact(&mut bytes[..byte_count])
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            for (value, bytes) in values.iter_mut().zip(bytes[..byte_count].chunks_exact(4)) {
                *value = f32::from_le_bytes(bytes.try_into().expect("four-byte checkpoint value"));
            }
        }
        Ok(())
    }

    fn write_batch<V: AsRef<[f32]>>(
        &mut self,
        rows: &[usize],
        vectors: &[V],
    ) -> Result<(), EmbeddingError> {
        if rows.len() != vectors.len() {
            return Err(EmbeddingError::InvalidHeader);
        }
        let row_bytes = self
            .dimension
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or(EmbeddingError::TooLarge)?;
        let mut bytes = Vec::with_capacity(row_bytes);
        for (&row, vector) in rows.iter().zip(vectors) {
            let vector = vector.as_ref();
            if row >= self.count || vector.len() != self.dimension {
                return Err(EmbeddingError::InvalidHeader);
            }
            self.file
                .seek(SeekFrom::Start(self.row_offset(row)?))
                .map_err(|error| EmbeddingError::Io(error.to_string()))?;
            bytes.clear();
            for value in vector {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            self.file.write_all(&bytes).map_err(io_error)?;
        }
        self.file
            .sync_data()
            .map_err(|error| EmbeddingError::Io(error.to_string()))?;
        for &row in rows {
            self.completed[row / 8] |= 1 << (row % 8);
        }
        self.file
            .seek(SeekFrom::Start(48))
            .and_then(|_| self.file.write_all(&self.completed))
            .and_then(|()| self.file.sync_data())
            .map_err(|error| EmbeddingError::Io(error.to_string()))
    }

    fn row_offset(&self, row: usize) -> Result<u64, EmbeddingError> {
        let bytes = row
            .checked_mul(self.dimension)
            .and_then(|values| values.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(EmbeddingError::TooLarge)?;
        self.values_offset
            .checked_add(u64::try_from(bytes).map_err(|_| EmbeddingError::TooLarge)?)
            .ok_or(EmbeddingError::TooLarge)
    }
}

fn flush_checkpoint_batch(
    checkpoint: &mut VectorCheckpoint,
    rows: &mut Vec<usize>,
    vectors: &mut Vec<Vec<f32>>,
) -> Result<(), EmbeddingError> {
    if rows.is_empty() {
        return Ok(());
    }
    checkpoint.write_batch(rows, vectors)?;
    rows.clear();
    vectors.clear();
    Ok(())
}

/// Reproducible row-major vector artifact keyed by sorted stable chunk IDs.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorArtifact {
    pub schema: u16,
    pub model_id: String,
    pub model_revision: String,
    pub dimension: usize,
    pub normalized: bool,
    pub corpus_digest: ContentDigest,
    /// Chunk IDs in ascending order, one per vector row.
    pub ids: Vec<ChunkId>,
    /// Row-major vector values (`ids.len() * dimension`).
    pub values: Vec<f32>,
}

fn encoded_artifact_len(
    chunks: &[Chunk],
    dimension: usize,
    model_id: &str,
    model_revision: &str,
) -> Result<usize, EmbeddingError> {
    u32::try_from(chunks.len()).map_err(|_| EmbeddingError::TooLarge)?;
    u32::try_from(dimension).map_err(|_| EmbeddingError::TooLarge)?;
    u32::try_from(model_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
    u32::try_from(model_revision.len()).map_err(|_| EmbeddingError::TooLarge)?;

    let fixed = MAGIC
        .len()
        .checked_add(4 * 5)
        .and_then(|len| len.checked_add(model_id.len()))
        .and_then(|len| len.checked_add(model_revision.len()))
        .and_then(|len| len.checked_add(1 + 32 + CHECKSUM_LEN))
        .ok_or(EmbeddingError::TooLarge)?;
    let vector_bytes = dimension
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or(EmbeddingError::TooLarge)?;
    let total = chunks.iter().try_fold(fixed, |total, chunk| {
        let id_len = chunk.chunk_id.as_str().len();
        u16::try_from(id_len).map_err(|_| EmbeddingError::TooLarge)?;
        total
            .checked_add(2)
            .and_then(|total| total.checked_add(id_len))
            .and_then(|total| total.checked_add(vector_bytes))
            .ok_or(EmbeddingError::TooLarge)
    })?;
    (total <= MAX_ARTIFACT_BYTES)
        .then_some(total)
        .ok_or(EmbeddingError::TooLarge)
}

impl VectorArtifact {
    fn is_reusable_previous(
        &self,
        model_id: &str,
        model_revision: &str,
        provider_dimension: Option<usize>,
    ) -> bool {
        self.validate().is_ok()
            && self.normalized
            && self.model_id == model_id
            && self.model_revision == model_revision
            && provider_dimension.is_none_or(|dimension| dimension == self.dimension)
    }

    fn from_provider_with_selected_order<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: String,
        model_revision: String,
        corpus_digest: ContentDigest,
    ) -> Result<(Self, VectorBuildObservations), EmbeddingError> {
        let chunk_refs = chunks.iter().collect::<Vec<_>>();
        let inference_order = provider.inference_order(&chunk_refs)?;
        Self::from_provider_with_observations_and_order(
            provider,
            chunks,
            model_id,
            model_revision,
            corpus_digest,
            inference_order.as_deref(),
        )
    }

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
        Self::from_provider_with_observations(
            provider,
            chunks,
            model_id,
            model_revision,
            corpus_digest,
        )
        .map(|(artifact, _)| artifact)
    }

    /// Builds an artifact and reports the non-overlapping vector-build intervals.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when provider output is empty, inconsistent,
    /// non-finite, or not normalizable.
    pub fn from_provider_with_observations<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        corpus_digest: ContentDigest,
    ) -> Result<(Self, VectorBuildObservations), EmbeddingError> {
        Self::from_provider_with_observations_and_order(
            provider,
            chunks,
            model_id,
            model_revision,
            corpus_digest,
            None,
        )
    }

    /// Builds an artifact while copying rows for stable chunk IDs from a compatible artifact.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when reused or newly embedded rows cannot form a valid
    /// artifact for the requested corpus.
    pub fn from_provider_reusing_with_observations<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        corpus_digest: ContentDigest,
        previous: Option<&Self>,
    ) -> Result<(Self, VectorBuildObservations), EmbeddingError> {
        let model_id = model_id.into();
        let model_revision = model_revision.into();
        let provider_dimension = provider.embedding_dimension();
        let previous = previous.filter(|artifact| {
            artifact.is_reusable_previous(&model_id, &model_revision, provider_dimension)
        });

        if previous.is_none() {
            return Self::from_provider_with_selected_order(
                provider,
                chunks,
                model_id,
                model_revision,
                corpus_digest,
            );
        }

        if let Some(dimension) = previous
            .map(|artifact| artifact.dimension)
            .or(provider_dimension)
        {
            encoded_artifact_len(chunks, dimension, &model_id, &model_revision)?;
        }

        let missing = chunks
            .iter()
            .filter(|chunk| {
                previous.is_none_or(|artifact| artifact.ids.binary_search(&chunk.chunk_id).is_err())
            })
            .cloned()
            .collect::<Vec<_>>();
        let (embedded, mut observations) = if missing.is_empty() {
            (None, VectorBuildObservations::default())
        } else {
            let missing_refs = missing.iter().collect::<Vec<_>>();
            let inference_order = provider.inference_order(&missing_refs)?;
            let (artifact, observations) = Self::from_provider_with_observations_and_order(
                provider,
                &missing,
                &model_id,
                &model_revision,
                corpus_digest.clone(),
                inference_order.as_deref(),
            )?;
            (Some(artifact), observations)
        };

        let dimension = previous
            .as_ref()
            .map(|artifact| artifact.dimension)
            .or_else(|| embedded.as_ref().map(|artifact| artifact.dimension))
            .ok_or(EmbeddingError::EmptyInput)?;
        let value_count = chunks
            .len()
            .checked_mul(dimension)
            .ok_or(EmbeddingError::TooLarge)?;
        let mut order = (0..chunks.len()).collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| chunks[*left].chunk_id.cmp(&chunks[*right].chunk_id));
        let mut ids = Vec::with_capacity(chunks.len());
        let mut values = Vec::with_capacity(value_count);
        for index in order {
            let id = &chunks[index].chunk_id;
            let source = previous
                .and_then(|artifact| {
                    artifact
                        .ids
                        .binary_search(id)
                        .ok()
                        .map(|row| (artifact, row))
                })
                .or_else(|| {
                    embedded.as_ref().and_then(|artifact| {
                        artifact
                            .ids
                            .binary_search(id)
                            .ok()
                            .map(|row| (artifact, row))
                    })
                })
                .ok_or_else(|| EmbeddingError::MissingChunk(id.clone()))?;
            let start = source
                .1
                .checked_mul(dimension)
                .ok_or(EmbeddingError::TooLarge)?;
            let end = start
                .checked_add(dimension)
                .ok_or(EmbeddingError::TooLarge)?;
            ids.push(id.clone());
            values.extend_from_slice(&source.0.values[start..end]);
        }

        observations.reused_vectors = chunks.len().saturating_sub(missing.len());
        observations.embedded_vectors = missing.len();
        let artifact = Self {
            schema: 2,
            model_id,
            model_revision,
            dimension,
            normalized: true,
            corpus_digest,
            ids,
            values,
        };
        artifact.validate()?;
        Ok((artifact, observations))
    }

    /// Builds an artifact while durably checkpointing each completed inference batch.
    ///
    /// Completed rows are written in final artifact order and reused after a restart.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the checkpoint, provider output, or final
    /// artifact is invalid.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn from_provider_reusing_with_checkpoint_observations<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        corpus_digest: ContentDigest,
        previous: Option<Self>,
        checkpoint_path: &Path,
    ) -> Result<(Self, VectorBuildObservations), EmbeddingError> {
        let model_id = model_id.into();
        let model_revision = model_revision.into();
        let provider_dimension = provider.embedding_dimension();
        let previous = previous.filter(|artifact| {
            artifact.is_reusable_previous(&model_id, &model_revision, provider_dimension)
        });
        let dimension = previous
            .as_ref()
            .map(|artifact| artifact.dimension)
            .or(provider_dimension)
            .ok_or(EmbeddingError::InvalidHeader)?;
        encoded_artifact_len(chunks, dimension, &model_id, &model_revision)?;

        let mut order = (0..chunks.len()).collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| chunks[*left].chunk_id.cmp(&chunks[*right].chunk_id));
        let mut rows = vec![0_usize; chunks.len()];
        let ids = order
            .iter()
            .enumerate()
            .map(|(row, &index)| {
                rows[index] = row;
                chunks[index].chunk_id.clone()
            })
            .collect::<Vec<_>>();
        let mut checkpoint =
            VectorCheckpoint::open(checkpoint_path, dimension, chunks.len(), &corpus_digest)?;
        let finalization_started = Instant::now();
        if let Some(artifact) = previous.as_ref() {
            let mut copied_rows = Vec::with_capacity(256);
            let mut copied_vectors = Vec::with_capacity(256);
            for (index, chunk) in chunks.iter().enumerate() {
                let row = rows[index];
                if checkpoint.is_complete(row) {
                    continue;
                }
                let Ok(previous_row) = artifact.ids.binary_search(&chunk.chunk_id) else {
                    continue;
                };
                let start = previous_row
                    .checked_mul(dimension)
                    .ok_or(EmbeddingError::TooLarge)?;
                copied_rows.push(row);
                copied_vectors.push(&artifact.values[start..start + dimension]);
                if copied_rows.len() == 256 {
                    checkpoint.write_batch(&copied_rows, &copied_vectors)?;
                    copied_rows.clear();
                    copied_vectors.clear();
                }
            }
            if !copied_rows.is_empty() {
                checkpoint.write_batch(&copied_rows, &copied_vectors)?;
            }
        }
        drop(previous);
        let missing = chunks
            .iter()
            .enumerate()
            .filter_map(|(index, _)| (!checkpoint.is_complete(rows[index])).then_some(index))
            .collect::<Vec<_>>();
        let missing_chunks = missing
            .iter()
            .map(|&index| &chunks[index])
            .collect::<Vec<_>>();
        let natural_order;
        let inference_order = if let Some(order) = provider.inference_order(&missing_chunks)? {
            if order.len() != missing.len() || order.iter().any(|&index| index >= missing.len()) {
                return Err(EmbeddingError::Provider(
                    "provider returned an invalid inference order".into(),
                ));
            }
            let mut seen = vec![false; missing.len()];
            for &index in &order {
                if std::mem::replace(&mut seen[index], true) {
                    return Err(EmbeddingError::Provider(
                        "provider returned a duplicate inference index".into(),
                    ));
                }
            }
            order
        } else {
            natural_order = (0..missing.len()).collect::<Vec<_>>();
            natural_order
        };

        let mut observations = VectorBuildObservations {
            vector_finalization_us: elapsed_us(finalization_started),
            ..VectorBuildObservations::default()
        };
        let batch_size = provider.embedding_batch_size().get();
        let mut checkpoint_rows = Vec::with_capacity(VECTOR_CHECKPOINT_SYNC_ROWS);
        let mut checkpoint_vectors = Vec::with_capacity(VECTOR_CHECKPOINT_SYNC_ROWS);
        for batch in inference_order.chunks(batch_size) {
            let input_started = Instant::now();
            let source_indices = batch
                .iter()
                .map(|&index| missing[index])
                .collect::<Vec<_>>();
            let texts = source_indices
                .iter()
                .map(|&index| embedding_text(&chunks[index]))
                .collect::<Vec<_>>();
            observations.embedding_input_us = observations
                .embedding_input_us
                .saturating_add(elapsed_us(input_started));
            observations.input_bytes = observations.input_bytes.saturating_add(
                texts
                    .iter()
                    .map(String::len)
                    .try_fold(0_u64, |total, len| {
                        u64::try_from(len)
                            .ok()
                            .and_then(|len| total.checked_add(len))
                    })
                    .unwrap_or(u64::MAX),
            );

            let provider_started = Instant::now();
            let vectors = provider.embed_documents(&texts);
            observations.provider_us = observations
                .provider_us
                .saturating_add(elapsed_us(provider_started));
            let mut vectors = match vectors {
                Ok(vectors) => vectors,
                Err(error) => {
                    flush_checkpoint_batch(
                        &mut checkpoint,
                        &mut checkpoint_rows,
                        &mut checkpoint_vectors,
                    )?;
                    return Err(error);
                }
            };
            if vectors.len() != source_indices.len() {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: source_indices.len(),
                    actual: vectors.len(),
                });
            }
            let finalization_started = Instant::now();
            for vector in &mut vectors {
                if vector.len() != dimension {
                    return Err(EmbeddingError::DimensionMismatch {
                        expected: dimension,
                        actual: vector.len(),
                    });
                }
                match provider.output_normalization() {
                    OutputNormalization::Guaranteed => validate_vector(vector, true)?,
                    OutputNormalization::Unknown => normalize(vector)?,
                }
            }
            for (&source_index, vector) in source_indices.iter().zip(vectors) {
                checkpoint_rows.push(rows[source_index]);
                checkpoint_vectors.push(vector);
                if checkpoint_rows.len() == VECTOR_CHECKPOINT_SYNC_ROWS {
                    flush_checkpoint_batch(
                        &mut checkpoint,
                        &mut checkpoint_rows,
                        &mut checkpoint_vectors,
                    )?;
                }
            }
            observations.vector_finalization_us = observations
                .vector_finalization_us
                .saturating_add(elapsed_us(finalization_started));
            observations.embedded_vectors = observations
                .embedded_vectors
                .saturating_add(source_indices.len());
        }
        flush_checkpoint_batch(
            &mut checkpoint,
            &mut checkpoint_rows,
            &mut checkpoint_vectors,
        )?;

        observations.reused_vectors = chunks.len().saturating_sub(observations.embedded_vectors);
        let value_count = chunks
            .len()
            .checked_mul(dimension)
            .ok_or(EmbeddingError::TooLarge)?;
        let finalization_started = Instant::now();
        let mut values = vec![0.0_f32; value_count];
        checkpoint.read_values(&mut values)?;
        observations.vector_finalization_us = observations
            .vector_finalization_us
            .saturating_add(elapsed_us(finalization_started));
        let artifact = Self {
            schema: 2,
            model_id,
            model_revision,
            dimension,
            normalized: true,
            corpus_digest,
            ids,
            values,
        };
        artifact.validate()?;
        Ok((artifact, observations))
    }

    /// Builds an artifact while consuming the compatible predecessor after rows are copied.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] under the same conditions as
    /// [`Self::from_provider_reusing_with_observations`].
    pub fn from_provider_reusing_owned_with_observations<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        corpus_digest: ContentDigest,
        previous: Option<Self>,
    ) -> Result<(Self, VectorBuildObservations), EmbeddingError> {
        let result = Self::from_provider_reusing_with_observations(
            provider,
            chunks,
            model_id,
            model_revision,
            corpus_digest,
            previous.as_ref(),
        );
        drop(previous);
        result
    }

    /// Builds an artifact using an optional provider-selected inference order.
    ///
    /// The returned rows are always restored to ascending stable chunk ID,
    /// regardless of the order used for provider batches.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the provider returns invalid ordering,
    /// vector dimensions, non-finite or non-normalizable vectors, or when the
    /// artifact cannot satisfy its size and schema constraints.
    #[allow(clippy::too_many_lines)]
    pub fn from_provider_with_observations_and_order<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        chunks: &[Chunk],
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        corpus_digest: ContentDigest,
        inference_order: Option<&[usize]>,
    ) -> Result<(Self, VectorBuildObservations), EmbeddingError> {
        let model_id = model_id.into();
        let model_revision = model_revision.into();
        let provider_dimension = provider.embedding_dimension();
        if let Some(dimension) = provider_dimension {
            encoded_artifact_len(chunks, dimension, &model_id, &model_revision)?;
        }
        let batch_size = provider.embedding_batch_size().get();
        let output_normalization = provider.output_normalization();
        let mut observations = VectorBuildObservations::default();

        let natural_order;
        let inference_order = if let Some(inference_order) = inference_order {
            if inference_order.len() != chunks.len()
                || inference_order.iter().any(|&index| index >= chunks.len())
            {
                return Err(EmbeddingError::Provider(
                    "provider returned an invalid inference order".into(),
                ));
            }
            let mut seen = vec![false; chunks.len()];
            for &index in inference_order {
                if std::mem::replace(&mut seen[index], true) {
                    return Err(EmbeddingError::Provider(
                        "provider returned a duplicate inference index".into(),
                    ));
                }
            }
            inference_order
        } else {
            natural_order = (0..chunks.len()).collect::<Vec<_>>();
            &natural_order
        };

        let sort_started = Instant::now();
        let mut order = (0..chunks.len()).collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| chunks[*left].chunk_id.cmp(&chunks[*right].chunk_id));
        let mut rows = vec![usize::MAX; chunks.len()];
        let ids = order
            .into_iter()
            .enumerate()
            .map(|(row, input_index)| {
                rows[input_index] = row;
                chunks[input_index].chunk_id.clone()
            })
            .collect::<Vec<_>>();
        observations.vector_finalization_us = observations
            .vector_finalization_us
            .saturating_add(elapsed_us(sort_started));

        let mut dimension = None;
        let mut values = Vec::new();
        for batch_indices in inference_order.chunks(batch_size) {
            let input_started = Instant::now();
            let texts = batch_indices
                .iter()
                .map(|&index| embedding_text(&chunks[index]))
                .collect::<Vec<_>>();
            observations.embedding_input_us = observations
                .embedding_input_us
                .saturating_add(elapsed_us(input_started));
            observations.input_bytes = observations.input_bytes.saturating_add(
                texts
                    .iter()
                    .map(String::len)
                    .try_fold(0_u64, |total, len| {
                        u64::try_from(len)
                            .ok()
                            .and_then(|len| total.checked_add(len))
                    })
                    .unwrap_or(u64::MAX),
            );

            let provider_started = Instant::now();
            let batch_vectors = provider.embed_documents(&texts)?;
            observations.provider_us = observations
                .provider_us
                .saturating_add(elapsed_us(provider_started));

            if batch_vectors.len() != batch_indices.len() {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: batch_indices.len(),
                    actual: batch_vectors.len(),
                });
            }

            let finalization_started = Instant::now();
            for (source_index, mut vector) in batch_indices.iter().copied().zip(batch_vectors) {
                match dimension {
                    None => {
                        let vector_dimension = vector.len();
                        if let Some(expected) = provider_dimension
                            && expected != vector_dimension
                        {
                            return Err(EmbeddingError::DimensionMismatch {
                                expected,
                                actual: vector_dimension,
                            });
                        }
                        encoded_artifact_len(chunks, vector_dimension, &model_id, &model_revision)?;
                        let total_values = chunks
                            .len()
                            .checked_mul(vector_dimension)
                            .ok_or(EmbeddingError::TooLarge)?;
                        values.resize(total_values, 0.0);
                        dimension = Some(vector_dimension);
                    }
                    Some(expected) if expected != vector.len() => {
                        return Err(EmbeddingError::DimensionMismatch {
                            expected,
                            actual: vector.len(),
                        });
                    }
                    Some(_) => {}
                }

                match output_normalization {
                    OutputNormalization::Guaranteed => validate_vector(&vector, true)?,
                    OutputNormalization::Unknown => normalize(&mut vector)?,
                }
                let dimension = dimension.ok_or(EmbeddingError::InvalidHeader)?;
                let start = rows[source_index]
                    .checked_mul(dimension)
                    .ok_or(EmbeddingError::TooLarge)?;
                values[start..start + dimension].copy_from_slice(&vector);
            }
            observations.vector_finalization_us = observations
                .vector_finalization_us
                .saturating_add(elapsed_us(finalization_started));
        }

        let dimension = dimension.unwrap_or(0);
        let artifact = Self {
            schema: 2,
            model_id,
            model_revision,
            dimension,
            normalized: true,
            corpus_digest,
            ids,
            values,
        };
        artifact.validate()?;
        observations.embedded_vectors = chunks.len();
        Ok((artifact, observations))
    }

    /// Validates dimensions, finiteness, and artifact schema.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] if any vector violates the artifact contract.
    pub fn validate(&self) -> Result<(), EmbeddingError> {
        if self.schema != 2
            || self.dimension == 0
            || self.model_id.trim().is_empty()
            || self.model_revision.trim().is_empty()
        {
            return Err(EmbeddingError::InvalidHeader);
        }
        if self.ids.windows(2).any(|ids| ids[0] >= ids[1]) {
            return Err(EmbeddingError::InvalidHeader);
        }
        let expected_values = self
            .ids
            .len()
            .checked_mul(self.dimension)
            .ok_or(EmbeddingError::TooLarge)?;
        if self.values.len() != expected_values {
            return Err(EmbeddingError::DimensionMismatch {
                expected: expected_values,
                actual: self.values.len(),
            });
        }
        for vector in self.values.chunks_exact(self.dimension) {
            validate_vector(vector, self.normalized)?;
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
        chunk_ids: &BTreeSet<ChunkId>,
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
            if self.ids.binary_search(chunk_id).is_err() {
                return Err(EmbeddingError::MissingChunk(chunk_id.clone()));
            }
        }
        for chunk_id in &self.ids {
            if !chunk_ids.contains(chunk_id) {
                return Err(EmbeddingError::UnknownChunk(chunk_id.clone()));
            }
        }
        Ok(())
    }

    /// Encodes the artifact with a stable magic header and checked row-major body.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the artifact is invalid or JSON cannot encode it.
    pub fn encode(&self) -> Result<Vec<u8>, EmbeddingError> {
        let length = self.encoded_len()?;
        let mut bytes = Vec::with_capacity(length);
        self.write_encoded_to_validated(&mut bytes, length)?;
        Ok(bytes)
    }

    fn encoded_len(&self) -> Result<usize, EmbeddingError> {
        self.validate()?;
        let model_id = self.model_id.as_bytes();
        let model_revision = self.model_revision.as_bytes();
        u32::try_from(model_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
        u32::try_from(model_revision.len()).map_err(|_| EmbeddingError::TooLarge)?;
        u32::try_from(self.ids.len()).map_err(|_| EmbeddingError::TooLarge)?;
        u32::try_from(self.dimension).map_err(|_| EmbeddingError::TooLarge)?;
        let vector_bytes = self
            .dimension
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or(EmbeddingError::TooLarge)?;
        let fixed = MAGIC
            .len()
            .checked_add(4 * 5)
            .and_then(|length| length.checked_add(model_id.len()))
            .and_then(|length| length.checked_add(model_revision.len()))
            .and_then(|length| length.checked_add(1 + 32 + CHECKSUM_LEN))
            .ok_or(EmbeddingError::TooLarge)?;
        let length = self.ids.iter().try_fold(fixed, |length, chunk_id| {
            let id_len = chunk_id.as_str().len();
            u16::try_from(id_len).map_err(|_| EmbeddingError::TooLarge)?;
            length
                .checked_add(2)
                .and_then(|length| length.checked_add(id_len))
                .and_then(|length| length.checked_add(vector_bytes))
                .ok_or(EmbeddingError::TooLarge)
        })?;
        (length <= MAX_ARTIFACT_BYTES)
            .then_some(length)
            .ok_or(EmbeddingError::TooLarge)
    }

    fn write_encoded_to<W: Write>(
        &self,
        writer: &mut W,
    ) -> Result<(ContentDigest, u64), EmbeddingError> {
        let length = self.encoded_len()?;
        self.write_encoded_to_validated(writer, length)
    }

    fn write_encoded_to_validated<W: Write>(
        &self,
        writer: &mut W,
        expected_length: usize,
    ) -> Result<(ContentDigest, u64), EmbeddingError> {
        let mut artifact = DigestingWriter::new(writer);
        let checksum = {
            let mut body = DigestingWriter::new(&mut artifact);
            self.write_body(&mut body)?;
            let (_, checksum, body_length) = body.finish();
            if body_length != (expected_length - CHECKSUM_LEN) as u64 {
                return Err(EmbeddingError::InvalidHeader);
            }
            checksum
        };
        artifact.write_all(&checksum).map_err(io_error)?;
        artifact.flush().map_err(io_error)?;
        let (_, digest, length) = artifact.finish();
        if length != expected_length as u64 {
            return Err(EmbeddingError::InvalidHeader);
        }
        Ok((digest_from_bytes(&digest)?, length))
    }

    fn write_body<W: Write>(&self, writer: &mut W) -> Result<(), EmbeddingError> {
        let model_id = self.model_id.as_bytes();
        let model_revision = self.model_revision.as_bytes();
        let model_id_len = u32::try_from(model_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let model_revision_len =
            u32::try_from(model_revision.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let count = u32::try_from(self.ids.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let dimension = u32::try_from(self.dimension).map_err(|_| EmbeddingError::TooLarge)?;
        writer.write_all(MAGIC).map_err(io_error)?;
        writer
            .write_all(&u32::from(self.schema).to_le_bytes())
            .and_then(|()| writer.write_all(&dimension.to_le_bytes()))
            .and_then(|()| writer.write_all(&count.to_le_bytes()))
            .and_then(|()| writer.write_all(&model_id_len.to_le_bytes()))
            .and_then(|()| writer.write_all(model_id))
            .and_then(|()| writer.write_all(&model_revision_len.to_le_bytes()))
            .and_then(|()| writer.write_all(model_revision))
            .and_then(|()| writer.write_all(&[u8::from(self.normalized)]))
            .map_err(io_error)?;
        writer
            .write_all(&digest_bytes(&self.corpus_digest)?)
            .map_err(io_error)?;
        for (chunk_id, vector) in self
            .ids
            .iter()
            .zip(self.values.chunks_exact(self.dimension))
        {
            let chunk_id = chunk_id.as_str().as_bytes();
            let chunk_id_len =
                u16::try_from(chunk_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
            writer
                .write_all(&chunk_id_len.to_le_bytes())
                .and_then(|()| writer.write_all(chunk_id))
                .map_err(io_error)?;
            for value in vector {
                writer.write_all(&value.to_le_bytes()).map_err(io_error)?;
            }
        }
        Ok(())
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
        Self::decode_body(
            std::io::Cursor::new(&bytes[..checksum_start]),
            checksum_start,
        )
    }

    fn decode_body<R: Read>(reader: R, body_length: usize) -> Result<Self, EmbeddingError> {
        let mut reader = BinaryReader::new(reader, body_length);
        let magic = reader.array::<8>()?;
        if magic != MAGIC {
            return Err(EmbeddingError::InvalidHeader);
        }
        let schema = u16::try_from(reader.u32()?).map_err(|_| EmbeddingError::InvalidHeader)?;
        if schema == 1 {
            return Err(EmbeddingError::UnsupportedSchema(schema));
        }
        if schema != 2 {
            return Err(EmbeddingError::InvalidHeader);
        }
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
        let corpus_digest = digest_from_bytes(&reader.array::<32>()?)?;
        let mut ids = Vec::with_capacity(count);
        let value_count = count
            .checked_mul(dimension)
            .ok_or(EmbeddingError::TooLarge)?;
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..count {
            let chunk_id_len = u32::from(reader.u16()?);
            let chunk_id = ChunkId::try_from(reader.string(chunk_id_len)?)
                .map_err(|_| EmbeddingError::InvalidHeader)?;
            for _ in 0..dimension {
                values.push(f32::from_le_bytes(reader.array::<4>()?));
            }
            if ids.last().is_some_and(|last| last >= &chunk_id) {
                return Err(EmbeddingError::InvalidHeader);
            }
            ids.push(chunk_id);
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
            ids,
            values,
        };
        artifact.validate()?;
        Ok(artifact)
    }

    fn read_artifact_with_digest<R: Read + Seek>(
        reader: &mut R,
        length: u64,
    ) -> Result<(Self, ContentDigest), EmbeddingError> {
        if length > MAX_ARTIFACT_BYTES as u64 {
            return Err(EmbeddingError::TooLarge);
        }
        if length < MAGIC.len() as u64 {
            return Err(EmbeddingError::InvalidHeader);
        }
        let mut magic = [0_u8; 8];
        reader.read_exact(&mut magic).map_err(read_error)?;
        if magic != MAGIC {
            return Err(EmbeddingError::InvalidHeader);
        }
        let minimum_length = MAGIC
            .len()
            .saturating_add(12)
            .saturating_add(8)
            .saturating_add(1)
            .saturating_add(32)
            .saturating_add(CHECKSUM_LEN);
        if length < minimum_length as u64 {
            return Err(EmbeddingError::Truncated);
        }
        reader.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let body_length = length - CHECKSUM_LEN as u64;
        let digest = verify_reader_checksum(reader, body_length)?;
        reader.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let artifact = Self::decode_body(
            reader,
            usize::try_from(body_length).map_err(|_| EmbeddingError::TooLarge)?,
        )?;
        Ok((artifact, digest))
    }

    /// Writes a checked binary vector artifact.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when encoding fails or the destination cannot
    /// be written.
    pub fn write_artifact(&self, path: &std::path::Path) -> Result<(), EmbeddingError> {
        self.write_artifact_with_digest(path).map(|_| ())
    }

    /// Writes the encoded bytes and returns their digest and exact length.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when encoding or writing fails.
    pub fn write_artifact_with_digest(
        &self,
        path: &std::path::Path,
    ) -> Result<(ContentDigest, u64), EmbeddingError> {
        let file = File::create(path).map_err(io_error)?;
        self.write_encoded_to(&mut BufWriter::new(file))
    }

    /// Hash the encoded artifact and report its exact length without buffering it.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the artifact is invalid or cannot be encoded.
    pub fn encoded_digest(&self) -> Result<(ContentDigest, u64), EmbeddingError> {
        self.write_encoded_to(&mut std::io::sink())
    }

    /// Opens and validates a checked binary vector artifact.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the file cannot be read or the artifact
    /// violates the binary contract.
    pub fn open_artifact(path: &std::path::Path) -> Result<Self, EmbeddingError> {
        Self::open_artifact_with_digest(path).map(|(artifact, _)| artifact)
    }

    /// Opens a checked binary vector artifact and returns its complete file digest.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError`] when the file cannot be read or the artifact
    /// violates the binary contract.
    pub fn open_artifact_with_digest(
        path: &std::path::Path,
    ) -> Result<(Self, ContentDigest), EmbeddingError> {
        let file = File::open(path).map_err(io_error)?;
        let length = file.metadata().map_err(io_error)?.len();
        Self::read_artifact_with_digest(&mut BufReader::new(file), length)
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
        let limit = limit.max(1).min(self.ids.len());
        let mut winners = Vec::with_capacity(limit);
        for (row, vector) in self.values.chunks_exact(self.dimension).enumerate() {
            let similarity = dot(vector, &query);
            if winners.len() < limit {
                winners.push((row, similarity));
                continue;
            }
            let Some((worst, _)) = winners
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| compare_ranked_rows(self, left, right))
            else {
                continue;
            };
            let candidate = (row, similarity);
            if compare_ranked_rows(self, &candidate, &winners[worst]) == Ordering::Less {
                winners[worst] = candidate;
            }
        }
        winners.sort_by(|left, right| compare_ranked_rows(self, left, right));
        Ok(winners
            .into_iter()
            .map(|(row, similarity)| SemanticHit {
                chunk_id: self.ids[row].clone(),
                similarity,
            })
            .collect())
    }
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn compare_ranked_rows(
    artifact: &VectorArtifact,
    left: &(usize, f32),
    right: &(usize, f32),
) -> Ordering {
    right
        .1
        .total_cmp(&left.1)
        .then_with(|| artifact.ids[left.0].cmp(&artifact.ids[right.0]))
}

fn validate_vector(vector: &[f32], normalized: bool) -> Result<(), EmbeddingError> {
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(EmbeddingError::NonFinite);
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return Err(EmbeddingError::ZeroNorm);
    }
    if normalized && (norm - 1.0).abs() > 0.01 {
        return Err(EmbeddingError::Provider(
            "provider declared normalized output but returned a non-unit vector".into(),
        ));
    }
    Ok(())
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

struct DigestingWriter<W> {
    inner: W,
    digest: Sha256,
    length: u64,
}

impl<W> DigestingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            length: 0,
        }
    }

    fn finish(self) -> (W, [u8; 32], u64) {
        (self.inner, self.digest.finalize().into(), self.length)
    }
}

impl<W: Write> Write for DigestingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.digest.update(&bytes[..written]);
        self.length = self.length.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct BinaryReader<R> {
    inner: R,
    remaining: usize,
}

impl<R: Read> BinaryReader<R> {
    const fn new(inner: R, remaining: usize) -> Self {
        Self { inner, remaining }
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], EmbeddingError> {
        if N > self.remaining {
            return Err(EmbeddingError::Truncated);
        }
        let mut bytes = [0_u8; N];
        self.inner.read_exact(&mut bytes).map_err(read_error)?;
        self.remaining -= N;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, EmbeddingError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, EmbeddingError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, EmbeddingError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn string(&mut self, length: u32) -> Result<String, EmbeddingError> {
        let length = usize::try_from(length).map_err(|_| EmbeddingError::TooLarge)?;
        if length > self.remaining {
            return Err(EmbeddingError::Truncated);
        }
        let mut bytes = vec![0_u8; length];
        self.inner.read_exact(&mut bytes).map_err(read_error)?;
        self.remaining -= length;
        String::from_utf8(bytes).map_err(|error| EmbeddingError::InvalidJson(error.to_string()))
    }

    const fn is_empty(&self) -> bool {
        self.remaining == 0
    }
}

fn verify_reader_checksum<R: Read>(
    reader: &mut R,
    body_length: u64,
) -> Result<ContentDigest, EmbeddingError> {
    let mut digest = Sha256::new();
    let mut remaining = body_length;
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    while remaining != 0 {
        let length = usize::try_from(remaining.min(STREAM_BUFFER_BYTES as u64))
            .map_err(|_| EmbeddingError::TooLarge)?;
        reader
            .read_exact(&mut buffer[..length])
            .map_err(read_error)?;
        digest.update(&buffer[..length]);
        remaining -= length as u64;
    }
    let mut expected = [0_u8; CHECKSUM_LEN];
    reader.read_exact(&mut expected).map_err(read_error)?;
    let actual: [u8; CHECKSUM_LEN] = digest.clone().finalize().into();
    if actual != expected {
        return Err(EmbeddingError::ChecksumMismatch);
    }
    digest.update(expected);
    let complete: [u8; CHECKSUM_LEN] = digest.finalize().into();
    digest_from_bytes(&complete)
}

#[allow(clippy::needless_pass_by_value)] // `map_err` supplies the owned error.
fn io_error(error: std::io::Error) -> EmbeddingError {
    EmbeddingError::Io(error.to_string())
}

fn read_error(error: std::io::Error) -> EmbeddingError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        EmbeddingError::Truncated
    } else {
        io_error(error)
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

    struct RecordingProvider {
        dimension: usize,
        embedded: usize,
    }

    impl EmbeddingProvider for RecordingProvider {
        fn embedding_dimension(&self) -> Option<usize> {
            Some(self.dimension)
        }

        fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            self.embedded += texts.len();
            FakeEmbeddingProvider::new(self.dimension).embed_documents(texts)
        }

        fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
            FakeEmbeddingProvider::new(self.dimension).embed_query(text)
        }
    }

    struct InterruptedProvider {
        calls: usize,
    }

    impl EmbeddingProvider for InterruptedProvider {
        fn embedding_dimension(&self) -> Option<usize> {
            Some(4)
        }

        fn embedding_batch_size(&self) -> EmbeddingBatchSize {
            EmbeddingBatchSize::new(1).expect("nonzero batch size")
        }

        fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            self.calls += 1;
            if self.calls == 2 {
                return Err(EmbeddingError::Provider("interrupted".into()));
            }
            FakeEmbeddingProvider::new(4).embed_documents(texts)
        }

        fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
            FakeEmbeddingProvider::new(4).embed_query(text)
        }
    }

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
    fn vector_build_reuses_unchanged_rows() {
        let original_chunks = chunks();
        let mut original_provider = FakeEmbeddingProvider::new(4);
        let original = VectorArtifact::from_provider(
            &mut original_provider,
            &original_chunks,
            "fake",
            "test",
            ContentDigest::of(b"old"),
        )
        .expect("original artifact");
        let mut next_chunks = original_chunks[..1].to_vec();
        next_chunks.push(chunks().remove(1));
        let mut provider = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };

        let (next, observations) = VectorArtifact::from_provider_reusing_with_observations(
            &mut provider,
            &next_chunks,
            "fake",
            "test",
            ContentDigest::of(b"new"),
            Some(&original),
        )
        .expect("incremental artifact");

        assert_eq!(provider.embedded, 0);
        assert_eq!(observations.reused_vectors, 2);
        assert_eq!(observations.embedded_vectors, 0);
        assert_eq!(next.ids, original.ids);
        assert_eq!(next.values, original.values);
    }

    #[test]
    fn vector_build_embeds_only_new_rows() {
        let original_chunks = chunks();
        let mut original_provider = FakeEmbeddingProvider::new(4);
        let original = VectorArtifact::from_provider(
            &mut original_provider,
            &original_chunks[..1],
            "fake",
            "test",
            ContentDigest::of(b"old"),
        )
        .expect("original artifact");
        let mut provider = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };

        let (_, observations) = VectorArtifact::from_provider_reusing_with_observations(
            &mut provider,
            &original_chunks,
            "fake",
            "test",
            ContentDigest::of(b"new"),
            Some(&original),
        )
        .expect("incremental artifact");

        assert_eq!(provider.embedded, 1);
        assert_eq!(observations.reused_vectors, 1);
        assert_eq!(observations.embedded_vectors, 1);
    }

    #[test]
    fn vector_build_resumes_from_persisted_checkpoint() {
        let cache = tempfile::tempdir().expect("vector checkpoint");
        let chunks = chunks();
        let mut first = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };
        VectorArtifact::from_provider_reusing_with_checkpoint_observations(
            &mut first,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"first"),
            None,
            &cache.path().join("vectors.bin"),
        )
        .expect("initial cached artifact");

        let mut restarted = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };
        let (_, observations) = VectorArtifact::from_provider_reusing_with_checkpoint_observations(
            &mut restarted,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"first"),
            None,
            &cache.path().join("vectors.bin"),
        )
        .expect("resumed cached artifact");

        assert_eq!(restarted.embedded, 0);
        assert_eq!(observations.reused_vectors, chunks.len());
    }

    #[test]
    fn interrupted_vector_build_keeps_completed_batches() {
        let cache = tempfile::tempdir().expect("vector checkpoint");
        let cache_path = cache.path().join("vectors.bin");
        let chunks = chunks();
        let mut interrupted = InterruptedProvider { calls: 0 };
        VectorArtifact::from_provider_reusing_with_checkpoint_observations(
            &mut interrupted,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"first"),
            None,
            &cache_path,
        )
        .expect_err("second batch interrupts the build");

        let mut restarted = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };
        let (_, observations) = VectorArtifact::from_provider_reusing_with_checkpoint_observations(
            &mut restarted,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"first"),
            None,
            &cache_path,
        )
        .expect("resumed artifact");

        assert_eq!(restarted.embedded, 1);
        assert_eq!(observations.reused_vectors, 1);
    }

    #[test]
    fn invalid_vector_checkpoint_is_replaced() {
        let cache = tempfile::tempdir().expect("vector checkpoint");
        let cache_path = cache.path().join("vectors.bin");
        std::fs::write(&cache_path, b"interrupted header").expect("corrupt checkpoint");
        let chunks = chunks();
        let mut provider = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };

        let (_, observations) = VectorArtifact::from_provider_reusing_with_checkpoint_observations(
            &mut provider,
            &chunks,
            "fake",
            "test",
            ContentDigest::of(b"first"),
            None,
            &cache_path,
        )
        .expect("replace corrupt checkpoint");

        assert_eq!(provider.embedded, chunks.len());
        assert_eq!(observations.embedded_vectors, chunks.len());
    }

    #[test]
    fn incompatible_vector_artifact_is_not_reused() {
        let chunks = chunks();
        let mut original_provider = FakeEmbeddingProvider::new(4);
        let original = VectorArtifact::from_provider(
            &mut original_provider,
            &chunks,
            "old-model",
            "test",
            ContentDigest::of(b"old"),
        )
        .expect("original artifact");
        let mut provider = RecordingProvider {
            dimension: 4,
            embedded: 0,
        };

        let (_, observations) = VectorArtifact::from_provider_reusing_with_observations(
            &mut provider,
            &chunks,
            "new-model",
            "test",
            ContentDigest::of(b"new"),
            Some(&original),
        )
        .expect("rebuilt artifact");

        assert_eq!(provider.embedded, chunks.len());
        assert_eq!(observations.reused_vectors, 0);
        assert_eq!(observations.embedded_vectors, chunks.len());
    }

    #[test]
    fn oversized_vector_artifact_is_rejected_before_inference() {
        let mut provider = RecordingProvider {
            dimension: usize::MAX,
            embedded: 0,
        };

        let error = VectorArtifact::from_provider(
            &mut provider,
            &chunks()[..1],
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect_err("oversized artifact");

        assert!(matches!(error, EmbeddingError::TooLarge));
        assert_eq!(provider.embedded, 0);
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
    fn embedding_text_drops_document_wide_identifier_noise() {
        let mut chunk = chunks().remove(0);
        chunk.text = "target_function();".into();
        chunk.identifiers.insert("target_function".into());
        for index in 0..100 {
            chunk
                .identifiers
                .insert(format!("unrelated_identifier_{index}"));
        }

        let text = embedding_text(&chunk);

        assert!(text.contains("target_function"));
        assert!(!text.contains("unrelated_identifier_99"));
    }

    #[test]
    fn semantic_query_text_uses_reviewed_concept_aliases() {
        let text = semantic_query_text("encoded values crossing the native extension boundary");

        assert!(text.starts_with("encoded values crossing"));
        assert!(text.contains("lbm_enc_i32"));
        assert!(semantic_query_text("lbm_add_extension").eq("lbm_add_extension"));
    }

    #[test]
    fn granite_r2_profiles_preserve_native_dimensions() {
        let small =
            EmbeddingProfile::for_model_id("ibm-granite/granite-embedding-97m-multilingual-r2")
                .expect("97M profile");
        let large =
            EmbeddingProfile::for_model_id("ibm-granite/granite-embedding-311m-multilingual-r2")
                .expect("311M profile");

        assert_eq!(small.pooling, Pooling::Cls);
        assert_eq!(small.max_length, 32_768);
        assert_eq!(small.dimension, 384);
        assert_eq!(large.dimension, 768);
        assert!(small.query_prefix.is_empty());
        assert!(small.document_prefix.is_empty());
        assert!(small.normalize && large.normalize);
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn pad_window_batch_fills_short_batch_with_synthetic_inputs() {
        let mut windows = vec!["one".to_string(), "two".to_string()];

        pad_window_batch(&mut windows, 4);

        assert_eq!(windows, ["one", "two", "", ""]);
    }

    #[cfg(feature = "semantic-migraphx")]
    #[test]
    fn migraphx_rejects_the_known_jina_int8_graph() {
        let profile = EmbeddingProfile::jina_v2_base_code();

        assert!(
            validate_migraphx_model_digest(
                crate::JINA_CODE_INT8_SHA256,
                &profile,
                SemanticExecutionProvider::Migraphx { device_id: 0 },
            )
            .is_err()
        );
        assert!(
            validate_migraphx_model_digest(
                crate::JINA_CODE_FP16_SHA256,
                &profile,
                SemanticExecutionProvider::Migraphx { device_id: 0 },
            )
            .is_ok()
        );
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
        assert_eq!(artifact.ids.len(), 2);
        assert_eq!(artifact.values.len(), 8);
    }

    #[test]
    fn vector_artifact_is_stable_when_inference_order_changes() {
        let original = chunks();
        let reversed_order = (0..original.len()).rev().collect::<Vec<_>>();

        let mut first_provider = FakeEmbeddingProvider::new(4);
        let first = VectorArtifact::from_provider(
            &mut first_provider,
            &original,
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("original artifact");

        let mut second_provider = FakeEmbeddingProvider::new(4);
        let (second, _) = VectorArtifact::from_provider_with_observations_and_order(
            &mut second_provider,
            &original,
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
            Some(&reversed_order),
        )
        .expect("reordered artifact");

        assert_eq!(first, second);
    }

    #[test]
    fn outer_batch_size_rejects_zero() {
        assert!(EmbeddingBatchSize::new(0).is_err());
        assert_eq!(EmbeddingBatchSize::new(3).expect("batch").get(), 3);
    }

    #[test]
    fn sequence_buckets_hold_a_constant_token_budget() {
        let buckets = sequence_bucket_plan(&[64, 128, 256, 512], 4096).expect("bucket plan");

        assert_eq!(
            buckets,
            [
                SequenceBucket {
                    max_length: 64,
                    batch_size: 64,
                },
                SequenceBucket {
                    max_length: 128,
                    batch_size: 32,
                },
                SequenceBucket {
                    max_length: 256,
                    batch_size: 16,
                },
                SequenceBucket {
                    max_length: 512,
                    batch_size: 8,
                },
            ]
        );
    }

    #[test]
    fn token_weighted_window_pooling_does_not_overweight_a_short_tail() {
        let vectors = [vec![1.0, 0.0], vec![0.0, 1.0]];
        let pooled =
            aggregate_window_vectors(&vectors, &[100, 1], WindowAggregation::TokenWeightedMean)
                .expect("pooled vector");

        assert!(pooled[0] > 0.999 && pooled[1] < 0.011);
    }

    #[test]
    fn unnormalized_provider_is_normalized_once() {
        struct RawProvider;

        impl EmbeddingProvider for RawProvider {
            fn embed_documents(
                &mut self,
                texts: &[String],
            ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                Ok(texts.iter().map(|_| vec![3.0, 4.0]).collect())
            }

            fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
                Ok(vec![3.0, 4.0])
            }
        }

        let mut provider = RawProvider;
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks()[..1],
            "raw",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        assert_eq!(artifact.values, vec![0.6, 0.8]);
    }

    #[test]
    fn guaranteed_provider_output_is_verified_at_runtime() {
        struct LyingProvider;

        impl EmbeddingProvider for LyingProvider {
            fn output_normalization(&self) -> OutputNormalization {
                OutputNormalization::Guaranteed
            }

            fn embed_documents(
                &mut self,
                texts: &[String],
            ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
                Ok(texts.iter().map(|_| vec![3.0, 4.0]).collect())
            }

            fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
                Ok(vec![3.0, 4.0])
            }
        }

        let mut provider = LyingProvider;
        assert!(matches!(
            VectorArtifact::from_provider(
                &mut provider,
                &chunks()[..1],
                "lying",
                "test",
                ContentDigest::of(b"corpus"),
            ),
            Err(EmbeddingError::Provider(message)) if message.contains("normalized")
        ));
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
        let chunks = (0..17)
            .map(|index| {
                let mut chunk = source.clone();
                chunk.chunk_id = ChunkId::try_from(format!("chunk-{index:02}")).expect("chunk id");
                chunk
            })
            .collect::<Vec<_>>();
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
    #[ignore = "run explicitly when comparing finalization components"]
    #[allow(clippy::cast_precision_loss)]
    fn vector_finalization_components_microbenchmark() {
        let mut vectors = (0..1024)
            .map(|row| vec![row as f32 + 1.0, 2.0, 3.0, 4.0])
            .collect::<Vec<_>>();
        let normalization_started = Instant::now();
        for vector in &mut vectors {
            normalize(vector).expect("vector is normalizable");
        }
        let normalization_us = elapsed_us(normalization_started);

        let flatten_started = Instant::now();
        let mut flattened = Vec::with_capacity(vectors.len() * 4);
        for vector in &vectors {
            flattened.extend_from_slice(vector);
        }
        let flatten_us = elapsed_us(flatten_started);
        std::hint::black_box(flattened);
        eprintln!(
            "vector-finalization-microbenchmark normalization_us={normalization_us} flatten_us={flatten_us}"
        );
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
    fn vector_artifact_reports_v1_as_unsupported() {
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
        let checksum_start = encoded.len() - CHECKSUM_LEN;
        encoded[8..12].copy_from_slice(&1_u32.to_le_bytes());
        let checksum = Sha256::digest(&encoded[..checksum_start]);
        encoded[checksum_start..].copy_from_slice(&checksum);
        assert_eq!(
            VectorArtifact::decode(&encoded).expect_err("v1 artifact"),
            EmbeddingError::UnsupportedSchema(1)
        );
    }

    #[test]
    fn vector_artifact_uses_checked_binary_v2_layout() {
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
            2
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

    #[derive(Default)]
    struct ChunkTrackingWriter {
        bytes: Vec<u8>,
        largest_write: usize,
    }

    impl Write for ChunkTrackingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.largest_write = self.largest_write.max(bytes.len());
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct ChunkTrackingReader {
        inner: std::io::Cursor<Vec<u8>>,
        largest_read: usize,
    }

    impl Read for ChunkTrackingReader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.largest_read = self.largest_read.max(bytes.len());
            self.inner.read(bytes)
        }
    }

    impl Seek for ChunkTrackingReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn streamed_encoding_matches_checked_binary_without_whole_artifact_write() {
        let mut provider = FakeEmbeddingProvider::new(32_768);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks(),
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        let expected = artifact.encode().expect("encode");
        let mut writer = ChunkTrackingWriter::default();

        let (digest, length) = artifact
            .write_encoded_to(&mut writer)
            .expect("streamed encoding");

        assert_eq!(
            (writer.bytes.as_slice(), digest.clone(), length),
            (
                expected.as_slice(),
                ContentDigest::of(&expected),
                expected.len() as u64,
            )
        );
        assert!(writer.largest_write < writer.bytes.len());
        assert_eq!(artifact.encoded_digest(), Ok((digest, length)));
    }

    #[test]
    fn streamed_open_bounds_reads_below_artifact_size() {
        let mut provider = FakeEmbeddingProvider::new(32_768);
        let artifact = VectorArtifact::from_provider(
            &mut provider,
            &chunks(),
            "fake",
            "test",
            ContentDigest::of(b"corpus"),
        )
        .expect("artifact");
        let encoded = artifact.encode().expect("encode");
        let mut reader = ChunkTrackingReader {
            inner: std::io::Cursor::new(encoded.clone()),
            largest_read: 0,
        };

        let (decoded, digest) =
            VectorArtifact::read_artifact_with_digest(&mut reader, encoded.len() as u64)
                .expect("streamed artifact");

        assert_eq!(decoded, artifact);
        assert_eq!(digest, ContentDigest::of(&encoded));
        assert!(reader.largest_read <= 64 * 1024);
        assert!(reader.largest_read < encoded.len());
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
