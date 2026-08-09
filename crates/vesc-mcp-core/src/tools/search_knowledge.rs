//! `search_vesc_knowledge` — search the embedded firmware and package knowledge index.

use crate::config::{
    DEFAULT_KNOWLEDGE_MAX_PASSAGE_BYTES, DEFAULT_KNOWLEDGE_MAX_RESPONSE_BYTES, KnowledgeConfig,
    RetrievalMode,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(any(feature = "semantic-fastembed", test))]
use std::sync::Condvar;
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(feature = "semantic-fastembed")]
use std::sync::{MutexGuard, Once};
#[cfg(any(feature = "semantic-fastembed", test))]
use std::time::Duration;
use std::time::Instant;
use vesc_knowledge_index::{
    Category, ExpandedContext, FusedCandidate, FusedHit, FusionConfig, LexicalCandidate,
    LexicalHit, LexicalIndex, RetrievalMetadata, SemanticHit, expand_adjacent_context,
    fuse_candidate_metadata,
};
#[cfg(any(feature = "semantic-fastembed", test))]
use vesc_knowledge_index::{EmbeddingProvider, FileBackedVectorArtifact, semantic_query_text};

use crate::{
    resources::ResourceRegistry,
    tools::knowledge_feedback::{FeedbackStore, KnowledgeCorrectionResult, search_feedback},
};
/// Retrieval backend selected for a knowledge search.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq,
)]
#[schemars(inline)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Use Tantivy over normalized chunks.
    #[default]
    Lexical,
    /// Require semantic retrieval and fuse it with lexical evidence.
    Hybrid,
    /// Select the staged default configured by the server.
    Auto,
}

/// Amount of detail returned by the search serialization boundary.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq,
)]
#[schemars(inline)]
#[serde(rename_all = "snake_case")]
pub enum SearchResponseDetail {
    /// Return provenance, bounded passages, and diagnostics in the first response.
    #[default]
    Full,
    /// Return bounded ranked rows when lower response cost is more important than context.
    Compact,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchVescKnowledgeParams {
    /// Free-text query matched against entry names, keywords, and summaries.
    /// The built-in maximum is 4,096 UTF-8 bytes; the effective configured
    /// maximum is returned in `index.limits.max_query_bytes`.
    pub query: String,
    /// Immutable snapshot returned by `prepare_vesc_knowledge`.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub snapshot_id: Option<String>,
    /// Maximum number of hits to return (default 10).
    #[serde(default = "default_search_limit")]
    #[schemars(range(min = 1), default = "default_search_limit")]
    pub limit: usize,
    /// Retrieval mode. Omit to use the server default; auto keeps lexical
    /// evidence in the same response when semantic retrieval is unavailable.
    #[serde(default)]
    pub mode: Option<SearchMode>,
    /// Additive filters for lexical/hybrid retrieval.
    #[serde(default)]
    pub filters: SearchVescKnowledgeFilters,
    /// Maximum response JSON size. The built-in default and maximum are 65,536
    /// bytes; the effective configured maximum is returned in
    /// `index.limits.max_response_bytes`.
    #[serde(default)]
    #[schemars(range(min = 1), default = "default_max_response_bytes")]
    pub max_response_bytes: Option<usize>,
    /// Maximum bytes retained in each returned evidence passage. The built-in
    /// default and maximum are 8,192 bytes; the effective configured maximum
    /// is returned in `index.limits.max_context_bytes`.
    #[serde(default)]
    #[schemars(range(min = 1), default = "default_max_context_bytes")]
    pub max_context_bytes: Option<usize>,
    /// Response detail; defaults to full evidence. Use compact for an explicit low-token query.
    #[serde(default)]
    pub detail: SearchResponseDetail,
}

/// Empty request for retrieving the effective search contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchVescKnowledgeCapabilitiesParams {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
#[serde(deny_unknown_fields)]
pub struct SearchVescKnowledgeFilters {
    /// Category filter. Unrecognized values are ignored.
    #[serde(default)]
    pub category: Option<String>,
    /// Exact repository identifier, such as `refloat` or `vesc-rust-poc`.
    #[serde(default)]
    pub repository: Option<String>,
    /// Exact source paths. Multiple paths are additive alternatives.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Exact immutable source revision filter.
    #[serde(default)]
    pub revision: Option<String>,
    /// Exact trust classification: `first_party`, `curated_upstream`,
    /// `fixture`, or `unverified_model_feedback`.
    #[serde(default)]
    pub trust_tier: Option<String>,
    /// Exact source family, such as `git_blob`, `git_commit`, or `markdown`.
    #[serde(default)]
    pub source_kind: Option<String>,
    /// Additive tag filters; every supplied tag must be present.
    #[serde(default)]
    pub tags: Vec<String>,
}

const fn default_search_limit() -> usize {
    10
}

const fn default_max_response_bytes() -> usize {
    DEFAULT_KNOWLEDGE_MAX_RESPONSE_BYTES
}

const fn default_max_context_bytes() -> usize {
    DEFAULT_KNOWLEDGE_MAX_PASSAGE_BYTES
}

const COMPACT_EXCERPT_BYTES: usize = 384;
const COMPACT_FIELDS: [&str; 7] = [
    "name",
    "category",
    "excerpt",
    "source_index",
    "chunk_id",
    "correction_ids",
    "origin",
];

/// Keep the enclosing symbol and nearby caller/test context in full results.
/// The per-passage byte budget still bounds the resulting response.
const MAX_CONTEXT_NEIGHBORS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeSource {
    pub repo: String,
    pub path: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_byte: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_byte: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
pub struct SearchVescKnowledgeResult {
    pub id: String,
    pub name: String,
    pub category: String,
    pub summary: String,
    pub source: SearchVescKnowledgeSource,
    pub score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_path: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_uri: Option<String>,
    /// Normalized retrieval score when the selected backend exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_score: Option<f64>,
    /// Origin for non-curated runtime feedback results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Relevant correction annotations for this exact result/resource identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub correction_ids: Vec<String>,
    /// Stable passage and source identity for citation/follow-up reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<SearchVescKnowledgeProvenance>,
    /// Deterministic explanation of the ranking stages that contributed this hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<SearchVescKnowledgeExplanation>,
    /// Bounded history information merged when identical evidence occurs in
    /// multiple revisions. The retained row remains the preferred ranked hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<SearchVescKnowledgeOccurrence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeOccurrence {
    pub count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revisions: Vec<String>,
    pub first_revision: Option<String>,
    pub last_revision: Option<String>,
    /// Stable handle for expanding the representative passage/document.
    pub representative_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
pub struct SearchVescKnowledgeExplanation {
    pub lexical_rank: Option<usize>,
    pub lexical_score: Option<f64>,
    pub semantic_rank: Option<usize>,
    pub semantic_similarity: Option<f64>,
    pub fusion_score: Option<f64>,
    pub exact_identifier: bool,
    /// Filters applied before ranking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter_effects: Vec<String>,
    /// Bounded neighboring-context decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expansion_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeProvenance {
    pub document_id: String,
    pub chunk_id: String,
    pub passage: String,
    pub heading_path: Vec<String>,
    pub resource_uri: Option<String>,
    pub revision: Option<String>,
    pub source_span: Option<SearchVescKnowledgeSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeSpan {
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: Option<u64>,
    pub end_byte: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeIndex {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_profile: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repositories: BTreeMap<String, String>,
    pub corpus_version: String,
    pub corpus_digest: Option<String>,
    pub document_count: usize,
    pub chunk_count: usize,
    pub source_count: usize,
    pub diagnostic_count: usize,
    pub component_versions: BTreeMap<String, String>,
    pub lexical_checksum: Option<String>,
    /// Effective request limits and defaults for this server instance.
    pub limits: SearchVescKnowledgeLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeLimits {
    pub default_limit: usize,
    pub max_limit: usize,
    pub max_query_bytes: usize,
    pub max_response_bytes: usize,
    pub max_context_bytes: usize,
    pub default_detail: SearchResponseDetail,
}

/// Effective search capabilities for clients that need limits before issuing a search.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeCapabilities {
    pub ok: bool,
    pub modes: Vec<SearchMode>,
    pub details: Vec<SearchResponseDetail>,
    pub limits: SearchVescKnowledgeLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeTiming {
    pub total_us: u64,
    pub result_count: usize,
}

/// Input for replaying a serious correction against base knowledge only.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct ReplayVescKnowledgeCorrectionParams {
    /// Stable correction ID returned by `correct_vesc_knowledge`.
    pub correction_id: String,
    /// Persist covered state only after a successful base-only replay.
    #[serde(default)]
    pub mark_covered: bool,
    /// Required when `mark_covered` is true.
    #[serde(default)]
    pub authorization: Option<crate::tools::knowledge_feedback::CorrectionAuthorization>,
}

/// Result of replaying the preserved failed query without learned advisories.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct CorrectionReplayReport {
    pub ok: bool,
    pub correction_id: String,
    pub query: String,
    pub covered_by_base_knowledge: bool,
    pub marked_covered: bool,
    pub matched_decisive_evidence: Vec<String>,
    pub missing_decisive_evidence: Vec<String>,
    pub ordered_result_ids: Vec<String>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CorrectionReplayReport {
    #[must_use]
    pub(crate) fn failure(correction_id: &str, query: String, error: String) -> Self {
        Self {
            ok: false,
            correction_id: correction_id.into(),
            query,
            covered_by_base_knowledge: false,
            marked_covered: false,
            matched_decisive_evidence: Vec::new(),
            missing_decisive_evidence: Vec::new(),
            ordered_result_ids: Vec::new(),
            warnings: Vec::new(),
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
pub struct SearchVescKnowledgeResponse {
    pub ok: bool,
    /// Mode requested by the caller (or the configured default when omitted).
    #[serde(default)]
    pub mode_requested: SearchMode,
    /// Mode that actually produced the results. This differs from `mode` when
    /// auto mode degrades to lexical retrieval.
    #[serde(default)]
    pub mode_used: SearchMode,
    pub mode: SearchMode,
    /// Detail profile actually represented on the wire after response bounds.
    #[serde(default)]
    pub detail: SearchResponseDetail,
    /// Retrieval capabilities available for the selected mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Current resource-backed corrections relevant to this query.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corrections: Vec<KnowledgeCorrectionResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<SearchVescKnowledgeResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Stable machine-readable warning identifiers parallel to `warnings`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warning_codes: Vec<String>,
    /// Typed request validation diagnostics for rejected fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<SearchVescKnowledgeValidation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<SearchVescKnowledgeIndex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<SearchVescKnowledgeTiming>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeValidation {
    pub field: String,
    pub rejected_value: String,
    pub accepted: String,
    pub clamping_safe: bool,
}

type CompactResultRow = (
    String,
    String,
    String,
    usize,
    Option<String>,
    Vec<String>,
    Option<String>,
);

/// Compact search wire shape. The field table keeps rows cheap without making
/// the positional payload ambiguous to clients.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct CompactSearchResponse {
    ok: bool,
    mode_requested: SearchMode,
    mode_used: SearchMode,
    mode: SearchMode,
    detail: SearchResponseDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    repositories: BTreeMap<String, String>,
    fields: [&'static str; 7],
    sources: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    corrections: Vec<KnowledgeCorrectionResult>,
    results: Vec<CompactResultRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warning_codes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation: Option<SearchVescKnowledgeValidation>,
}

#[must_use]
fn compact_response(response: &SearchVescKnowledgeResponse, query: &str) -> CompactSearchResponse {
    let mut sources = Vec::new();
    let mut results = Vec::with_capacity(response.results.len());
    for result in &response.results {
        let source = format!(
            "{}:{}:{}",
            result.source.repo, result.source.path, result.source.line
        );
        let source_index = sources
            .iter()
            .position(|known| known == &source)
            .unwrap_or_else(|| {
                sources.push(source);
                sources.len() - 1
            });
        let excerpt = compact_excerpt(result, query);
        results.push((
            result.name.clone(),
            result.category.clone(),
            excerpt,
            source_index,
            result.chunk_id.clone(),
            result.correction_ids.clone(),
            result.origin.clone(),
        ));
    }
    CompactSearchResponse {
        ok: response.ok,
        mode_requested: response.mode_requested,
        mode_used: response.mode_used,
        mode: response.mode,
        detail: response.detail,
        snapshot_id: response
            .index
            .as_ref()
            .and_then(|index| index.snapshot_id.clone()),
        repositories: response
            .index
            .as_ref()
            .map(|index| index.repositories.clone())
            .unwrap_or_default(),
        fields: COMPACT_FIELDS,
        sources,
        corrections: response.corrections.clone(),
        results,
        error: response.error.clone(),
        warnings: response.warnings.clone(),
        warning_codes: response.warning_codes.clone(),
        validation: response.validation.clone(),
    }
}

fn compact_excerpt(result: &SearchVescKnowledgeResult, query: &str) -> String {
    let exact_identifier = result
        .explanation
        .as_ref()
        .is_some_and(|explanation| explanation.exact_identifier);
    if !exact_identifier {
        let mut excerpt = result.summary.clone();
        truncate_utf8(&mut excerpt, COMPACT_EXCERPT_BYTES);
        return excerpt;
    }

    let Some((anchor, _)) = symbol_anchor(&result.summary, query) else {
        let mut excerpt = result.summary.clone();
        truncate_utf8(&mut excerpt, COMPACT_EXCERPT_BYTES);
        return excerpt;
    };
    let mut start = anchor.saturating_sub(COMPACT_EXCERPT_BYTES / 2);
    while start > 0 && !result.summary.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start
        .saturating_add(COMPACT_EXCERPT_BYTES)
        .min(result.summary.len());
    while end < result.summary.len() && !result.summary.is_char_boundary(end) {
        end += 1;
    }
    let mut excerpt = String::with_capacity(end - start + 2);
    if start > 0 {
        excerpt.push('…');
    }
    excerpt.push_str(&result.summary[start..end]);
    if end < result.summary.len() {
        excerpt.push('…');
    }
    truncate_utf8(&mut excerpt, COMPACT_EXCERPT_BYTES);
    excerpt
}

fn symbol_anchor(text: &str, query: &str) -> Option<(usize, usize)> {
    query
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| {
            token.contains('_')
                || token
                    .chars()
                    .skip(1)
                    .any(|character| character.is_ascii_uppercase())
        })
        .find_map(|token| identifier_position(text, token))
        .or_else(|| first_snake_case_identifier(text))
}

fn identifier_position(text: &str, identifier: &str) -> Option<(usize, usize)> {
    text.match_indices(identifier).find_map(|(start, matched)| {
        let end = start + matched.len();
        let boundary = |character: Option<char>| {
            character
                .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'))
        };
        (boundary(text[..start].chars().next_back()) && boundary(text[end..].chars().next()))
            .then_some((start, end))
    })
}

fn first_snake_case_identifier(text: &str) -> Option<(usize, usize)> {
    let mut token_start = None;
    for (index, character) in text
        .char_indices()
        .chain(std::iter::once((text.len(), '\0')))
    {
        let is_identifier = character.is_ascii_alphanumeric() || character == '_';
        match (token_start, is_identifier) {
            (None, true) => token_start = Some(index),
            (Some(start), false) => {
                let token = &text[start..index];
                if token.contains('_')
                    && token
                        .chars()
                        .any(|character| character.is_ascii_alphabetic())
                {
                    return Some((start, index));
                }
                token_start = None;
            }
            _ => {}
        }
    }
    None
}

fn parse_category(raw: Option<&str>) -> Option<Category> {
    raw.and_then(|name| serde_json::from_value(serde_json::Value::String(name.to_string())).ok())
}

#[must_use]
pub fn search_vesc_knowledge_tool(
    params: &SearchVescKnowledgeParams,
) -> SearchVescKnowledgeResponse {
    search_vesc_knowledge_tool_with_config(params, &KnowledgeConfig::default())
}

/// Execute a search using the resolved server knowledge configuration.
#[must_use]
pub fn search_vesc_knowledge_tool_with_config(
    params: &SearchVescKnowledgeParams,
    config: &KnowledgeConfig,
) -> SearchVescKnowledgeResponse {
    search_vesc_knowledge_tool_with_executor(params, config, search_mode)
}

fn search_vesc_knowledge_tool_with_executor<F>(
    params: &SearchVescKnowledgeParams,
    configured: &KnowledgeConfig,
    executor: F,
) -> SearchVescKnowledgeResponse
where
    F: FnOnce(
        &SearchVescKnowledgeParams,
        SearchMode,
        &vesc_knowledge_index::LexicalFilters,
        usize,
        &KnowledgeConfig,
    ) -> Result<(Vec<SearchVescKnowledgeResult>, Vec<String>, SearchMode), String>,
{
    let mode = params.mode.unwrap_or_else(|| configured_mode(configured));
    let (selected, config, limit) = match validate_search_inputs(params, configured, mode) {
        Ok(inputs) => inputs,
        Err(response) => return *response,
    };
    let started = Instant::now();

    match parse_filters(params) {
        Ok(filters) => match executor(params, mode, &filters, limit, &config) {
            Ok((mut results, warnings, mode_used)) => {
                if let Some(snapshot_id) = selected
                    .as_ref()
                    .and_then(|artifact| artifact.snapshot_id.as_ref())
                {
                    qualify_snapshot_resources(&mut results, snapshot_id.as_str());
                }
                let mut response = SearchVescKnowledgeResponse {
                    ok: true,
                    mode_requested: mode,
                    mode_used,
                    mode,
                    detail: params.detail,
                    capabilities: capabilities_for_mode(mode_used),
                    corrections: Vec::new(),
                    results,
                    error: None,
                    warnings,
                    warning_codes: Vec::new(),
                    validation: None,
                    index: index_metadata(&config, selected.as_ref()),
                    timing: None,
                };
                response.timing = Some(SearchVescKnowledgeTiming {
                    total_us: elapsed_us(started),
                    result_count: response.results.len(),
                });
                response = response.bounded(params, &config, params.detail);
                if let Some(timing) = &mut response.timing {
                    timing.result_count = response.results.len();
                }
                response
            }
            Err(error) => SearchVescKnowledgeResponse {
                ok: false,
                mode_requested: mode,
                mode_used: mode,
                mode,
                detail: params.detail,
                capabilities: Vec::new(),
                corrections: Vec::new(),
                results: Vec::new(),
                error: Some(error),
                warnings: Vec::new(),
                warning_codes: vec!["retrieval_failed".into()],
                validation: None,
                index: index_metadata(&config, selected.as_ref()),
                timing: None,
            },
        },
        Err(error) => validation_error_response(
            mode,
            "filters",
            "provided object".into(),
            "object with supported enum values and non-empty identifiers".into(),
            false,
            error,
        ),
    }
}

#[cfg(test)]
fn search_vesc_knowledge_tool_with_provider<P: EmbeddingProvider + ?Sized>(
    params: &SearchVescKnowledgeParams,
    config: &KnowledgeConfig,
    provider: &mut P,
) -> SearchVescKnowledgeResponse {
    search_vesc_knowledge_tool_with_executor(
        params,
        config,
        |params, mode, filters, limit, config| {
            search_mode_with_semantic(
                params,
                mode,
                filters,
                limit,
                config,
                |query, limit, config| {
                    let vector = load_vector_artifact(config)?;
                    let hits = semantic_hits_with_provider(query, limit, &vector, provider)?;
                    Ok((hits, false))
                },
            )
        },
    )
}

fn selected_search_config(
    params: &SearchVescKnowledgeParams,
    config: &KnowledgeConfig,
) -> Result<
    (
        Option<crate::config::ResolvedKnowledgeArtifact>,
        Option<KnowledgeConfig>,
    ),
    String,
> {
    let Some(snapshot_id) = params.snapshot_id.as_deref() else {
        return Ok((None, None));
    };
    let resolved = config.resolved_snapshot(snapshot_id).ok_or_else(|| {
        "snapshot is unknown or not ready; call list_vesc_source_versions then prepare_vesc_knowledge"
            .to_owned()
    })?;
    let mut selected = config.clone();
    selected.managed_git = false;
    selected.artifact_path = Some(resolved.path.clone());
    Ok((Some(resolved), Some(selected)))
}

fn validate_search_inputs(
    params: &SearchVescKnowledgeParams,
    configured: &KnowledgeConfig,
    mode: SearchMode,
) -> Result<
    (
        Option<crate::config::ResolvedKnowledgeArtifact>,
        KnowledgeConfig,
        usize,
    ),
    Box<SearchVescKnowledgeResponse>,
> {
    if configured.mode == RetrievalMode::Lexical && mode != SearchMode::Lexical {
        return Err(Box::new(validation_error_response(
            mode,
            "mode",
            format!("{mode:?}"),
            "lexical".into(),
            false,
            "service is configured for lexical search; retry with mode \"lexical\"".into(),
        )));
    }
    let (selected, selected_config) =
        selected_search_config(params, configured).map_err(|error| {
            Box::new(validation_error_response(
                mode,
                "snapshot_id",
                params.snapshot_id.clone().unwrap_or_default(),
                "an available snapshot ID from list_vesc_source_versions".into(),
                false,
                error,
            ))
        })?;
    let config = selected_config.unwrap_or_else(|| configured.clone());
    if let Err(error) = resolved_search_artifact(&config) {
        return Err(Box::new(error_response(mode, error)));
    }
    if params.query.len() > config.max_query_bytes {
        return Err(Box::new(validation_error_response(
            mode,
            "query",
            params.query.clone(),
            format!("UTF-8 string up to {} bytes", config.max_query_bytes),
            false,
            format!("query exceeds {} bytes", config.max_query_bytes),
        )));
    }
    let limit = if params.limit == 0 {
        default_search_limit()
    } else {
        params.limit
    };
    if limit > config.max_limit {
        return Err(Box::new(validation_error_response(
            mode,
            "limit",
            limit.to_string(),
            format!("integer in 1..={}", config.max_limit),
            true,
            format!("limit exceeds maximum {}", config.max_limit),
        )));
    }
    if params
        .max_response_bytes
        .is_some_and(|budget| budget == 0 || budget > config.max_response_bytes)
    {
        return Err(Box::new(validation_error_response(
            mode,
            "max_response_bytes",
            params.max_response_bytes.unwrap_or_default().to_string(),
            format!("integer in 1..={}", config.max_response_bytes),
            true,
            format!(
                "max_response_bytes must be between 1 and {}",
                config.max_response_bytes
            ),
        )));
    }
    if params
        .max_context_bytes
        .is_some_and(|budget| budget == 0 || budget > config.max_passage_bytes)
    {
        return Err(Box::new(validation_error_response(
            mode,
            "max_context_bytes",
            params.max_context_bytes.unwrap_or_default().to_string(),
            format!("integer in 1..={}", config.max_passage_bytes),
            true,
            format!(
                "max_context_bytes must be between 1 and {}",
                config.max_passage_bytes
            ),
        )));
    }
    Ok((selected, config, limit))
}

fn resolved_search_artifact(config: &KnowledgeConfig) -> Result<Option<PathBuf>, String> {
    let artifact = config.resolved_artifact_path();
    if artifact.is_none() && config.manages_repositories() {
        return Err(
            "managed knowledge is unavailable while repository preparation is incomplete or failed"
                .into(),
        );
    }
    Ok(artifact)
}

fn qualify_snapshot_resources(results: &mut [SearchVescKnowledgeResult], snapshot_id: &str) {
    for result in results {
        if let Some(chunk_id) = &result.chunk_id {
            let uri = format!("vesc://knowledge/snapshot/{snapshot_id}/chunk/{chunk_id}");
            result.resource_uri = Some(uri.clone());
            if let Some(provenance) = &mut result.provenance {
                provenance.resource_uri = Some(uri);
            }
        }
        if let Some(document_id) = &result.document_id {
            result.document_uri = Some(format!(
                "vesc://knowledge/snapshot/{snapshot_id}/document/{document_id}"
            ));
        }
    }
}

const fn configured_mode(config: &KnowledgeConfig) -> SearchMode {
    match config.mode {
        RetrievalMode::Lexical => SearchMode::Lexical,
        RetrievalMode::Auto => SearchMode::Auto,
        RetrievalMode::Hybrid => SearchMode::Hybrid,
    }
}

fn capabilities_for_mode(mode: SearchMode) -> Vec<String> {
    match mode {
        SearchMode::Lexical => vec![
            "lexical-index".into(),
            "provenance".into(),
            "knowledge-chunk-resource".into(),
            "knowledge-document-resource".into(),
        ],
        SearchMode::Auto | SearchMode::Hybrid => vec![
            "lexical-index".into(),
            "semantic-index".into(),
            "hybrid-fusion".into(),
            "provenance".into(),
            "knowledge-chunk-resource".into(),
            "knowledge-document-resource".into(),
        ],
    }
}

fn error_response(mode: SearchMode, error: String) -> SearchVescKnowledgeResponse {
    SearchVescKnowledgeResponse {
        ok: false,
        mode_requested: mode,
        mode_used: mode,
        mode,
        detail: SearchResponseDetail::Full,
        capabilities: Vec::new(),
        corrections: Vec::new(),
        results: Vec::new(),
        error: Some(error),
        warnings: Vec::new(),
        warning_codes: vec!["request_failed".into()],
        validation: None,
        index: None,
        timing: None,
    }
}

fn validation_error_response(
    mode: SearchMode,
    field: &str,
    rejected_value: String,
    accepted: String,
    clamping_safe: bool,
    error: String,
) -> SearchVescKnowledgeResponse {
    let mut response = error_response(mode, error);
    response.warning_codes = vec!["validation_failed".into()];
    response.validation = Some(SearchVescKnowledgeValidation {
        field: field.into(),
        rejected_value,
        accepted,
        clamping_safe,
    });
    response
}

impl SearchVescKnowledgeResponse {
    fn bounded(
        mut self,
        params: &SearchVescKnowledgeParams,
        config: &KnowledgeConfig,
        detail: SearchResponseDetail,
    ) -> Self {
        self.warning_codes = warning_codes(&self.warnings);
        let limit = if params.limit == 0 {
            default_search_limit()
        } else {
            params.limit
        };
        self.results.truncate(limit);
        let passage_limit = params
            .max_context_bytes
            .unwrap_or(config.max_passage_bytes)
            .min(config.max_passage_bytes);
        for result in &mut self.results {
            if let Some(provenance) = &mut result.provenance {
                truncate_utf8(&mut provenance.passage, passage_limit);
                result.summary = provenance.passage.clone();
                result.passage = Some(result.summary.clone());
            }
        }
        let budget = params
            .max_response_bytes
            .unwrap_or(config.max_response_bytes)
            .min(config.max_response_bytes);
        if detail == SearchResponseDetail::Compact
            || response_exceeds_budget(&self, budget, detail, &params.query)
        {
            for correction in &mut self.corrections {
                compact_correction(correction);
            }
        }
        let initial_results = self.results.len();
        while response_exceeds_budget(&self, budget, detail, &params.query)
            && self.results.len() > 1
        {
            self.results.pop();
        }
        if self.results.len() < initial_results {
            self.warning_codes.push("results_dropped".into());
            self.warnings
                .push("response budget removed lower-ranked result rows".into());
        }
        if response_exceeds_budget(&self, budget, detail, &params.query) {
            for result in &mut self.results {
                compact_result(result);
            }
            self.detail = SearchResponseDetail::Compact;
            self.warning_codes.push("detail_degraded".into());
            self.warning_codes.push("provenance_removed".into());
            self.warnings.push(
                "full detail exceeded the response budget; returned compact rows without provenance"
                    .into(),
            );
        }
        let compacted_results = self.results.len();
        while response_exceeds_budget(&self, budget, detail, &params.query)
            && self.results.len() > 1
        {
            self.results.pop();
        }
        if self.results.len() < compacted_results {
            self.warning_codes.push("results_dropped".into());
        }
        if response_exceeds_budget(&self, budget, detail, &params.query) {
            self.results.clear();
            self.index = None;
            if detail == SearchResponseDetail::Full {
                self.detail = SearchResponseDetail::Compact;
                self.warning_codes.push("detail_degraded".into());
                self.warnings.push(
                    "full detail could not fit the response budget; evidence fields were removed"
                        .into(),
                );
            }
        }
        while response_exceeds_budget(&self, budget, detail, &params.query)
            && self.corrections.len() > 1
        {
            self.corrections.pop();
        }
        if response_exceeds_budget(&self, budget, detail, &params.query) {
            self.corrections.clear();
        }
        if response_exceeds_budget(&self, budget, detail, &params.query) {
            self.warnings
                .push("response budget is smaller than the fixed response envelope".into());
            self.warning_codes.push("response_budget_exceeded".into());
        }
        if let Some(timing) = &mut self.timing {
            timing.result_count = self.results.len();
        }
        self
    }
}

fn warning_codes(warnings: &[String]) -> Vec<String> {
    warnings
        .iter()
        .filter_map(|warning| warning.split(':').next())
        .map(|code| code.trim().to_owned())
        .filter(|code| !code.is_empty() && *code != "full detail exceeded the response budget")
        .collect()
}

fn response_exceeds_budget(
    response: &SearchVescKnowledgeResponse,
    budget: usize,
    detail: SearchResponseDetail,
    query: &str,
) -> bool {
    match detail {
        SearchResponseDetail::Compact => serde_json::to_vec(&compact_response(response, query))
            .map_or(true, |bytes| bytes.len() > budget),
        SearchResponseDetail::Full => {
            serde_json::to_vec(response).map_or(true, |bytes| bytes.len() > budget)
        }
    }
}

fn compact_result(result: &mut SearchVescKnowledgeResult) {
    result.passage = None;
    result.heading_path = None;
    result.resource_uri = None;
    result.document_uri = None;
    result.provenance = None;
    truncate_utf8(&mut result.name, 128);
    truncate_utf8(&mut result.summary, 256);
}

fn compact_correction(correction: &mut KnowledgeCorrectionResult) {
    truncate_utf8(&mut correction.question, 128);
    truncate_utf8(&mut correction.what_we_know, 512);
    truncate_utf8(&mut correction.common_mistake, 256);
    truncate_utf8(&mut correction.reasoning_failure, 384);
    truncate_utf8(&mut correction.mistaken_conclusion, 256);
    truncate_utf8(&mut correction.correction, 512);
    for qualifier in &mut correction.qualifiers {
        truncate_utf8(qualifier, 128);
    }
    correction.qualifiers.truncate(4);
    for next in &mut correction.check_next {
        truncate_utf8(next, 256);
    }
    correction.check_next.truncate(6);
    correction.gap_diagnoses.truncate(4);
    correction.recommended_data_actions.truncate(4);
    correction.affected_resources.truncate(8);
    for evidence in &mut correction.evidence {
        evidence.excerpt.clear();
    }
    correction.evidence.truncate(4);
}

fn truncate_utf8(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn index_metadata(
    config: &KnowledgeConfig,
    selected: Option<&crate::config::ResolvedKnowledgeArtifact>,
) -> Option<SearchVescKnowledgeIndex> {
    if let Some(resolved) = selected.cloned().or_else(|| config.resolved_artifact()) {
        let root = resolved.path;
        if root.is_file() {
            return None;
        }
        if let Ok(manifest) = active_artifact_summary(&root) {
            return Some(SearchVescKnowledgeIndex {
                snapshot_id: resolved.snapshot_id.map(|id| id.as_str().to_owned()),
                snapshot_profile: resolved.snapshot_profile,
                repositories: resolved
                    .repositories
                    .into_iter()
                    .map(|(id, commit)| (id.as_str().to_owned(), commit))
                    .collect(),
                corpus_version: manifest.corpus_version.to_string(),
                corpus_digest: Some(manifest.corpus_digest.to_string()),
                document_count: manifest.document_count,
                chunk_count: manifest.chunk_count,
                source_count: manifest.source_count,
                diagnostic_count: manifest.diagnostic_count,
                component_versions: manifest.component_versions.clone(),
                lexical_checksum: manifest.lexical_checksum.as_ref().map(ToString::to_string),
                limits: search_limits(config),
            });
        }
    }
    let count = vesc_knowledge_index::embedded_entries().len();
    Some(SearchVescKnowledgeIndex {
        snapshot_id: None,
        snapshot_profile: None,
        repositories: BTreeMap::new(),
        corpus_version: "embedded-catalog-v1".into(),
        corpus_digest: None,
        document_count: count,
        chunk_count: count,
        source_count: 0,
        diagnostic_count: 0,
        component_versions: BTreeMap::new(),
        lexical_checksum: None,
        limits: search_limits(config),
    })
}

const fn search_limits(config: &KnowledgeConfig) -> SearchVescKnowledgeLimits {
    SearchVescKnowledgeLimits {
        default_limit: default_search_limit(),
        max_limit: config.max_limit,
        max_query_bytes: config.max_query_bytes,
        max_response_bytes: config.max_response_bytes,
        max_context_bytes: config.max_passage_bytes,
        default_detail: SearchResponseDetail::Full,
    }
}

/// Serialize the effective search contract without requiring a trial search.
///
/// # Panics
///
/// Panics only if the infallible capabilities response cannot be serialized.
#[must_use]
pub fn search_vesc_knowledge_capabilities_json(config: &KnowledgeConfig) -> String {
    serde_json::to_string(&search_vesc_knowledge_capabilities(config))
        .expect("search capabilities contain only infallibly serializable fields")
}

/// Return the effective search contract without requiring a trial search.
#[must_use]
pub fn search_vesc_knowledge_capabilities(
    config: &KnowledgeConfig,
) -> SearchVescKnowledgeCapabilities {
    SearchVescKnowledgeCapabilities {
        ok: true,
        modes: vec![SearchMode::Lexical, SearchMode::Auto, SearchMode::Hybrid],
        details: vec![SearchResponseDetail::Full, SearchResponseDetail::Compact],
        limits: search_limits(config),
    }
}

fn parse_filters(
    params: &SearchVescKnowledgeParams,
) -> Result<vesc_knowledge_index::LexicalFilters, String> {
    let category = parse_category(params.filters.category.as_deref());
    let repository = params
        .filters
        .repository
        .as_deref()
        .map(vesc_knowledge_index::RepositoryId::try_from)
        .transpose()
        .map_err(|_| "repository filter must be non-empty".to_string())?;
    let revision = params
        .filters
        .revision
        .as_deref()
        .map(vesc_knowledge_index::Revision::try_from)
        .transpose()
        .map_err(|_| "revision filter must be non-empty".to_string())?;
    let trust_tier = params
        .filters
        .trust_tier
        .as_deref()
        .map(|value| {
            serde_json::from_value(serde_json::Value::String(value.into()))
                .map_err(|_| format!("unsupported trust_tier {value:?}"))
        })
        .transpose()?;
    let source_kind = params
        .filters
        .source_kind
        .as_deref()
        .map(|value| {
            serde_json::from_value(serde_json::Value::String(value.into()))
                .map_err(|_| format!("unsupported source_kind {value:?}"))
        })
        .transpose()?;
    let tags = params
        .filters
        .tags
        .iter()
        .map(|tag| {
            if tag.trim().is_empty() {
                Err("tag filters must be non-empty".to_string())
            } else {
                Ok(tag.to_ascii_lowercase())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(vesc_knowledge_index::LexicalFilters {
        category,
        repository,
        paths: params
            .filters
            .paths
            .iter()
            .filter(|path| !path.trim().is_empty())
            .cloned()
            .collect(),
        revision,
        source_kind,
        trust_tier,
        tags,
    })
}

fn search_mode(
    params: &SearchVescKnowledgeParams,
    mode: SearchMode,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<(Vec<SearchVescKnowledgeResult>, Vec<String>, SearchMode), String> {
    search_mode_with_semantic(params, mode, filters, limit, config, semantic_hits)
}

fn search_mode_with_semantic<S>(
    params: &SearchVescKnowledgeParams,
    mode: SearchMode,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
    mut semantic: S,
) -> Result<(Vec<SearchVescKnowledgeResult>, Vec<String>, SearchMode), String>
where
    S: FnMut(&str, usize, &KnowledgeConfig) -> Result<(Vec<SemanticHit>, bool), String>,
{
    match mode {
        SearchMode::Lexical => lexical_search_results(params, filters, limit, config)
            .map(|(results, warnings)| (results, warnings, SearchMode::Lexical)),
        SearchMode::Auto => {
            match hybrid_results_with_semantic(params, filters, limit, config, &mut semantic) {
                Ok((results, warnings)) => Ok((results, warnings, SearchMode::Hybrid)),
                Err(error) => {
                    let (results, _) = lexical_search_results(params, filters, limit, config)?;
                    Ok((
                        results,
                        vec![format!(
                            "semantic_unavailable: {error}; used lexical retrieval"
                        )],
                        SearchMode::Lexical,
                    ))
                }
            }
        }
        SearchMode::Hybrid => {
            hybrid_results_with_semantic(params, filters, limit, config, &mut semantic)
                .map(|(results, warnings)| (results, warnings, SearchMode::Hybrid))
                .map_err(|error| {
                    format!("semantic retrieval failed: {error}; retry with mode \"lexical\"")
                })
        }
    }
}

fn lexical_search_results(
    params: &SearchVescKnowledgeParams,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<(Vec<SearchVescKnowledgeResult>, Vec<String>), String> {
    let candidate_limit = limit.saturating_mul(5).clamp(20, 100);
    let hits = lexical_results(&params.query, filters, candidate_limit, config)?;
    let results = if params.detail == SearchResponseDetail::Full {
        let mut chunks = hits
            .iter()
            .map(|hit| (hit.chunk.chunk_id.clone(), hit.chunk.clone()))
            .collect::<ChunkMap>();
        let fused = hits
            .iter()
            .enumerate()
            .map(|(rank, hit)| FusedHit {
                chunk: hit.chunk.clone(),
                score: f64::from(hit.score),
                lexical_rank: Some(rank + 1),
                semantic_rank: None,
                lexical_score: Some(hit.score),
                semantic_similarity: None,
                exact_identifier: hit.exact_identifier,
            })
            .collect::<Vec<_>>();
        hydrate_adjacent_chunks(&fused, config, &mut chunks)?;
        lexical_results_with_context(fused, filters, &chunks, context_budget(params, config))
    } else {
        hits.into_iter()
            .enumerate()
            .map(|(rank, hit)| lexical_result(hit, rank, filters))
            .collect()
    };
    Ok((
        retain_diverse_results(results, filters, limit, &preferred_revisions(config)),
        Vec::new(),
    ))
}

fn context_budget(params: &SearchVescKnowledgeParams, config: &KnowledgeConfig) -> usize {
    params
        .max_context_bytes
        .unwrap_or(config.max_passage_bytes)
        .min(config.max_passage_bytes)
}

fn lexical_results_with_context(
    hits: Vec<FusedHit>,
    filters: &vesc_knowledge_index::LexicalFilters,
    chunks: &ChunkMap,
    max_context_bytes: usize,
) -> Vec<SearchVescKnowledgeResult> {
    hits.into_iter()
        .map(|hit| {
            let context = expand_adjacent_context(
                &hit.chunk,
                chunks,
                MAX_CONTEXT_NEIGHBORS,
                max_context_bytes,
            );
            fused_result(hit, &context, filters)
        })
        .collect()
}

fn lexical_results(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<Vec<LexicalHit>, String> {
    if let Some(path) = resolved_search_artifact(config)? {
        let lexical_path = active_lexical_path(&path)?;
        let repositories_root = config.managed_repositories_root();
        return with_cached_lexical_index(&lexical_path, repositories_root.as_deref(), |index| {
            index
                .search(query, filters, limit)
                .map_err(|error| error.to_string())
        });
    }
    vesc_knowledge_index::lexical_index()
        .search(query, filters, limit)
        .map_err(|error| error.to_string())
}

fn hybrid_results_with_semantic<S>(
    params: &SearchVescKnowledgeParams,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
    semantic: &mut S,
) -> Result<(Vec<SearchVescKnowledgeResult>, Vec<String>), String>
where
    S: FnMut(&str, usize, &KnowledgeConfig) -> Result<(Vec<SemanticHit>, bool), String>,
{
    let candidate_limit = limit.saturating_mul(5).clamp(20, 100);
    let (lexical, mut metadata) =
        lexical_candidates_and_metadata(&params.query, filters, candidate_limit, config)?;
    let (mut semantic_hits, live_rerank) = semantic(&params.query, candidate_limit, config)?;
    load_semantic_metadata(&mut semantic_hits, filters, config, &mut metadata)?;
    let context_budget = context_budget(params, config);
    let fused = fuse_candidate_metadata(
        &lexical,
        &semantic_hits,
        &metadata,
        FusionConfig {
            limit: candidate_limit,
            ..FusionConfig::default()
        },
    );
    let (fused, mut chunks) = hydrate_fused_candidates(fused, config)?;
    hydrate_adjacent_chunks(&fused, config, &mut chunks)?;
    let results = retain_diverse_results(
        lexical_results_with_context(fused, filters, &chunks, context_budget),
        filters,
        limit,
        &preferred_revisions(config),
    );
    let warnings = live_rerank
        .then(|| {
            "snapshot vector artifact unavailable; used the local model to semantically rerank lexical candidates"
                .into()
        })
        .into_iter()
        .collect();
    Ok((results, warnings))
}

#[cfg(test)]
fn hybrid_results_with_provider<P: EmbeddingProvider + ?Sized>(
    params: &SearchVescKnowledgeParams,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
    provider: &mut P,
) -> Result<(Vec<SearchVescKnowledgeResult>, bool), String> {
    hybrid_results_with_semantic(
        params,
        filters,
        limit,
        config,
        &mut |query, limit, config| {
            let vector = load_vector_artifact(config)?;
            let hits = semantic_hits_with_provider(query, limit, &vector, provider)?;
            Ok((hits, false))
        },
    )
    .map(|(results, _warnings)| (results, false))
}

fn lexical_candidates_and_metadata(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<(Vec<LexicalCandidate>, CandidateMetadataMap), String> {
    if let Some(path) = resolved_search_artifact(config)? {
        let lexical_path = active_lexical_path(&path)?;
        let repositories_root = config.managed_repositories_root();
        return with_cached_lexical_index(&lexical_path, repositories_root.as_deref(), |index| {
            let hits = index
                .search_candidates(query, filters, limit)
                .map_err(|error| error.to_string())?;
            let metadata = hits
                .iter()
                .map(|hit| (hit.chunk.chunk_id.clone(), hit.chunk.clone()))
                .collect();
            Ok((hits, metadata))
        });
    }
    let index = vesc_knowledge_index::lexical_index();
    let hits = index
        .search_candidates(query, filters, limit)
        .map_err(|error| error.to_string())?;
    let metadata = hits
        .iter()
        .map(|hit| (hit.chunk.chunk_id.clone(), hit.chunk.clone()))
        .collect();
    Ok((hits, metadata))
}

type ChunkMap = BTreeMap<vesc_knowledge_index::ChunkId, vesc_knowledge_index::Chunk>;
type CandidateMetadataMap = BTreeMap<vesc_knowledge_index::ChunkId, RetrievalMetadata>;
type ArtifactCache<T> = OnceLock<Mutex<Option<(PathBuf, Arc<T>)>>>;
static LEXICAL_ARTIFACT_CACHE: ArtifactCache<LexicalIndex> = OnceLock::new();
static ARTIFACT_METADATA_CACHE: ArtifactCache<vesc_knowledge_index::PreviousArtifactSummary> =
    OnceLock::new();

#[cfg(any(feature = "semantic-fastembed", test))]
static VECTOR_ARTIFACT_CACHE: ArtifactCache<FileBackedVectorArtifact> = OnceLock::new();
#[cfg(any(feature = "semantic-fastembed", test))]
static VECTOR_VALIDATION_CACHE: OnceLock<
    Mutex<Option<(PathBuf, crate::preparation_status::ValidatedVectorArtifact)>>,
> = OnceLock::new();

fn cached_artifact<T>(
    cache: &'static ArtifactCache<T>,
    path: &Path,
    load: impl FnOnce() -> Result<T, String>,
) -> Result<Arc<T>, String> {
    let cache = cache.get_or_init(|| Mutex::new(None));
    let mut cache = cache
        .lock()
        .map_err(|_| "artifact cache is poisoned".to_string())?;
    if let Some(value) = cache
        .as_ref()
        .filter(|(key, _)| key == path)
        .map(|(_, value)| Arc::clone(value))
    {
        return Ok(value);
    }
    cache.take();
    let value = Arc::new(load()?);
    *cache = Some((path.to_owned(), Arc::clone(&value)));
    drop(cache);
    Ok(value)
}

#[cfg(any(feature = "semantic-fastembed", test))]
fn evict_cached_artifact<T>(cache: &'static ArtifactCache<T>) {
    if let Some(cache) = cache.get() {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

fn active_artifact_summary(
    root: &Path,
) -> Result<Arc<vesc_knowledge_index::PreviousArtifactSummary>, String> {
    let generation = vesc_knowledge_index::active_generation_path(root)
        .map_err(|_| "configured knowledge artifact unavailable".to_string())?;
    cached_artifact(&ARTIFACT_METADATA_CACHE, &generation, || {
        vesc_knowledge_index::inspect_previous_artifact(
            &vesc_knowledge_index::active_manifest_path(root),
        )
        .map_err(|_| "configured knowledge artifact unavailable".to_string())
    })
}

/// Reuse the active generation's Tantivy index between MCP requests.
fn with_cached_lexical_index<T>(
    path: &Path,
    repositories_root: Option<&Path>,
    operation: impl FnOnce(&LexicalIndex) -> Result<T, String>,
) -> Result<T, String> {
    let index = cached_artifact(&LEXICAL_ARTIFACT_CACHE, path, || {
        repositories_root
            .map_or_else(
                || LexicalIndex::open_search_artifact(path),
                |root| LexicalIndex::open_git_search_artifact(path, root),
            )
            .map_err(|_| "configured lexical artifact unavailable".to_string())
    })?;
    operation(&index)
}

#[allow(clippy::significant_drop_tightening)]
fn semantic_hits(
    query: &str,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<(Vec<SemanticHit>, bool), String> {
    #[cfg(feature = "semantic-fastembed")]
    {
        let vector = load_vector_artifact(config)?;
        let mut state = initialize_semantic_model(config)?;
        let entry = state
            .as_mut()
            .ok_or_else(|| "semantic provider cache is empty".to_string())?;
        let result = semantic_hits_with_provider(query, limit, &vector, &mut entry.provider)
            .map(|hits| (hits, false));
        entry.last_used = Instant::now();
        semantic_model_cache().wake.notify_one();
        result
    }

    #[cfg(not(feature = "semantic-fastembed"))]
    {
        let _ = (query, limit, config);
        Err("semantic-fastembed feature is disabled".into())
    }
}

#[cfg(feature = "semantic-fastembed")]
static SEMANTIC_PROVIDER: OnceLock<SemanticModelCache<CachedSemanticProvider>> = OnceLock::new();

#[cfg(feature = "semantic-fastembed")]
static SEMANTIC_REAPER: Once = Once::new();

#[cfg(any(feature = "semantic-fastembed", test))]
struct SemanticModelCache<T> {
    state: Mutex<Option<T>>,
    wake: Condvar,
}

#[cfg(feature = "semantic-fastembed")]
struct CachedSemanticProvider {
    key: String,
    provider: vesc_knowledge_index::FastEmbedProvider,
    last_used: Instant,
    idle_timeout: Duration,
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_model_cache() -> &'static SemanticModelCache<CachedSemanticProvider> {
    let cache = SEMANTIC_PROVIDER.get_or_init(|| SemanticModelCache {
        state: Mutex::new(None),
        wake: Condvar::new(),
    });
    SEMANTIC_REAPER.call_once(|| {
        std::thread::Builder::new()
            .name("vesc-semantic-model-reaper".into())
            .spawn(reap_idle_semantic_model)
            .expect("spawn semantic model reaper");
    });
    cache
}

#[cfg(feature = "semantic-fastembed")]
fn initialize_semantic_model(
    config: &KnowledgeConfig,
) -> Result<MutexGuard<'static, Option<CachedSemanticProvider>>, String> {
    let model_dir = config
        .semantic_model_dir
        .as_deref()
        .ok_or_else(|| "semantic model directory is not configured".to_string())?;
    let model_id = config
        .semantic_model_id
        .as_deref()
        .ok_or_else(|| "semantic model identity is not configured".to_string())?;
    let model_revision = config
        .semantic_model_revision
        .as_deref()
        .ok_or_else(|| "semantic model revision is not configured".to_string())?;
    let key = format!(
        "{}\0{}\0{}\0{:?}",
        model_dir.display(),
        model_id,
        model_revision,
        config.semantic_max_length
    );
    let cache = semantic_model_cache();
    let mut state = cache
        .state
        .lock()
        .map_err(|_| "semantic provider cache is poisoned".to_string())?;
    if state.as_ref().is_none_or(|entry| entry.key != key) {
        let mut profile = vesc_knowledge_index::EmbeddingProfile::for_model_id(model_id)
            .ok_or_else(|| format!("no embedding profile is registered for {model_id}"))?;
        if let Some(max_length) = config.semantic_max_length {
            if max_length == 0 || max_length > profile.max_length {
                return Err(format!(
                    "semantic max length must be between 1 and {} for {model_id}",
                    profile.max_length
                ));
            }
            profile.max_length = max_length;
        }
        let provider =
            vesc_knowledge_index::FastEmbedProvider::from_model_dir_with_profile_and_threads(
                model_dir,
                None,
                profile,
                Some(semantic_query_intra_threads()),
            )
            .map_err(|error| format!("semantic provider unavailable: {error}"))?;
        *state = Some(CachedSemanticProvider {
            key,
            provider,
            last_used: Instant::now(),
            idle_timeout: Duration::from_secs(config.semantic_idle_timeout_secs),
        });
    }
    if let Some(entry) = state.as_mut() {
        entry.last_used = Instant::now();
        entry.idle_timeout = Duration::from_secs(config.semantic_idle_timeout_secs);
    }
    cache.wake.notify_one();
    Ok(state)
}

#[cfg(any(feature = "semantic-fastembed", test))]
const fn semantic_query_intra_threads() -> usize {
    1
}

#[cfg(any(feature = "semantic-fastembed", test))]
fn reap_one_idle_entry<T, U>(
    cache: &SemanticModelCache<T>,
    remaining: impl Fn(&T) -> Duration,
    artifact_cache: &'static ArtifactCache<U>,
) {
    let mut state = cache
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        let Some(entry) = state.as_ref() else {
            state = cache
                .wake
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            continue;
        };
        let remaining = remaining(entry);
        if remaining.is_zero() {
            drop(state.take().expect("semantic cache entry exists"));
            evict_cached_artifact(artifact_cache);
            return;
        }
        let (next, _) = cache
            .wake
            .wait_timeout(state, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
    }
}

#[cfg(feature = "semantic-fastembed")]
fn reap_idle_semantic_model() {
    let cache = SEMANTIC_PROVIDER
        .get()
        .expect("semantic cache initialized before reaper");
    loop {
        reap_one_idle_entry(
            cache,
            |entry| entry.idle_timeout.saturating_sub(entry.last_used.elapsed()),
            &VECTOR_ARTIFACT_CACHE,
        );
    }
}

#[cfg(any(feature = "semantic-fastembed", test))]
fn load_vector_artifact(config: &KnowledgeConfig) -> Result<Arc<FileBackedVectorArtifact>, String> {
    let root = resolved_search_artifact(config)?
        .ok_or_else(|| "vector artifact is not configured".to_string())?;
    let artifact = active_artifact_summary(&root)
        .map_err(|_| "configured vector artifact unavailable".to_string())?;
    let vector_path = root
        .join("generations")
        .join(artifact.generation.to_string())
        .join("vectors.bin");
    let current_proof = crate::preparation_status::ValidatedVectorArtifact::current_identity(&root);
    let process_validated = current_proof.as_ref().is_some_and(|proof| {
        VECTOR_VALIDATION_CACHE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(path, cached)| path == &root && cached == proof)
    });
    let preparation_validated = current_proof.as_ref().is_some_and(|proof| {
        config
            .data_root
            .as_ref()
            .and_then(|data_root| {
                crate::preparation_status::read_preparation_status(data_root.as_path())
            })
            .and_then(|status| status.validated_vector)
            .as_ref()
            == Some(proof)
    });
    let lifecycle_validated = process_validated || preparation_validated;
    if !process_validated {
        evict_cached_artifact(&VECTOR_ARTIFACT_CACHE);
    }
    let vector = cached_artifact(&VECTOR_ARTIFACT_CACHE, &vector_path, || {
        let vector = if lifecycle_validated {
            FileBackedVectorArtifact::open_search_artifact(&vector_path)
        } else {
            FileBackedVectorArtifact::open_artifact(&vector_path)
        }
        .map_err(|_| "configured vector artifact unavailable".to_string())?;
        if vector.corpus_digest != artifact.corpus_digest || vector.len() != artifact.chunk_count {
            return Err("semantic artifact incompatible with the active corpus".into());
        }
        let validated_proof =
            crate::preparation_status::ValidatedVectorArtifact::current_identity(&root);
        if validated_proof != current_proof {
            return Err("configured vector artifact unavailable".into());
        }
        if let Some(proof) = validated_proof {
            *VECTOR_VALIDATION_CACHE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((root.clone(), proof));
        }
        Ok(vector)
    })?;
    let model_id = config
        .semantic_model_id
        .as_deref()
        .ok_or_else(|| "semantic model identity is not configured".to_string())?;
    let model_revision = config
        .semantic_model_revision
        .as_deref()
        .ok_or_else(|| "semantic model revision is not configured".to_string())?;
    if vector.model_id != model_id || vector.model_revision != model_revision {
        return Err("semantic artifact incompatible with the configured model".into());
    }
    Ok(vector)
}

#[cfg(any(feature = "semantic-fastembed", test))]
fn semantic_hits_with_provider<P: EmbeddingProvider + ?Sized>(
    query: &str,
    limit: usize,
    vector: &FileBackedVectorArtifact,
    provider: &mut P,
) -> Result<Vec<SemanticHit>, String> {
    let query = provider
        .embed_query(&semantic_query_text(query))
        .map_err(|error| format!("query embedding failed: {error}"))?;
    vector
        .search(&query, limit)
        .map_err(|error| format!("semantic search failed: {error}"))
}

fn load_semantic_metadata(
    hits: &mut Vec<SemanticHit>,
    filters: &vesc_knowledge_index::LexicalFilters,
    config: &KnowledgeConfig,
    metadata: &mut CandidateMetadataMap,
) -> Result<(), String> {
    let missing = hits
        .iter()
        .map(|hit| hit.chunk_id.clone())
        .filter(|id| !metadata.contains_key(id))
        .collect();
    metadata.extend(load_metadata_ids(&missing, filters, config)?);
    hits.retain(|hit| metadata.contains_key(&hit.chunk_id));
    Ok(())
}

fn load_metadata_ids(
    ids: &BTreeSet<vesc_knowledge_index::ChunkId>,
    filters: &vesc_knowledge_index::LexicalFilters,
    config: &KnowledgeConfig,
) -> Result<CandidateMetadataMap, String> {
    if let Some(root) = resolved_search_artifact(config)? {
        let lexical_path = active_lexical_path(&root)?;
        let repositories_root = config.managed_repositories_root();
        return with_cached_lexical_index(&lexical_path, repositories_root.as_deref(), |index| {
            index
                .metadata_by_id(ids, filters)
                .map_err(|error| error.to_string())
        });
    }
    vesc_knowledge_index::lexical_index()
        .metadata_by_id(ids, filters)
        .map_err(|error| error.to_string())
}

fn hydrate_fused_candidates(
    candidates: Vec<FusedCandidate>,
    config: &KnowledgeConfig,
) -> Result<(Vec<FusedHit>, ChunkMap), String> {
    let ids = candidates
        .iter()
        .map(|candidate| candidate.chunk.chunk_id.clone())
        .collect();
    let mut chunks = ChunkMap::new();
    hydrate_chunk_ids(&ids, config, &mut chunks)?;
    let hits = candidates
        .into_iter()
        .map(|candidate| {
            let chunk = chunks
                .get(&candidate.chunk.chunk_id)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "lexical artifact is missing fused chunk {}",
                        candidate.chunk.chunk_id
                    )
                })?;
            Ok(FusedHit {
                chunk,
                score: candidate.score,
                lexical_rank: candidate.lexical_rank,
                semantic_rank: candidate.semantic_rank,
                lexical_score: candidate.lexical_score,
                semantic_similarity: candidate.semantic_similarity,
                exact_identifier: candidate.exact_identifier,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((hits, chunks))
}

fn hydrate_adjacent_chunks(
    hits: &[FusedHit],
    config: &KnowledgeConfig,
    chunks: &mut ChunkMap,
) -> Result<(), String> {
    let mut frontier = hits
        .iter()
        .map(|hit| hit.chunk.chunk_id.clone())
        .collect::<BTreeSet<_>>();
    let mut seen = frontier.clone();
    for _ in 0..MAX_CONTEXT_NEIGHBORS {
        let adjacent = frontier
            .iter()
            .filter_map(|id| chunks.get(id))
            .flat_map(|chunk| {
                [chunk.previous_chunk.clone(), chunk.next_chunk.clone()]
                    .into_iter()
                    .flatten()
            })
            .filter(|id| seen.insert(id.clone()))
            .collect::<BTreeSet<_>>();
        if adjacent.is_empty() {
            break;
        }
        hydrate_chunk_ids(&adjacent, config, chunks)?;
        frontier = adjacent
            .into_iter()
            .filter(|id| chunks.contains_key(id))
            .collect();
    }
    Ok(())
}

fn hydrate_chunk_ids(
    ids: &BTreeSet<vesc_knowledge_index::ChunkId>,
    config: &KnowledgeConfig,
    chunks: &mut ChunkMap,
) -> Result<(), String> {
    let missing = ids
        .iter()
        .filter(|id| !chunks.contains_key(*id))
        .cloned()
        .collect::<BTreeSet<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let loaded = if let Some(root) = resolved_search_artifact(config)? {
        let lexical_path = active_lexical_path(&root)?;
        let repositories_root = config.managed_repositories_root();
        with_cached_lexical_index(&lexical_path, repositories_root.as_deref(), |index| {
            index
                .chunks_by_id(&missing)
                .map_err(|error| error.to_string())
        })?
    } else {
        let index = vesc_knowledge_index::lexical_index();
        missing
            .iter()
            .filter_map(|id| {
                index
                    .chunks()
                    .get(id)
                    .cloned()
                    .map(|chunk| (id.clone(), chunk))
            })
            .collect()
    };
    chunks.extend(loaded);
    Ok(())
}

fn fused_result(
    hit: vesc_knowledge_index::FusedHit,
    context: &ExpandedContext,
    filters: &vesc_knowledge_index::LexicalFilters,
) -> SearchVescKnowledgeResult {
    let chunk = hit.chunk;
    let id = chunk
        .registered_id
        .clone()
        .unwrap_or_else(|| chunk.chunk_id.to_string());
    let line = chunk.source_span.as_ref().map_or(0, |span| span.start_line);
    let source_span = chunk.source_span;
    let chunk_id = chunk.chunk_id.to_string();
    let document_id = chunk.document_id.to_string();
    let passage = context.passage.clone();
    let heading_path = chunk.heading_path.clone();
    let resource_uri = chunk.resource_uri.as_ref().map(ToString::to_string);
    let document_uri = Some(format!("vesc://knowledge/document/{document_id}"));
    SearchVescKnowledgeResult {
        id,
        name: chunk.title.clone(),
        category: chunk.category.map_or_else(
            || "unknown".into(),
            |category| category_label(category).into(),
        ),
        // Keep the anchor as the internal diversity identity; the bounded
        // response rewrites `summary` to the expanded passage after ranking.
        summary: chunk.text.clone(),
        source: SearchVescKnowledgeSource {
            repo: chunk.repository.to_string(),
            path: chunk.path.clone(),
            line,
            end_line: source_span.map(|span| span.end_line),
            start_byte: source_span.and_then(|span| span.start_byte),
            end_byte: source_span.and_then(|span| span.end_byte),
            revision: Some(chunk.revision.to_string()),
        },
        score: if hit.exact_identifier { 1_000_000 } else { 1 },
        chunk_id: Some(chunk_id.clone()),
        document_id: Some(document_id.clone()),
        passage: Some(passage.clone()),
        heading_path: Some(heading_path.clone()),
        resource_uri: resource_uri.clone(),
        document_uri,
        retrieval_score: Some(hit.score),
        origin: None,
        correction_ids: Vec::new(),
        provenance: Some(SearchVescKnowledgeProvenance {
            document_id,
            chunk_id,
            passage,
            heading_path,
            resource_uri,
            revision: Some(chunk.revision.to_string()),
            source_span: source_span.map(|span| SearchVescKnowledgeSpan {
                start_line: span.start_line,
                end_line: span.end_line,
                start_byte: span.start_byte,
                end_byte: span.end_byte,
            }),
        }),
        explanation: Some(SearchVescKnowledgeExplanation {
            lexical_rank: hit.lexical_rank,
            lexical_score: hit.lexical_score.map(f64::from),
            semantic_rank: hit.semantic_rank,
            semantic_similarity: hit.semantic_similarity.map(f64::from),
            fusion_score: Some(hit.score),
            exact_identifier: hit.exact_identifier,
            filter_effects: filter_effects(filters),
            expansion_reason: context.reason.clone(),
        }),
        occurrence: None,
    }
}

fn retain_diverse_results(
    results: Vec<SearchVescKnowledgeResult>,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    preferred_revisions: &BTreeMap<String, String>,
) -> Vec<SearchVescKnowledgeResult> {
    if filters.revision.is_some() {
        return results.into_iter().take(limit).collect();
    }
    let mut retained = Vec::with_capacity(limit);
    let mut seen = BTreeSet::new();
    for result in results {
        let key = (
            result.source.repo.clone(),
            result.source.path.clone(),
            normalized_passage(&result),
        );
        if seen.insert(key) {
            if retained.len() < limit {
                retained.push(result);
            }
            continue;
        }
        let Some(existing_index) = retained.iter().position(|existing| {
            existing.source.repo == result.source.repo
                && existing.source.path == result.source.path
                && normalized_passage(existing) == normalized_passage(&result)
        }) else {
            continue;
        };
        let candidate_revision = result.source.revision.clone();
        let existing = &mut retained[existing_index];
        let occurrence = existing
            .occurrence
            .get_or_insert_with(|| SearchVescKnowledgeOccurrence {
                count: 1,
                revisions: existing.source.revision.clone().into_iter().collect(),
                first_revision: existing.source.revision.clone(),
                last_revision: existing.source.revision.clone(),
                representative_id: existing.id.clone(),
            });
        occurrence.count += 1;
        if let Some(revision) = candidate_revision.as_ref() {
            if occurrence.revisions.len() < 8 && !occurrence.revisions.contains(revision) {
                occurrence.revisions.push(revision.clone());
            }
            occurrence.last_revision = Some(revision.clone());
        }
        if preferred_revisions
            .get(&result.source.repo)
            .is_some_and(|preferred| candidate_revision.as_deref() == Some(preferred))
            && existing.source.revision != candidate_revision
        {
            let occurrence = existing.occurrence.take();
            let mut replacement = result;
            replacement.occurrence = occurrence;
            retained[existing_index] = replacement;
            let representative_id = retained[existing_index].id.clone();
            if let Some(occurrence) = &mut retained[existing_index].occurrence {
                occurrence.representative_id = representative_id;
            }
        }
    }
    retained
}

fn preferred_revisions(config: &KnowledgeConfig) -> BTreeMap<String, String> {
    config
        .resolved_artifact()
        .map(|artifact| {
            artifact
                .repositories
                .into_iter()
                .map(|(repository, commit)| (repository.to_string(), commit))
                .collect()
        })
        .unwrap_or_default()
}

fn normalized_passage(result: &SearchVescKnowledgeResult) -> String {
    result
        .summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn active_lexical_path(root: &Path) -> Result<std::path::PathBuf, String> {
    if root.is_file() {
        return Ok(root.to_owned());
    }
    active_artifact_summary(root)
        .map(|artifact| {
            root.join("generations")
                .join(artifact.generation.to_string())
                .join("lexical.json")
        })
        .map_err(|_| "configured lexical artifact unavailable".to_string())
}

fn lexical_result(
    hit: vesc_knowledge_index::LexicalHit,
    rank: usize,
    filters: &vesc_knowledge_index::LexicalFilters,
) -> SearchVescKnowledgeResult {
    let chunk = hit.chunk;
    let name = chunk.title.clone();
    let id = chunk
        .registered_id
        .clone()
        .unwrap_or_else(|| chunk.chunk_id.to_string());
    let line = chunk.source_span.as_ref().map_or(0, |span| span.start_line);
    let source_span = chunk.source_span;
    let chunk_id = chunk.chunk_id.to_string();
    let document_id = chunk.document_id.to_string();
    let passage = chunk.text.clone();
    let heading_path = chunk.heading_path.clone();
    let resource_uri = chunk.resource_uri.as_ref().map(ToString::to_string);
    let document_uri = Some(format!("vesc://knowledge/document/{document_id}"));
    SearchVescKnowledgeResult {
        id,
        name,
        category: chunk.category.map_or_else(
            || "unknown".into(),
            |category| category_label(category).into(),
        ),
        summary: chunk.text.clone(),
        source: SearchVescKnowledgeSource {
            repo: chunk.repository.to_string(),
            path: chunk.path.clone(),
            line,
            end_line: source_span.map(|span| span.end_line),
            start_byte: source_span.and_then(|span| span.start_byte),
            end_byte: source_span.and_then(|span| span.end_byte),
            revision: Some(chunk.revision.to_string()),
        },
        score: if hit.exact_identifier { 1_000_000 } else { 1 },
        chunk_id: Some(chunk_id.clone()),
        document_id: Some(document_id.clone()),
        passage: Some(passage.clone()),
        heading_path: Some(heading_path.clone()),
        resource_uri: resource_uri.clone(),
        document_uri,
        retrieval_score: Some(f64::from(hit.score)),
        origin: None,
        correction_ids: Vec::new(),
        provenance: Some(SearchVescKnowledgeProvenance {
            document_id,
            chunk_id,
            passage,
            heading_path,
            resource_uri,
            revision: Some(chunk.revision.to_string()),
            source_span: source_span.map(|span| SearchVescKnowledgeSpan {
                start_line: span.start_line,
                end_line: span.end_line,
                start_byte: span.start_byte,
                end_byte: span.end_byte,
            }),
        }),
        explanation: Some(SearchVescKnowledgeExplanation {
            lexical_rank: Some(rank + 1),
            lexical_score: Some(f64::from(hit.score)),
            semantic_rank: None,
            semantic_similarity: None,
            fusion_score: None,
            exact_identifier: hit.exact_identifier,
            filter_effects: filter_effects(filters),
            expansion_reason: None,
        }),
        occurrence: None,
    }
}

fn filter_effects(filters: &vesc_knowledge_index::LexicalFilters) -> Vec<String> {
    let mut effects = Vec::new();
    if let Some(category) = filters.category {
        effects.push(format!("category={}", category_label(category)));
    }
    if let Some(repository) = &filters.repository {
        effects.push(format!("repository={repository}"));
    }
    effects.extend(filters.paths.iter().map(|path| format!("path={path}")));
    if let Some(revision) = &filters.revision {
        effects.push(format!("revision={revision}"));
    }
    if let Some(trust_tier) = filters.trust_tier {
        effects.push(format!("trust_tier={trust_tier:?}"));
    }
    if let Some(source_kind) = filters.source_kind {
        effects.push(format!("source_kind={source_kind:?}"));
    }
    effects.extend(filters.tags.iter().map(|tag| format!("tag={tag}")));
    effects
}

const fn category_label(category: Category) -> &'static str {
    match category {
        Category::FirmwareApi => "firmware_api",
        Category::Lispbm => "lispbm",
        Category::PackageBuild => "package_build",
        Category::RefloatCommand => "refloat_command",
        Category::NativeLibAbi => "native_lib_abi",
    }
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn search_vesc_knowledge_json(params: &SearchVescKnowledgeParams) -> String {
    let response = search_vesc_knowledge_tool(params);
    serialize_search_response(&response, params.detail, &params.query)
}

/// Serialize a search response using the resolved server configuration.
#[must_use]
pub fn search_vesc_knowledge_json_with_config(
    params: &SearchVescKnowledgeParams,
    config: &KnowledgeConfig,
) -> String {
    let response = search_vesc_knowledge_tool_with_config(params, config);
    serialize_search_response(&response, params.detail, &params.query)
}

/// Serialize a search response augmented with durable learned notes and corrections.
#[must_use]
pub fn search_vesc_knowledge_json_with_feedback(
    params: &SearchVescKnowledgeParams,
    config: &KnowledgeConfig,
    feedback: Option<&FeedbackStore>,
    resources: &ResourceRegistry,
) -> String {
    let mut response = search_vesc_knowledge_tool_with_config(params, config);
    if response.ok
        && let Some(store) = feedback
    {
        let limit = if params.limit == 0 {
            default_search_limit()
        } else {
            params.limit
        };
        let feedback = parse_filters(params)
            .map_err(|error| format!("feedback filters unavailable: {error}"))
            .and_then(|filters| {
                search_feedback(&params.query, store, resources, &filters, limit)
                    .map_err(|error| error.to_string())
            });
        match feedback {
            Ok(matches) => {
                response.corrections = matches.corrections;
                annotate_affected_results(&mut response.results, &response.corrections);
                let notes = matches
                    .notes
                    .into_iter()
                    .take(limit)
                    .map(feedback_note_result)
                    .collect::<Vec<_>>();
                response.results.truncate(limit.saturating_sub(notes.len()));
                response.results.extend(notes);
            }
            Err(error) => response
                .warnings
                .push(format!("feedback retrieval unavailable: {error}")),
        }
        response = response.bounded(params, config, params.detail);
    }
    serialize_search_response(&response, params.detail, &params.query)
}

fn replay_search_params(
    correction: &crate::tools::knowledge_feedback::KnowledgeCorrection,
) -> Result<SearchVescKnowledgeParams, String> {
    let mode = correction
        .retrieval_trace
        .mode
        .as_ref()
        .map(|mode| {
            serde_json::from_value(serde_json::Value::String(mode.clone()))
                .map_err(|_| format!("unsupported replay mode {mode:?}"))
        })
        .transpose()?;
    let mut filters = SearchVescKnowledgeFilters::default();
    for filter in &correction.retrieval_trace.filters {
        let Some((key, value)) = filter.split_once('=') else {
            return Err(format!("malformed replay filter {filter:?}"));
        };
        if value.is_empty() {
            return Err(format!("empty replay filter value for {key:?}"));
        }
        match key {
            "category" if filters.category.is_none() => filters.category = Some(value.into()),
            "repository" if filters.repository.is_none() => {
                filters.repository = Some(value.into());
            }
            "revision" if filters.revision.is_none() => filters.revision = Some(value.into()),
            "trust_tier" if filters.trust_tier.is_none() => {
                filters.trust_tier = Some(value.into());
            }
            "source_kind" if filters.source_kind.is_none() => {
                filters.source_kind = Some(value.into());
            }
            "tag" | "tags" => filters.tags.push(value.into()),
            "category" | "repository" | "revision" | "trust_tier" | "source_kind" => {
                return Err(format!("duplicate replay filter {key:?}"));
            }
            _ => return Err(format!("unsupported replay filter {key:?}")),
        }
    }
    Ok(SearchVescKnowledgeParams {
        query: correction.retrieval_trace.query.clone(),
        snapshot_id: None,
        limit: correction.retrieval_trace.limit,
        mode,
        filters,
        max_response_bytes: correction.retrieval_trace.max_response_bytes,
        max_context_bytes: correction.retrieval_trace.max_context_bytes,
        detail: SearchResponseDetail::Full,
    })
}

#[must_use]
pub fn replay_vesc_knowledge_correction(
    params: &ReplayVescKnowledgeCorrectionParams,
    config: &KnowledgeConfig,
    store: &FeedbackStore,
) -> CorrectionReplayReport {
    let failure = |query: String, error: String| {
        CorrectionReplayReport::failure(&params.correction_id, query, error)
    };
    if params.mark_covered && params.authorization.is_none() {
        return failure(
            String::new(),
            "authorization is required when mark_covered is true".into(),
        );
    }
    let record = match store.get(&params.correction_id) {
        Ok(Some(record)) => record,
        Ok(None) => return failure(String::new(), "correction not found".into()),
        Err(error) => return failure(String::new(), error.to_string()),
    };
    let crate::tools::knowledge_feedback::KnowledgeRecord::Correction(correction) = record else {
        return failure(String::new(), "record is not a correction".into());
    };

    let mut warnings = Vec::new();
    let replay = match replay_search_params(&correction) {
        Ok(replay) => replay,
        Err(error) => return failure(correction.retrieval_trace.query.clone(), error),
    };
    let response = search_vesc_knowledge_tool_with_config(&replay, config);
    warnings.extend(response.warnings);
    if !response.ok {
        return CorrectionReplayReport {
            ok: false,
            correction_id: correction.id,
            query: replay.query,
            covered_by_base_knowledge: false,
            marked_covered: false,
            matched_decisive_evidence: Vec::new(),
            missing_decisive_evidence: correction.retrieval_trace.decisive_evidence,
            ordered_result_ids: Vec::new(),
            warnings,
            error: response.error,
        };
    }

    let ordered_result_ids = response
        .results
        .iter()
        .map(|result| result.id.clone())
        .collect::<Vec<_>>();
    let mut matched_decisive_evidence = Vec::new();
    let mut missing_decisive_evidence = Vec::new();
    for decisive in &correction.retrieval_trace.decisive_evidence {
        let matched = response.results.iter().any(|result| {
            result.id == *decisive
                || result.chunk_id.as_deref() == Some(decisive)
                || result.document_id.as_deref() == Some(decisive)
                || result.resource_uri.as_deref() == Some(decisive)
                || result.document_uri.as_deref() == Some(decisive)
        });
        if matched {
            matched_decisive_evidence.push(decisive.clone());
        } else {
            missing_decisive_evidence.push(decisive.clone());
        }
    }
    let covered_by_base_knowledge = !correction.retrieval_trace.decisive_evidence.is_empty()
        && missing_decisive_evidence.is_empty();
    let mut marked_covered = false;
    if covered_by_base_knowledge && params.mark_covered {
        if let Err(error) =
            store.mark_correction_covered(&correction.id, &matched_decisive_evidence)
        {
            return failure(replay.query, error.to_string());
        }
        marked_covered = true;
    } else if !covered_by_base_knowledge {
        warnings.push(
            "base knowledge replay still misses decisive evidence; keep the advisory active and apply its recommended data actions"
                .into(),
        );
    }

    CorrectionReplayReport {
        ok: true,
        correction_id: correction.id,
        query: replay.query,
        covered_by_base_knowledge,
        marked_covered,
        matched_decisive_evidence,
        missing_decisive_evidence,
        ordered_result_ids,
        warnings,
        error: None,
    }
}

fn feedback_note_result(
    matched: crate::tools::knowledge_feedback::FeedbackNoteMatch,
) -> SearchVescKnowledgeResult {
    let id = matched.note.id;
    let summary = matched.note.lesson;
    SearchVescKnowledgeResult {
        name: format!("Learned note: {}", matched.note.question),
        category: "model_feedback".into(),
        source: SearchVescKnowledgeSource {
            repo: "vesc-mcp-feedback".into(),
            path: format!("feedback/{id}.json"),
            line: 0,
            end_line: None,
            start_byte: None,
            end_byte: None,
            revision: Some("runtime-feedback-v1".into()),
        },
        score: 1,
        chunk_id: None,
        document_id: None,
        passage: Some(summary.clone()),
        heading_path: None,
        resource_uri: Some(format!("vesc://knowledge/feedback/{id}")),
        document_uri: None,
        retrieval_score: Some(f64::from(matched.score)),
        origin: Some("unverified_model_feedback".into()),
        correction_ids: Vec::new(),
        provenance: None,
        explanation: None,
        occurrence: None,
        id,
        summary,
    }
}

fn annotate_affected_results(
    results: &mut [SearchVescKnowledgeResult],
    corrections: &[KnowledgeCorrectionResult],
) {
    for result in results {
        let correction_ids = corrections
            .iter()
            .filter(|correction| correction_affects_result(correction, result))
            .map(|correction| correction.id.clone())
            .collect::<Vec<_>>();
        result.correction_ids.extend(correction_ids);
    }
}

fn correction_affects_result(
    correction: &KnowledgeCorrectionResult,
    result: &SearchVescKnowledgeResult,
) -> bool {
    correction.affected_resources.iter().any(|affected| {
        affected == &result.id
            || result.chunk_id.as_ref() == Some(affected)
            || result.document_id.as_ref() == Some(affected)
            || result.resource_uri.as_ref() == Some(affected)
            || result.document_uri.as_ref() == Some(affected)
    })
}

fn serialize_search_response(
    response: &SearchVescKnowledgeResponse,
    detail: SearchResponseDetail,
    query: &str,
) -> String {
    match detail {
        SearchResponseDetail::Compact => serde_json::to_string(&compact_response(response, query)),
        SearchResponseDetail::Full => serde_json::to_string(response),
    }
    .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_cache_serializes_first_load() {
        let cache: &'static ArtifactCache<usize> = Box::leak(Box::new(OnceLock::new()));
        let path = PathBuf::from("vectors.bin");
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (first_entered_tx, first_entered_rx) = std::sync::mpsc::channel();
        let first_release = Arc::clone(&release);
        let first_path = path.clone();
        let first = std::thread::spawn(move || {
            cached_artifact(cache, &first_path, || {
                first_entered_tx.send(()).expect("report first load");
                let (lock, wake) = &*first_release;
                let mut released = lock.lock().expect("release mutex");
                while !*released {
                    released = wake.wait(released).expect("release wait");
                }
                drop(released);
                Ok(7)
            })
        });
        first_entered_rx.recv().expect("first load started");

        let (second_entered_tx, second_entered_rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            cached_artifact(cache, &path, || {
                second_entered_tx.send(()).expect("report second load");
                Ok(8)
            })
        });
        let loaded_twice = second_entered_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_ok();

        let (lock, wake) = &*release;
        *lock.lock().expect("release mutex") = true;
        wake.notify_all();
        let first = first.join().expect("first loader").expect("first value");
        let second = second.join().expect("second loader").expect("second value");

        assert!(!loaded_twice, "the artifact was loaded concurrently");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn artifact_cache_drops_old_generation_before_loading_new() {
        let cache: &'static ArtifactCache<usize> = Box::leak(Box::new(OnceLock::new()));
        let old_value =
            cached_artifact(cache, Path::new("old/vectors.bin"), || Ok(7)).expect("old generation");
        let old = Arc::downgrade(&old_value);
        drop(old_value);
        assert!(old.upgrade().is_some(), "old generation remains cached");

        let value = cached_artifact(cache, Path::new("new/vectors.bin"), || {
            assert!(
                old.upgrade().is_none(),
                "old generation remained cached while loading its replacement"
            );
            Ok(8)
        })
        .expect("new generation");

        assert_eq!(*value, 8);
    }

    #[test]
    fn artifact_cache_evicts_idle_generation() {
        let cache: &'static ArtifactCache<usize> = Box::leak(Box::new(OnceLock::new()));
        let value =
            cached_artifact(cache, Path::new("vectors.bin"), || Ok(7)).expect("cached value");
        let retained = Arc::downgrade(&value);
        drop(value);

        evict_cached_artifact(cache);

        assert!(retained.upgrade().is_none());
    }

    #[test]
    fn semantic_reaper_evicts_provider_and_vector_at_idle_deadline() {
        struct TestEntry {
            last_used: Instant,
            idle_timeout: Duration,
            dropped: Option<std::sync::mpsc::Sender<Instant>>,
        }

        impl Drop for TestEntry {
            fn drop(&mut self) {
                self.dropped
                    .take()
                    .expect("drop notification")
                    .send(Instant::now())
                    .expect("report drop");
            }
        }

        let idle_timeout = Duration::from_millis(10);
        let started = Instant::now();
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let vector_cache: &'static ArtifactCache<usize> = Box::leak(Box::new(OnceLock::new()));
        let vector = cached_artifact(vector_cache, Path::new("vectors.bin"), || Ok(7))
            .expect("cached vector");
        let retained_vector = Arc::downgrade(&vector);
        drop(vector);
        let cache = Arc::new(SemanticModelCache {
            state: Mutex::new(Some(TestEntry {
                last_used: started,
                idle_timeout,
                dropped: Some(dropped_tx),
            })),
            wake: Condvar::new(),
        });
        let reaper_cache = Arc::clone(&cache);
        let reaper = std::thread::spawn(move || {
            reap_one_idle_entry(
                &reaper_cache,
                |entry| entry.idle_timeout.saturating_sub(entry.last_used.elapsed()),
                vector_cache,
            );
        });

        let dropped_at = dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("provider was evicted");
        reaper.join().expect("reaper completed");

        assert!(dropped_at.duration_since(started) >= idle_timeout);
        assert!(cache.state.lock().expect("cache mutex").is_none());
        assert!(retained_vector.upgrade().is_none());
    }

    #[test]
    fn semantic_queries_use_one_inference_thread() {
        assert_eq!(semantic_query_intra_threads(), 1);
    }

    #[test]
    fn invalid_category_is_ignored() {
        let resp = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "nvm".into(),
            snapshot_id: None,
            limit: 10,
            mode: Some(SearchMode::Lexical),
            filters: SearchVescKnowledgeFilters {
                category: Some("not_a_category".into()),
                ..SearchVescKnowledgeFilters::default()
            },
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        });
        assert!(resp.ok);
        assert!(resp.error.is_none());
        assert!(!resp.results.is_empty());
    }

    #[test]
    fn repository_and_path_filters_are_accepted() {
        let params: SearchVescKnowledgeParams = serde_json::from_value(serde_json::json!({
            "query": "bms_update",
            "filters": {
                "repository": "refloat",
                "paths": ["src/bms.c"]
            }
        }))
        .expect("filters");

        let filters = parse_filters(&params).expect("parsed filters");
        assert_eq!(
            filters.repository.as_ref().map(ToString::to_string),
            Some("refloat".into())
        );
        assert_eq!(filters.paths, vec!["src/bms.c"]);
    }

    #[test]
    fn removed_search_fields_are_rejected() {
        for params in [
            serde_json::json!({ "query": "nvm", "category": "firmware_api" }),
            serde_json::json!({
                "query": "nvm",
                "filters": { "repository_ids": ["vesc"] }
            }),
        ] {
            serde_json::from_value::<SearchVescKnowledgeParams>(params)
                .expect_err("removed field must not be accepted");
        }
    }

    #[test]
    fn zero_limit_uses_default() {
        let resp = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "pkg".into(),
            snapshot_id: None,
            limit: 0,
            mode: Some(SearchMode::Lexical),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        });
        assert!(resp.ok);
        assert!(!resp.results.is_empty());
    }

    #[test]
    fn invalid_limit_returns_structured_validation_remediation() {
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "nvm".into(),
                snapshot_id: None,
                limit: 999,
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Lexical,
                max_limit: 10,
                ..KnowledgeConfig::default()
            },
        );

        let validation = response.validation.expect("validation details");
        assert_eq!(validation.field, "limit");
        assert_eq!(validation.rejected_value, "999");
        assert!(validation.accepted.contains("1..=10"));
        assert!(validation.clamping_safe);
        assert_eq!(response.warning_codes, vec!["validation_failed"]);
    }

    #[test]
    fn category_label_maps_firmware_api() {
        assert_eq!(
            category_label(vesc_knowledge_index::Category::FirmwareApi),
            "firmware_api"
        );
    }

    #[test]
    fn omitted_mode_and_limits_use_resolved_knowledge_config() {
        let config = KnowledgeConfig {
            mode: RetrievalMode::Lexical,
            max_limit: 1,
            max_query_bytes: 32,
            max_response_bytes: 64 * 1024,
            max_passage_bytes: 128,
            ..KnowledgeConfig::default()
        };
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "nvm".into(),
                snapshot_id: None,
                limit: 1,
                mode: None,
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &config,
        );

        assert!(response.ok);
        assert_eq!(response.mode, SearchMode::Lexical);
        assert!(response.results.len() <= 1);
        let limits = &response.index.expect("index metadata").limits;
        assert_eq!(limits.default_limit, 10);
        assert_eq!(limits.max_limit, 1);
        assert_eq!(limits.default_detail, SearchResponseDetail::Full);
    }

    #[test]
    fn omitted_detail_defaults_to_full_evidence() {
        let params: SearchVescKnowledgeParams = serde_json::from_value(serde_json::json!({
            "query": "nvm",
        }))
        .expect("search params");

        assert_eq!(params.detail, SearchResponseDetail::Full);
    }

    #[test]
    fn search_schema_advertises_detail_profiles() {
        let schema = serde_json::to_value(schemars::schema_for!(SearchVescKnowledgeParams))
            .expect("search schema");
        let detail = &schema["properties"]["detail"];
        let detail_definition = detail;

        assert!(detail_definition["oneOf"].is_array());
        assert!(
            schema["properties"]["mode"]["anyOf"]
                .as_array()
                .is_some_and(|variants| variants.iter().any(|variant| variant["oneOf"].is_array()))
        );
        assert_eq!(schema["properties"]["filters"]["type"], "object");
        assert_eq!(schema["properties"]["limit"]["default"], 10);
        assert_eq!(schema["properties"]["limit"]["minimum"], 1);
        assert_eq!(schema["properties"]["max_response_bytes"]["minimum"], 1);
        assert_eq!(schema["properties"]["max_context_bytes"]["minimum"], 1);
        assert_eq!(
            schema["properties"]["max_response_bytes"]["default"],
            65_536
        );
        assert_eq!(schema["properties"]["max_context_bytes"]["default"], 8_192);
        assert!(
            schema["properties"]["query"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("max_query_bytes"))
        );
        assert!(
            schema["properties"]["filters"]["properties"]["repository"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("Exact repository identifier"))
        );
        assert!(
            schema["properties"]["filters"]["properties"]["tags"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("every supplied tag"))
        );
        assert_eq!(
            detail_definition["oneOf"]
                .as_array()
                .expect("detail variants")
                .iter()
                .map(|variant| variant["const"].clone())
                .collect::<Vec<_>>(),
            vec![serde_json::json!("full"), serde_json::json!("compact")]
        );
    }

    #[test]
    fn capabilities_report_effective_search_limits() {
        let config = KnowledgeConfig {
            max_limit: 7,
            max_query_bytes: 123,
            max_response_bytes: 456,
            max_passage_bytes: 789,
            ..KnowledgeConfig::default()
        };

        let value: serde_json::Value =
            serde_json::from_str(&search_vesc_knowledge_capabilities_json(&config))
                .expect("capabilities JSON");

        assert_eq!(value["limits"]["max_limit"], 7);
        assert_eq!(value["limits"]["max_query_bytes"], 123);
        assert_eq!(value["limits"]["max_response_bytes"], 456);
        assert_eq!(value["limits"]["max_context_bytes"], 789);
        assert_eq!(value["limits"]["default_detail"], "full");
    }

    #[test]
    fn full_context_admits_three_adjacent_chunks() {
        use vesc_knowledge_index::{Chunk, NormalizedDocument, RepositoryId, Revision, SourceKind};

        let document = NormalizedDocument::new(
            "doc",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            "docs/doc.md",
            "text/markdown",
            "one two three four",
        )
        .expect("document");
        let mut chunks = (0..4)
            .map(|index| {
                Chunk::from_document(
                    &document,
                    index,
                    ["one", "two", "three", "four"][index as usize].into(),
                    Vec::new(),
                    None,
                )
                .expect("chunk")
            })
            .collect::<Vec<_>>();
        for index in 0..3 {
            chunks[index].next_chunk = Some(chunks[index + 1].chunk_id.clone());
            chunks[index + 1].previous_chunk = Some(chunks[index].chunk_id.clone());
        }
        let map = chunks
            .iter()
            .cloned()
            .map(|chunk| (chunk.chunk_id.clone(), chunk))
            .collect();

        let context = vesc_knowledge_index::expand_adjacent_context(
            &chunks[0],
            &map,
            MAX_CONTEXT_NEIGHBORS,
            4096,
        );
        assert_eq!(context.neighbor_count, 3);
        assert!(context.passage.contains("four"));
    }

    #[test]
    fn lexical_full_results_expand_adjacent_context() {
        use vesc_knowledge_index::{Chunk, NormalizedDocument, RepositoryId, Revision, SourceKind};

        let document = NormalizedDocument::new(
            "doc",
            SourceKind::Markdown,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("rev").expect("revision"),
            "docs/doc.md",
            "text/markdown",
            "anchor before after",
        )
        .expect("document");
        let mut chunks = (0..3)
            .map(|index| {
                Chunk::from_document(
                    &document,
                    index,
                    ["anchor", "before", "after"][index as usize].into(),
                    Vec::new(),
                    None,
                )
                .expect("chunk")
            })
            .collect::<Vec<_>>();
        chunks[0].next_chunk = Some(chunks[1].chunk_id.clone());
        chunks[1].previous_chunk = Some(chunks[0].chunk_id.clone());
        chunks[1].next_chunk = Some(chunks[2].chunk_id.clone());
        chunks[2].previous_chunk = Some(chunks[1].chunk_id.clone());
        let map = chunks
            .iter()
            .cloned()
            .map(|chunk| (chunk.chunk_id.clone(), chunk))
            .collect::<ChunkMap>();
        let hit = FusedHit {
            chunk: chunks[1].clone(),
            score: 1.0,
            lexical_rank: Some(1),
            semantic_rank: None,
            lexical_score: Some(1.0),
            semantic_similarity: None,
            exact_identifier: true,
        };

        let results = lexical_results_with_context(
            vec![hit],
            &vesc_knowledge_index::LexicalFilters::default(),
            &map,
            4096,
        );

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert!(
            result
                .passage
                .as_deref()
                .is_some_and(|passage| { passage.contains("anchor") && passage.contains("after") })
        );
        assert_eq!(
            result
                .explanation
                .as_ref()
                .and_then(|explanation| explanation.expansion_reason.as_deref()),
            Some("adjacent chunks included")
        );
    }

    #[test]
    fn unversioned_search_collapses_duplicate_history_passages() {
        let filters = vesc_knowledge_index::LexicalFilters::default();
        let results = vec![
            history_test_result("old", "abc123", "same body"),
            history_test_result("new", "def456", "same   body"),
            history_test_result("new", "ghi789", "different body"),
        ];

        let retained = retain_diverse_results(results, &filters, 10, &BTreeMap::new());

        assert_eq!(
            retained
                .iter()
                .map(|result| result.id.as_str())
                .collect::<Vec<_>>(),
            vec!["old", "new"]
        );
        let occurrence = retained[0].occurrence.as_ref().expect("history summary");
        assert_eq!(occurrence.count, 2);
        assert_eq!(occurrence.revisions, vec!["abc123", "def456"]);
        assert_eq!(occurrence.representative_id, "old");
    }

    #[test]
    fn revision_filter_preserves_duplicate_history_passages() {
        let filters = vesc_knowledge_index::LexicalFilters {
            revision: Some("abc123".try_into().expect("revision")),
            ..vesc_knowledge_index::LexicalFilters::default()
        };
        let results = vec![
            history_test_result("old", "abc123", "same body"),
            history_test_result("new", "def456", "same body"),
        ];

        let retained = retain_diverse_results(results, &filters, 10, &BTreeMap::new());

        assert_eq!(retained.len(), 2);
    }

    #[test]
    fn duplicate_history_prefers_the_configured_default_revision() {
        let filters = vesc_knowledge_index::LexicalFilters::default();
        let results = vec![
            history_test_result("old", "oldrev", "same body"),
            history_test_result("default", "defaultrev", "same body"),
        ];
        let preferred = BTreeMap::from([(String::from("vesc"), String::from("defaultrev"))]);

        let retained = retain_diverse_results(results, &filters, 10, &preferred);

        assert_eq!(retained[0].id, "default");
        assert_eq!(
            retained[0].occurrence.as_ref().expect("occurrence").count,
            2
        );
        assert_eq!(
            retained[0]
                .occurrence
                .as_ref()
                .expect("occurrence")
                .representative_id,
            "default"
        );
    }

    fn history_test_result(id: &str, revision: &str, passage: &str) -> SearchVescKnowledgeResult {
        SearchVescKnowledgeResult {
            id: id.into(),
            name: "motor.rs".into(),
            category: "firmware_api".into(),
            summary: passage.into(),
            source: SearchVescKnowledgeSource {
                repo: "vesc".into(),
                path: "motor.rs".into(),
                line: 1,
                end_line: None,
                start_byte: None,
                end_byte: None,
                revision: Some(revision.into()),
            },
            score: 1,
            chunk_id: None,
            document_id: None,
            passage: Some(passage.into()),
            heading_path: None,
            resource_uri: None,
            document_uri: None,
            retrieval_score: None,
            origin: None,
            correction_ids: Vec::new(),
            provenance: None,
            explanation: None,
            occurrence: None,
        }
    }

    #[test]
    fn configured_lexical_mode_rejects_semantic_request_override() {
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "nvm".into(),
                snapshot_id: None,
                limit: 1,
                mode: Some(SearchMode::Hybrid),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Lexical,
                ..KnowledgeConfig::default()
            },
        );

        assert!(!response.ok);
        assert_eq!(response.mode, SearchMode::Hybrid);
        assert!(response.results.is_empty());
        assert!(response.error.as_deref().is_some_and(|error| {
            error.contains("configured for lexical search")
                && error.contains("retry with mode \"lexical\"")
        }));
    }

    #[cfg(not(feature = "semantic-fastembed"))]
    #[test]
    fn explicit_hybrid_without_a_model_returns_structured_error() {
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "nvm".into(),
                snapshot_id: None,
                limit: 1,
                mode: Some(SearchMode::Hybrid),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Auto,
                ..KnowledgeConfig::default()
            },
        );

        assert!(!response.ok);
        assert_eq!(response.mode, SearchMode::Hybrid);
        assert!(response.error.as_deref().is_some_and(|error| {
            error.contains("semantic-fastembed feature is disabled")
                && error.contains("retry with mode \"lexical\"")
        }));
    }

    #[test]
    fn auto_semantic_failure_returns_lexical_results_with_warning() {
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "nvm".into(),
                snapshot_id: None,
                limit: 1,
                mode: Some(SearchMode::Auto),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Auto,
                ..KnowledgeConfig::default()
            },
        );

        assert!(response.ok);
        assert!(!response.results.is_empty());
        assert!(response.error.is_none());
        assert_eq!(response.mode_requested, SearchMode::Auto);
        assert_eq!(response.mode_used, SearchMode::Lexical);
        assert!(
            response
                .warning_codes
                .iter()
                .any(|code| code == "semantic_unavailable")
        );
        assert!(response.warnings.iter().any(|warning| {
            warning.contains("semantic_unavailable") && warning.contains("lexical")
        }));
    }

    #[test]
    fn auto_handler_falls_back_after_corrupt_vector_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut build_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            temp.path(),
            &mut build_provider,
            "fake",
            "test",
        )
        .expect("semantic artifact build");
        let config = KnowledgeConfig {
            mode: RetrievalMode::Auto,
            artifact_path: Some(temp.path().into()),
            semantic_model_id: Some("fake".into()),
            semantic_model_revision: Some("test".into()),
            ..KnowledgeConfig::default()
        };
        let params = SearchVescKnowledgeParams {
            query: "lbm_add_extension".into(),
            snapshot_id: None,
            limit: 3,
            mode: Some(SearchMode::Auto),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        };
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        let response = search_vesc_knowledge_tool_with_provider(&params, &config, &mut provider);
        assert!(response.ok, "response: {response:?}");
        assert_eq!(response.mode_used, SearchMode::Hybrid);

        let vector_path = vesc_knowledge_index::active_generation_path(temp.path())
            .expect("active generation")
            .join("vectors.bin");
        let mut bytes = std::fs::read(&vector_path).expect("read vectors");
        let payload_byte = bytes.len() / 2;
        bytes[payload_byte] ^= 1;
        std::fs::write(&vector_path, bytes).expect("corrupt vectors");

        let response = search_vesc_knowledge_tool_with_provider(&params, &config, &mut provider);
        assert!(response.ok, "response: {response:?}");
        assert_eq!(response.mode_used, SearchMode::Lexical);
        assert!(!response.results.is_empty());
        assert!(
            response
                .warning_codes
                .iter()
                .any(|code| code == "semantic_unavailable")
        );
        assert!(
            response
                .results
                .iter()
                .all(|result| result.provenance.is_some())
        );
    }

    #[test]
    fn auto_handler_failure_matrix_keeps_lexical_evidence_and_hybrid_strict() {
        for failure in ["missing", "incompatible", "provider"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let (model_id, mut provider) = match failure {
                "missing" => {
                    vesc_knowledge_index::build_embedded_artifacts(temp.path())
                        .expect("lexical artifact build");
                    ("fake", vesc_knowledge_index::FakeEmbeddingProvider::new(8))
                }
                "incompatible" => {
                    let mut build_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
                    vesc_knowledge_index::build_embedded_artifacts_with_provider(
                        temp.path(),
                        &mut build_provider,
                        "other",
                        "test",
                    )
                    .expect("semantic artifact build");
                    ("fake", vesc_knowledge_index::FakeEmbeddingProvider::new(8))
                }
                "provider" => {
                    let mut build_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
                    vesc_knowledge_index::build_embedded_artifacts_with_provider(
                        temp.path(),
                        &mut build_provider,
                        "fake",
                        "test",
                    )
                    .expect("semantic artifact build");
                    ("fake", vesc_knowledge_index::FakeEmbeddingProvider::new(0))
                }
                _ => unreachable!("failure case is exhaustive"),
            };
            let config = KnowledgeConfig {
                mode: RetrievalMode::Auto,
                artifact_path: Some(temp.path().into()),
                semantic_model_id: Some(model_id.into()),
                semantic_model_revision: Some("test".into()),
                ..KnowledgeConfig::default()
            };
            let mut params = SearchVescKnowledgeParams {
                query: "lbm_add_extension".into(),
                snapshot_id: None,
                limit: 3,
                mode: Some(SearchMode::Auto),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            };

            let response =
                search_vesc_knowledge_tool_with_provider(&params, &config, &mut provider);
            assert!(response.ok, "{failure}: {response:?}");
            assert_eq!(response.mode_used, SearchMode::Lexical, "{failure}");
            assert!(!response.results.is_empty(), "{failure}");
            assert!(
                response
                    .results
                    .iter()
                    .all(|result| result.provenance.is_some()),
                "{failure}"
            );
            assert!(
                response
                    .warning_codes
                    .iter()
                    .any(|code| code == "semantic_unavailable"),
                "{failure}"
            );

            params.mode = Some(SearchMode::Hybrid);
            let response =
                search_vesc_knowledge_tool_with_provider(&params, &config, &mut provider);
            assert!(!response.ok, "{failure}: {response:?}");
            assert!(response.results.is_empty(), "{failure}: {response:?}");
            assert_eq!(response.warning_codes, vec!["retrieval_failed"]);
            assert!(
                response
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("retry with mode \"lexical\"")),
                "{failure}: {response:?}"
            );
        }
    }

    #[test]
    fn configured_artifact_is_loaded_for_lexical_search() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("artifact build");
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm_add_extension".into(),
                snapshot_id: None,
                limit: 1,
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Lexical,
                artifact_path: Some(temp.path().into()),
                ..KnowledgeConfig::default()
            },
        );

        assert!(response.ok);
        assert_eq!(response.results[0].id, "native_lib_abi.lbm_add_extension");
        assert!(response.index.is_some());
        assert!(
            response
                .timing
                .is_some_and(|timing| timing.result_count == 1)
        );
        assert!(response.results[0].chunk_id.is_some());
        assert!(response.results[0].document_id.is_some());
        assert!(response.results[0].passage.is_some());
        assert!(response.results[0].source.revision.is_some());
        assert!(response.results[0].source.end_line.is_some());
    }

    #[test]
    fn unavailable_managed_knowledge_never_falls_back_to_static_or_embedded_search() {
        let root = tempfile::tempdir().expect("data root");
        let mut config = crate::config::McpConfig::from_toml(
            &format!(
                r#"
[knowledge]
managed_git = true
artifact_path = "{}"
data_root = "{}"

[[knowledge.repositories]]
id = "vesc"
remote_url = "https://github.com/vedderb/bldc.git"
default_ref = "refs/heads/master"
policy = "required"
include = ["**/*.c"]
exclude = []
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824
"#,
                root.path().join("static-artifact").display(),
                root.path().display(),
            ),
            &crate::managed_repositories::DataRootInputs::default(),
        )
        .expect("managed configuration")
        .knowledge;
        config.mode = RetrievalMode::Lexical;
        crate::preparation_status::write_preparation_status(
            root.path(),
            &crate::preparation_status::KnowledgePreparationStatus::preparing(
                crate::preparation_status::PreparationPhase::PlanningHistory,
                0,
                1,
            )
            .with_freshness_required(true),
        )
        .expect("strict preparation status");

        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm_add_extension".into(),
                snapshot_id: None,
                limit: default_search_limit(),
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::default(),
            },
            &config,
        );

        assert!(!response.ok);
        assert!(response.results.is_empty());
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("managed knowledge is unavailable"))
        );
    }

    #[test]
    fn unknown_explicit_snapshot_never_falls_back_to_default_knowledge() {
        let params: SearchVescKnowledgeParams = serde_json::from_value(serde_json::json!({
            "query": "lbm_add_extension",
            "mode": "lexical",
            "snapshot_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }))
        .expect("search params");

        let response = search_vesc_knowledge_tool_with_config(&params, &KnowledgeConfig::default());

        assert!(!response.ok);
        assert!(response.results.is_empty());
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("list_vesc_source_versions"))
        );
    }

    #[test]
    fn explicit_snapshot_discloses_provenance_and_qualifies_resource_uris() {
        let temp = tempfile::tempdir().expect("tempdir");
        let snapshot_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let artifact = temp.path().join("artifacts").join(snapshot_id);
        vesc_knowledge_index::build_embedded_artifacts(&artifact).expect("artifact build");
        std::fs::create_dir_all(temp.path().join("snapshots")).expect("snapshot directory");
        std::fs::write(
            temp.path()
                .join("snapshots")
                .join(format!("{snapshot_id}.json")),
            serde_json::to_vec(&serde_json::json!({
                "id": snapshot_id,
                "profile": "selected_trees",
                "repositories": [{
                    "repository": "fixture",
                    "commit": "1111111111111111111111111111111111111111"
                }]
            }))
            .expect("snapshot manifest"),
        )
        .expect("write snapshot manifest");
        let params: SearchVescKnowledgeParams = serde_json::from_value(serde_json::json!({
            "query": "lbm_add_extension",
            "mode": "lexical",
            "detail": "full",
            "limit": 1,
            "snapshot_id": snapshot_id
        }))
        .expect("search params");
        let config = KnowledgeConfig {
            data_root: Some(
                crate::managed_repositories::DataRoot::new(temp.path().to_path_buf())
                    .expect("data root"),
            ),
            ..KnowledgeConfig::default()
        };

        let response = search_vesc_knowledge_tool_with_config(&params, &config);

        assert!(response.ok, "{:?}", response.error);
        let index = response.index.as_ref().expect("index metadata");
        assert_eq!(index.snapshot_id.as_deref(), Some(snapshot_id));
        assert_eq!(
            index.repositories.get("fixture").map(String::as_str),
            Some("1111111111111111111111111111111111111111")
        );
        assert!(
            response.results[0]
                .resource_uri
                .as_deref()
                .is_some_and(|uri| uri
                    .starts_with(&format!("vesc://knowledge/snapshot/{snapshot_id}/chunk/")))
        );
        assert!(
            response.results[0]
                .document_uri
                .as_deref()
                .is_some_and(|uri| uri.starts_with(&format!(
                    "vesc://knowledge/snapshot/{snapshot_id}/document/"
                )))
        );
        let compact = compact_response(&response, "query");
        assert_eq!(compact.snapshot_id.as_deref(), Some(snapshot_id));
        assert_eq!(
            compact.repositories.get("fixture").map(String::as_str),
            Some("1111111111111111111111111111111111111111")
        );
    }

    #[test]
    fn compact_symbol_rows_center_exact_identifier_in_code() {
        let response = SearchVescKnowledgeResponse {
            ok: true,
            mode_requested: SearchMode::Lexical,
            mode_used: SearchMode::Lexical,
            mode: SearchMode::Lexical,
            detail: SearchResponseDetail::Full,
            capabilities: Vec::new(),
            corrections: Vec::new(),
            results: vec![SearchVescKnowledgeResult {
                id: "chunk".into(),
                name: "motor.rs".into(),
                category: "firmware_api".into(),
                summary: format!(
                    "{}\nfn update_pid_position_offset(&self, position: PidPosition) {{}}\n{}",
                    "unrelated_symbol(); ".repeat(32),
                    "tail ".repeat(64)
                ),
                source: SearchVescKnowledgeSource {
                    repo: "vesc".into(),
                    path: "motor.rs".into(),
                    line: 1,
                    end_line: None,
                    start_byte: None,
                    end_byte: None,
                    revision: None,
                },
                score: 1,
                chunk_id: None,
                document_id: None,
                passage: None,
                heading_path: None,
                resource_uri: None,
                document_uri: None,
                retrieval_score: None,
                origin: None,
                correction_ids: Vec::new(),
                provenance: None,
                explanation: Some(SearchVescKnowledgeExplanation {
                    lexical_rank: Some(1),
                    lexical_score: Some(1.0),
                    semantic_rank: None,
                    semantic_similarity: None,
                    fusion_score: None,
                    exact_identifier: true,
                    filter_effects: Vec::new(),
                    expansion_reason: None,
                }),
                occurrence: None,
            }],
            error: None,
            warnings: Vec::new(),
            warning_codes: Vec::new(),
            validation: None,
            index: None,
            timing: None,
        };

        let compact = compact_response(&response, "update_pid_position_offset");
        let excerpt = &compact.results[0].2;
        assert!(excerpt.contains("update_pid_position_offset"), "{excerpt}");
        assert!(excerpt.len() <= COMPACT_EXCERPT_BYTES);
    }

    #[test]
    fn hybrid_results_fuse_fake_semantic_candidates_with_lexical_hits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut build_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            temp.path(),
            &mut build_provider,
            "fake",
            "test",
        )
        .expect("semantic artifact build");
        let config = KnowledgeConfig {
            mode: RetrievalMode::Hybrid,
            artifact_path: Some(temp.path().into()),
            semantic_model_dir: None,
            semantic_model_id: Some("fake".into()),
            semantic_model_revision: Some("test".into()),
            ..KnowledgeConfig::default()
        };
        let params = SearchVescKnowledgeParams {
            query: "lbm_add_extension".into(),
            snapshot_id: None,
            limit: 3,
            mode: Some(SearchMode::Hybrid),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        };
        let mut query_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        let filters = vesc_knowledge_index::LexicalFilters::default();
        let (results, live_rerank) =
            hybrid_results_with_provider(&params, &filters, 3, &config, &mut query_provider)
                .expect("hybrid results");

        assert!(!live_rerank);
        assert!(!results.is_empty());
        assert!(results.iter().any(|result| {
            result
                .explanation
                .as_ref()
                .is_some_and(|explanation| explanation.semantic_rank.is_some())
        }));
    }

    #[test]
    fn hybrid_rejects_same_length_vector_corruption() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut build_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            temp.path(),
            &mut build_provider,
            "fake",
            "test",
        )
        .expect("semantic artifact build");
        let vector_path = vesc_knowledge_index::active_generation_path(temp.path())
            .expect("active generation")
            .join("vectors.bin");
        let config = KnowledgeConfig {
            mode: RetrievalMode::Hybrid,
            artifact_path: Some(temp.path().into()),
            semantic_model_dir: None,
            semantic_model_id: Some("fake".into()),
            semantic_model_revision: Some("test".into()),
            ..KnowledgeConfig::default()
        };
        let params = SearchVescKnowledgeParams {
            query: "lbm_add_extension".into(),
            snapshot_id: None,
            limit: 3,
            mode: Some(SearchMode::Hybrid),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        };
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        hybrid_results_with_provider(
            &params,
            &vesc_knowledge_index::LexicalFilters::default(),
            3,
            &config,
            &mut provider,
        )
        .expect("initial validated hybrid results");

        let mut bytes = std::fs::read(&vector_path).expect("read vectors");
        let payload_byte = bytes.len() / 2;
        bytes[payload_byte] ^= 1;
        std::fs::write(&vector_path, bytes).expect("corrupt vectors");

        let error = hybrid_results_with_provider(
            &params,
            &vesc_knowledge_index::LexicalFilters::default(),
            3,
            &config,
            &mut provider,
        )
        .expect_err("corrupt vector artifact");

        assert_eq!(error, "configured vector artifact unavailable");
    }

    #[test]
    fn hybrid_without_vector_artifact_recommends_lexical() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("lexical artifact");
        let config = KnowledgeConfig {
            mode: RetrievalMode::Hybrid,
            artifact_path: Some(temp.path().into()),
            semantic_model_id: Some("fake".into()),
            semantic_model_revision: Some("test".into()),
            ..KnowledgeConfig::default()
        };
        let params = SearchVescKnowledgeParams {
            query: "lbm_add_extension".into(),
            snapshot_id: None,
            limit: 3,
            mode: Some(SearchMode::Hybrid),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        };
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);

        let error = hybrid_results_with_provider(
            &params,
            &vesc_knowledge_index::LexicalFilters::default(),
            3,
            &config,
            &mut provider,
        )
        .expect_err("missing vector artifact");

        assert!(error.contains("vector artifact"), "{error}");
    }

    #[test]
    fn filtered_result_explains_filter_effects() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("artifact build");
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm_add_extension".into(),
                snapshot_id: None,
                limit: 1,
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters {
                    category: Some("firmware_api".into()),
                    revision: Some("embedded-catalog-v1".into()),
                    ..SearchVescKnowledgeFilters::default()
                },
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Lexical,
                artifact_path: Some(temp.path().into()),
                ..KnowledgeConfig::default()
            },
        );

        assert!(response.ok);
        assert_eq!(
            response.results[0]
                .explanation
                .as_ref()
                .expect("explanation")
                .filter_effects,
            vec!["category=firmware_api", "revision=embedded-catalog-v1"]
        );
    }

    #[test]
    fn response_budget_is_enforced_after_evidence_bounding() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("artifact build");
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm".into(),
                snapshot_id: None,
                limit: 10,
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: Some(1_024),
                max_context_bytes: Some(64),
                detail: SearchResponseDetail::Full,
            },
            &KnowledgeConfig {
                mode: RetrievalMode::Lexical,
                artifact_path: Some(temp.path().into()),
                max_response_bytes: 1_024,
                max_passage_bytes: 64,
                ..KnowledgeConfig::default()
            },
        );

        let bytes = serde_json::to_vec(&response).expect("response JSON");
        assert!(bytes.len() <= 1_024, "{} bytes", bytes.len());
        assert!(
            response
                .warning_codes
                .iter()
                .any(|code| code == "detail_degraded")
        );
        assert_eq!(response.detail, SearchResponseDetail::Compact);
    }
}
