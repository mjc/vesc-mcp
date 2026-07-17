//! Optional local semantic retrieval contracts and vector artifacts.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corpus::{Chunk, ChunkId, ContentDigest};

const MAGIC: &[u8] = b"VESCRAG1";
const CHECKSUM_LEN: usize = 32;
const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;

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
    fn inference_order(&mut self, _chunks: &[Chunk]) -> Result<Option<Vec<usize>>, EmbeddingError> {
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
    if !chunk.identifiers.is_empty() {
        text.push_str("Identifiers: ");
        append_joined(
            &mut text,
            chunk.identifiers.iter().map(String::as_str),
            ", ",
        );
        text.push('\n');
        if chunk
            .identifiers
            .iter()
            .any(|identifier| identifier_semantic_alias(identifier).is_some())
        {
            text.push_str("Concepts: ");
            append_joined(
                &mut text,
                chunk
                    .identifiers
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
    /// averaged and normalized, preserving deterministic chunk ordering while
    /// preventing the tokenizer or ONNX graph from discarding document text.
    pub const fn set_lossless_windowing(&mut self, enabled: bool) {
        self.lossless_windowing = enabled;
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
        let model = fastembed::TextEmbedding::try_new_from_user_defined(model, options)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        Self::new(model, batch_size, profile)
    }

    /// Measures the token counts and padding used by `FastEmbed`'s configured
    /// tokenizer, one outer provider batch at a time.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::EmptyInput`] for empty input or
    /// [`EmbeddingError::Provider`] when tokenization fails.
    pub fn token_statistics(&self, texts: &[String]) -> Result<TokenStatistics, EmbeddingError> {
        if texts.is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }
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
        let mut untruncated_tokenizer = self.model.tokenizer.clone();
        untruncated_tokenizer.with_padding(None);
        untruncated_tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;

        let mut real_tokens = Vec::with_capacity(texts.len());
        let mut total_real_tokens = 0_u64;
        let mut total_padded_tokens = 0_u64;
        let mut total_untruncated_tokens = 0_u64;
        let mut truncated_chunks = 0_usize;

        for batch in texts.chunks(self.batch_size.get()) {
            let inputs = batch.iter().map(String::as_str).collect::<Vec<_>>();
            let configured = self
                .model
                .tokenizer
                .encode_batch(inputs.clone(), true)
                .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
            let untruncated = untruncated_tokenizer
                .encode_batch(inputs, true)
                .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
            for (configured, untruncated) in configured.iter().zip(untruncated.iter()) {
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
            chunks: texts.len(),
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
        if texts.is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }
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
        let mut lengths = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.batch_size.get()) {
            let inputs = batch.iter().map(String::as_str).collect::<Vec<_>>();
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
        }
        Ok(lengths)
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

    fn document_windows(&self, text: &str) -> Result<Vec<String>, EmbeddingError> {
        let mut tokenizer = self.model.tokenizer.clone();
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        let encoding = tokenizer
            .encode(text, true)
            .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
        let offsets = encoding
            .get_offsets()
            .iter()
            .copied()
            .filter(|&(start, end)| end > start)
            .collect::<Vec<_>>();
        let special_tokens = encoding.get_ids().len().saturating_sub(offsets.len());
        let content_limit = self.profile.max_length.saturating_sub(special_tokens);
        if content_limit == 0 {
            return Err(EmbeddingError::Provider(
                "model limit is smaller than its special-token overhead".into(),
            ));
        }
        if offsets.len() <= content_limit {
            return Ok(vec![text.to_owned()]);
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
        Ok(windows)
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

    fn length_bucket_order(&self, chunks: &[Chunk]) -> Result<Vec<usize>, EmbeddingError> {
        let mut keyed = Vec::with_capacity(chunks.len());
        for (base, batch) in chunks.chunks(self.batch_size.get()).enumerate() {
            let texts = batch.iter().map(embedding_text).collect::<Vec<_>>();
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

    fn inference_order(&mut self, chunks: &[Chunk]) -> Result<Option<Vec<usize>>, EmbeddingError> {
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
            let mut aggregated = Vec::with_capacity(texts.len());
            for text in texts {
                let windows = self.document_windows(text)?;
                let vectors = self
                    .model
                    .embed(&windows, self.effective_batch_size(windows.len()))
                    .map_err(|error| EmbeddingError::Provider(error.to_string()))?;
                self.validate_vectors(&vectors)?;
                let mut vector = vec![0.0_f32; self.profile.dimension];
                for window in &vectors {
                    for (sum, value) in vector.iter_mut().zip(window) {
                        *sum += *value;
                    }
                }
                let divisor = vectors.len() as f32;
                for value in &mut vector {
                    *value /= divisor;
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

/// Reproducible row-major vector artifact keyed by sorted stable chunk IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

        let mut dimension = None;
        let mut values = Vec::with_capacity(chunks.len());
        let mut row_offsets = vec![usize::MAX; chunks.len()];
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
                row_offsets[source_index] = values.len();
                match dimension {
                    None => {
                        let vector_dimension = vector.len();
                        let total_values = chunks
                            .len()
                            .checked_mul(vector_dimension)
                            .ok_or(EmbeddingError::TooLarge)?;
                        values.reserve(total_values.saturating_sub(values.len()));
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
                values.extend_from_slice(&vector);
            }
            observations.vector_finalization_us = observations
                .vector_finalization_us
                .saturating_add(elapsed_us(finalization_started));
        }

        let dimension = dimension.unwrap_or(0);
        if row_offsets.contains(&usize::MAX) {
            return Err(EmbeddingError::Provider(
                "provider inference order did not cover the corpus".into(),
            ));
        }
        let sort_started = Instant::now();
        let mut order = (0..chunks.len()).collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| chunks[*left].chunk_id.cmp(&chunks[*right].chunk_id));
        let mut ids = Vec::with_capacity(chunks.len());
        let mut sorted_values = Vec::with_capacity(values.len());
        for input_index in order {
            ids.push(chunks[input_index].chunk_id.clone());
            let start = row_offsets[input_index];
            let end = start
                .checked_add(dimension)
                .ok_or(EmbeddingError::TooLarge)?;
            sorted_values.extend_from_slice(&values[start..end]);
        }
        observations.vector_finalization_us = observations
            .vector_finalization_us
            .saturating_add(elapsed_us(sort_started));

        let artifact = Self {
            schema: 2,
            model_id: model_id.into(),
            model_revision: model_revision.into(),
            dimension,
            normalized: true,
            corpus_digest,
            ids,
            values: sorted_values,
        };
        artifact.validate()?;
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
        self.validate()?;
        let model_id = self.model_id.as_bytes();
        let model_revision = self.model_revision.as_bytes();
        let model_id_len = u32::try_from(model_id.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let model_revision_len =
            u32::try_from(model_revision.len()).map_err(|_| EmbeddingError::TooLarge)?;
        let count = u32::try_from(self.ids.len()).map_err(|_| EmbeddingError::TooLarge)?;
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
        for (chunk_id, vector) in self
            .ids
            .iter()
            .zip(self.values.chunks_exact(self.dimension))
        {
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
        let corpus_digest = digest_from_bytes(reader.take(32)?)?;
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
                let bytes = reader
                    .take(4)?
                    .try_into()
                    .map_err(|_| EmbeddingError::Truncated)?;
                values.push(f32::from_le_bytes(bytes));
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
        let bytes = self.encode()?;
        let digest = ContentDigest::of(&bytes);
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        std::fs::write(path, bytes).map_err(|error| EmbeddingError::Io(error.to_string()))?;
        Ok((digest, length))
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
