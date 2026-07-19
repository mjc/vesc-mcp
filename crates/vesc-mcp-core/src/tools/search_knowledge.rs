//! `search_vesc_knowledge` — search the embedded firmware and package knowledge index.

use crate::config::{KnowledgeConfig, RetrievalMode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(feature = "semantic-fastembed")]
use std::sync::{Condvar, MutexGuard, Once};
use std::sync::{Mutex, OnceLock};
#[cfg(feature = "semantic-fastembed")]
use std::time::Duration;
use std::time::Instant;
use vesc_knowledge_index::{
    Category, ExpandedContext, FusionConfig, LexicalHit, LexicalIndex, SemanticHit, VectorArtifact,
    expand_adjacent_context, search_knowledge,
};
#[cfg(any(feature = "semantic-fastembed", test))]
use vesc_knowledge_index::{EmbeddingProvider, semantic_query_text};

use crate::{
    resources::ResourceRegistry,
    tools::knowledge_feedback::{FeedbackStore, KnowledgeCorrectionResult, search_feedback},
};
/// Retrieval backend selected for a knowledge search.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Preserve the original ranked legacy index contract.
    #[default]
    Legacy,
    /// Use Tantivy over normalized chunks.
    Lexical,
    /// Use all enabled retrieval backends; semantic retrieval is optional.
    Hybrid,
    /// Select the staged default configured by the server.
    Auto,
}

/// Amount of detail returned by the search serialization boundary.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchResponseDetail {
    /// Return bounded ranked rows; read the linked resource for full evidence.
    #[default]
    Compact,
    /// Return the compatibility response with provenance and diagnostics.
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchVescKnowledgeParams {
    /// Free-text query matched against entry names, keywords, and summaries.
    pub query: String,
    /// Optional category filter (`firmware_api`, `lispbm`, `package_build`, etc.).
    #[serde(default)]
    pub category: Option<String>,
    /// Maximum number of hits to return (default 10).
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    /// Retrieval mode. Defaults to offline `lexical`; `legacy` remains explicit
    /// compatibility mode.
    #[serde(default)]
    pub mode: Option<SearchMode>,
    /// Additive filters for lexical/hybrid retrieval.
    #[serde(default)]
    pub filters: SearchVescKnowledgeFilters,
    /// Maximum response JSON size; bounded to 64 KiB by default.
    #[serde(default)]
    pub max_response_bytes: Option<usize>,
    /// Maximum bytes retained in each returned evidence passage.
    #[serde(default)]
    pub max_context_bytes: Option<usize>,
    /// Response detail; defaults to compact progressive disclosure.
    #[serde(default)]
    #[schemars(skip)]
    pub detail: SearchResponseDetail,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchVescKnowledgeFilters {
    /// Category filter; conflicts with the legacy top-level category when both differ.
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    /// Exact immutable source revision filter.
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub trust_tier: Option<String>,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

const fn default_search_limit() -> usize {
    10
}

const COMPACT_EXCERPT_BYTES: usize = 96;
const COMPACT_FIELDS: [&str; 7] = [
    "name",
    "category",
    "excerpt",
    "source_index",
    "chunk_id",
    "correction_ids",
    "origin",
];

/// One neighboring passage is enough to complete a bounded evidence context.
const MAX_EXPANDED_NEIGHBORS: usize = 1;

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
    pub corpus_version: String,
    pub corpus_digest: Option<String>,
    pub document_count: usize,
    pub chunk_count: usize,
    pub source_count: usize,
    pub diagnostic_count: usize,
    pub component_versions: BTreeMap<String, String>,
    pub lexical_checksum: Option<String>,
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
    pub mode: SearchMode,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<SearchVescKnowledgeIndex>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<SearchVescKnowledgeTiming>,
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
    mode: SearchMode,
    fields: [&'static str; 7],
    sources: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    corrections: Vec<KnowledgeCorrectionResult>,
    results: Vec<CompactResultRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[must_use]
fn compact_response(response: &SearchVescKnowledgeResponse) -> CompactSearchResponse {
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
        let mut excerpt = result.summary.clone();
        truncate_utf8(&mut excerpt, COMPACT_EXCERPT_BYTES);
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
        mode: response.mode,
        fields: COMPACT_FIELDS,
        sources,
        corrections: response.corrections.clone(),
        results,
        error: response.error.clone(),
        warnings: response.warnings.clone(),
    }
}

fn parse_category(raw: Option<&str>) -> Result<Option<Category>, String> {
    raw.map(|name| {
        let value = serde_json::Value::String(name.to_string());
        serde_json::from_value(value).map_err(|_| format!("unsupported category {name:?}"))
    })
    .transpose()
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
    let mode = params.mode.unwrap_or_else(|| configured_mode(config));
    let started = Instant::now();
    if params.query.len() > config.max_query_bytes {
        return error_response(
            mode,
            format!("query exceeds {} bytes", config.max_query_bytes),
        );
    }
    let limit = if params.limit == 0 {
        default_search_limit()
    } else {
        params.limit
    };
    if limit > config.max_limit {
        return error_response(mode, format!("limit exceeds maximum {}", config.max_limit));
    }
    if params
        .max_response_bytes
        .is_some_and(|budget| budget == 0 || budget > config.max_response_bytes)
    {
        return error_response(
            mode,
            format!(
                "max_response_bytes must be between 1 and {}",
                config.max_response_bytes
            ),
        );
    }
    if params
        .max_context_bytes
        .is_some_and(|budget| budget == 0 || budget > config.max_passage_bytes)
    {
        return error_response(
            mode,
            format!(
                "max_context_bytes must be between 1 and {}",
                config.max_passage_bytes
            ),
        );
    }

    match parse_filters(params) {
        Ok((category, filters)) => {
            match search_mode(params, mode, category, &filters, limit, config) {
                Ok((results, warnings)) => {
                    let mut response = SearchVescKnowledgeResponse {
                        ok: true,
                        mode,
                        capabilities: capabilities_for_result(mode, &warnings),
                        corrections: Vec::new(),
                        results,
                        error: None,
                        warnings,
                        index: index_metadata(config),
                        timing: None,
                    };
                    response.timing = Some(SearchVescKnowledgeTiming {
                        total_us: elapsed_us(started),
                        result_count: response.results.len(),
                    });
                    response = response.bounded(params, config, params.detail);
                    if let Some(timing) = &mut response.timing {
                        timing.result_count = response.results.len();
                    }
                    response
                }
                Err(error) => SearchVescKnowledgeResponse {
                    ok: false,
                    mode,
                    capabilities: Vec::new(),
                    corrections: Vec::new(),
                    results: Vec::new(),
                    error: Some(error),
                    warnings: Vec::new(),
                    index: index_metadata(config),
                    timing: None,
                },
            }
        }
        Err(error) => error_response(mode, error),
    }
}

const fn configured_mode(config: &KnowledgeConfig) -> SearchMode {
    match config.mode {
        RetrievalMode::Legacy => SearchMode::Legacy,
        RetrievalMode::Lexical => SearchMode::Lexical,
        RetrievalMode::Auto => SearchMode::Auto,
        RetrievalMode::Hybrid => SearchMode::Hybrid,
    }
}

fn capabilities_for_mode(mode: SearchMode) -> Vec<String> {
    match mode {
        SearchMode::Legacy => vec!["legacy-index".into()],
        SearchMode::Lexical => vec![
            "lexical-index".into(),
            "provenance".into(),
            "knowledge-chunk-resource".into(),
            "knowledge-document-resource".into(),
        ],
        SearchMode::Auto => vec![
            "lexical-index".into(),
            "auto-fallback".into(),
            "provenance".into(),
            "knowledge-chunk-resource".into(),
            "knowledge-document-resource".into(),
        ],
        SearchMode::Hybrid => vec![
            "lexical-index".into(),
            "semantic-index".into(),
            "hybrid-fusion".into(),
            "provenance".into(),
            "knowledge-chunk-resource".into(),
            "knowledge-document-resource".into(),
        ],
    }
}

fn capabilities_for_result(mode: SearchMode, warnings: &[String]) -> Vec<String> {
    let mut capabilities = capabilities_for_mode(mode);
    if mode == SearchMode::Auto && warnings.is_empty() {
        capabilities.extend(["semantic-index".into(), "hybrid-fusion".into()]);
    }
    capabilities
}

const fn error_response(mode: SearchMode, error: String) -> SearchVescKnowledgeResponse {
    SearchVescKnowledgeResponse {
        ok: false,
        mode,
        capabilities: Vec::new(),
        corrections: Vec::new(),
        results: Vec::new(),
        error: Some(error),
        warnings: Vec::new(),
        index: None,
        timing: None,
    }
}

impl SearchVescKnowledgeResponse {
    fn bounded(
        mut self,
        params: &SearchVescKnowledgeParams,
        config: &KnowledgeConfig,
        detail: SearchResponseDetail,
    ) -> Self {
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
        if detail == SearchResponseDetail::Compact || response_exceeds_budget(&self, budget, detail)
        {
            for correction in &mut self.corrections {
                compact_correction(correction);
            }
        }
        while response_exceeds_budget(&self, budget, detail) && self.results.len() > 1 {
            self.results.pop();
        }
        if response_exceeds_budget(&self, budget, detail) {
            for result in &mut self.results {
                compact_result(result);
            }
        }
        while response_exceeds_budget(&self, budget, detail) && self.results.len() > 1 {
            self.results.pop();
        }
        if response_exceeds_budget(&self, budget, detail) {
            self.results.clear();
            self.index = None;
        }
        while response_exceeds_budget(&self, budget, detail) && self.corrections.len() > 1 {
            self.corrections.pop();
        }
        if response_exceeds_budget(&self, budget, detail) {
            self.corrections.clear();
        }
        if response_exceeds_budget(&self, budget, detail) {
            self.warnings
                .push("response budget is smaller than the fixed response envelope".into());
        }
        if let Some(timing) = &mut self.timing {
            timing.result_count = self.results.len();
        }
        self
    }
}

fn response_exceeds_budget(
    response: &SearchVescKnowledgeResponse,
    budget: usize,
    detail: SearchResponseDetail,
) -> bool {
    match detail {
        SearchResponseDetail::Compact => serde_json::to_vec(&compact_response(response))
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

fn index_metadata(config: &KnowledgeConfig) -> Option<SearchVescKnowledgeIndex> {
    if let Some(root) = &config.artifact_path {
        if root.is_file() {
            return None;
        }
        if let Ok(manifest) = vesc_knowledge_index::inspect_manifest(
            &vesc_knowledge_index::active_manifest_path(root),
        ) {
            return Some(SearchVescKnowledgeIndex {
                corpus_version: manifest.corpus.corpus_version.to_string(),
                corpus_digest: Some(manifest.corpus.content_digest.to_string()),
                document_count: manifest.corpus.documents.len(),
                chunk_count: manifest.corpus.chunks.len(),
                source_count: manifest.sources.len(),
                diagnostic_count: manifest.diagnostics.len(),
                component_versions: manifest.component_versions,
                lexical_checksum: manifest.lexical_checksum.map(|digest| digest.to_string()),
            });
        }
    }
    let count = vesc_knowledge_index::embedded_entries().len();
    Some(SearchVescKnowledgeIndex {
        corpus_version: "embedded-legacy-v1".into(),
        corpus_digest: None,
        document_count: count,
        chunk_count: count,
        source_count: 0,
        diagnostic_count: 0,
        component_versions: BTreeMap::new(),
        lexical_checksum: None,
    })
}

fn parse_filters(
    params: &SearchVescKnowledgeParams,
) -> Result<(Option<Category>, vesc_knowledge_index::LexicalFilters), String> {
    let category = parse_category(params.category.as_deref())?;
    let filter_category = parse_category(params.filters.category.as_deref())?;
    if category.is_some() && filter_category.is_some() && category != filter_category {
        return Err("category and filters.category conflict".into());
    }
    let category = category.or(filter_category);
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
    Ok((
        category,
        vesc_knowledge_index::LexicalFilters {
            category,
            repository,
            revision,
            source_kind,
            trust_tier,
            tags,
        },
    ))
}

fn search_mode(
    params: &SearchVescKnowledgeParams,
    mode: SearchMode,
    category: Option<Category>,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<(Vec<SearchVescKnowledgeResult>, Vec<String>), String> {
    match mode {
        SearchMode::Legacy => Ok((
            search_knowledge(&params.query, category, limit)
                .into_iter()
                .map(legacy_result)
                .collect(),
            Vec::new(),
        )),
        SearchMode::Lexical => Ok((
                lexical_results(&params.query, filters, limit, config)?
                    .into_iter()
                    .enumerate()
                    .map(|(rank, hit)| lexical_result(hit, rank, filters))
                .collect(),
            Vec::new(),
        )),
        SearchMode::Auto => match hybrid_results(params, filters, limit, config) {
            Ok(results) => Ok((results, Vec::new())),
            Err(semantic_error) => match lexical_results(&params.query, filters, limit, config) {
                Ok(results) => Ok((
                    results
                        .into_iter()
                        .enumerate()
                        .map(|(rank, hit)| lexical_result(hit, rank, filters))
                        .collect(),
                    vec![format!(
                        "semantic retrieval unavailable; lexical results returned: {semantic_error}"
                    )],
                )),
                Err(_) => Ok((
                    embedded_lexical_results(&params.query, filters, limit)?,
                    vec![
                        "configured lexical artifact unavailable; embedded lexical results returned"
                            .into(),
                        format!(
                            "semantic retrieval unavailable; lexical results returned: {semantic_error}"
                        ),
                    ],
                )),
            },
        },
        SearchMode::Hybrid => Ok((
            hybrid_results(params, filters, limit, config)?,
            Vec::new(),
        )),
    }
}

fn lexical_results(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<Vec<LexicalHit>, String> {
    if let Some(path) = &config.artifact_path {
        let lexical_path = active_lexical_path(path)?;
        return with_cached_lexical_index(&lexical_path, |index| {
            index
                .search(query, filters, limit)
                .map_err(|error| error.to_string())
        });
    }
    vesc_knowledge_index::lexical_index()
        .search(query, filters, limit)
        .map_err(|error| error.to_string())
}

fn hybrid_results(
    params: &SearchVescKnowledgeParams,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<Vec<SearchVescKnowledgeResult>, String> {
    let candidate_limit = limit.saturating_mul(5).clamp(20, 100);
    let (lexical, chunks) =
        lexical_hits_and_chunks(&params.query, filters, candidate_limit, config)?;
    let semantic = semantic_hits(&params.query, filters, candidate_limit, config, &chunks)?;
    let context_budget = params
        .max_context_bytes
        .unwrap_or(config.max_passage_bytes)
        .min(config.max_passage_bytes);
    Ok(vesc_knowledge_index::fuse_candidates(
        &lexical,
        &semantic,
        &chunks,
        FusionConfig {
            limit,
            lexical_floor: true,
            ..FusionConfig::default()
        },
    )
    .into_iter()
    .map(|hit| {
        let context =
            expand_adjacent_context(&hit.chunk, &chunks, MAX_EXPANDED_NEIGHBORS, context_budget);
        fused_result(hit, &context, filters)
    })
    .collect())
}

#[cfg(test)]
fn hybrid_results_with_provider<P: EmbeddingProvider + ?Sized>(
    params: &SearchVescKnowledgeParams,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
    provider: &mut P,
) -> Result<Vec<SearchVescKnowledgeResult>, String> {
    let candidate_limit = limit.saturating_mul(5).clamp(20, 100);
    let (lexical, chunks) =
        lexical_hits_and_chunks(&params.query, filters, candidate_limit, config)?;
    let vector = load_vector_artifact(config, &chunks)?;
    let semantic = semantic_hits_with_provider(
        &params.query,
        filters,
        candidate_limit,
        &vector,
        &chunks,
        provider,
    )?;
    let context_budget = params
        .max_context_bytes
        .unwrap_or(config.max_passage_bytes)
        .min(config.max_passage_bytes);
    Ok(vesc_knowledge_index::fuse_candidates(
        &lexical,
        &semantic,
        &chunks,
        FusionConfig {
            limit,
            lexical_floor: true,
            ..FusionConfig::default()
        },
    )
    .into_iter()
    .map(|hit| {
        let context =
            expand_adjacent_context(&hit.chunk, &chunks, MAX_EXPANDED_NEIGHBORS, context_budget);
        fused_result(hit, &context, filters)
    })
    .collect())
}

fn lexical_hits_and_chunks(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
) -> Result<
    (
        Vec<LexicalHit>,
        BTreeMap<vesc_knowledge_index::ChunkId, vesc_knowledge_index::Chunk>,
    ),
    String,
> {
    if let Some(path) = &config.artifact_path {
        let lexical_path = active_lexical_path(path)?;
        return with_cached_lexical_index(&lexical_path, |index| {
            let hits = index
                .search(query, filters, limit)
                .map_err(|error| error.to_string())?;
            Ok((hits, index.chunks().clone()))
        });
    }
    let index = vesc_knowledge_index::lexical_index();
    let hits = index
        .search(query, filters, limit)
        .map_err(|error| error.to_string())?;
    Ok((hits, index.chunks().clone()))
}

static LEXICAL_ARTIFACT_CACHE: OnceLock<Mutex<Option<CachedLexicalArtifact>>> = OnceLock::new();

struct CachedLexicalArtifact {
    key: PathBuf,
    index: LexicalIndex,
}

/// Reuse the active generation's Tantivy index between MCP requests.
///
/// ponytail: one global lock is enough for the small embedded corpus; split
/// per-generation caches only if measured concurrent throughput requires it.
#[allow(clippy::significant_drop_tightening)]
fn with_cached_lexical_index<T>(
    path: &Path,
    operation: impl FnOnce(&LexicalIndex) -> Result<T, String>,
) -> Result<T, String> {
    let cache = LEXICAL_ARTIFACT_CACHE.get_or_init(|| Mutex::new(None));
    let mut cache = cache
        .lock()
        .map_err(|_| "lexical artifact cache is poisoned".to_string())?;
    if cache.as_ref().is_none_or(|entry| entry.key != path) {
        let index = LexicalIndex::open_artifact(path)
            .map_err(|_| "configured lexical artifact unavailable".to_string())?;
        *cache = Some(CachedLexicalArtifact {
            key: path.to_owned(),
            index,
        });
    }
    let entry = cache
        .as_ref()
        .ok_or_else(|| "lexical artifact cache is empty".to_string())?;
    operation(&entry.index)
}

#[allow(clippy::significant_drop_tightening)]
fn semantic_hits(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    config: &KnowledgeConfig,
    chunks: &BTreeMap<vesc_knowledge_index::ChunkId, vesc_knowledge_index::Chunk>,
) -> Result<Vec<SemanticHit>, String> {
    let vector = load_vector_artifact(config, chunks)?;

    #[cfg(feature = "semantic-fastembed")]
    {
        let mut state = initialize_semantic_model(config)?;
        let entry = state
            .entry
            .as_mut()
            .ok_or_else(|| "semantic provider cache is empty".to_string())?;
        let result = semantic_hits_with_provider(
            query,
            filters,
            limit,
            &vector,
            chunks,
            &mut entry.provider,
        );
        entry.last_used = Instant::now();
        semantic_model_cache().wake.notify_one();
        result
    }

    #[cfg(not(feature = "semantic-fastembed"))]
    {
        let _ = (query, filters, limit, vector);
        Err("semantic-fastembed feature is disabled".into())
    }
}

#[cfg(feature = "semantic-fastembed")]
static SEMANTIC_PROVIDER: OnceLock<SemanticModelCache> = OnceLock::new();

#[cfg(feature = "semantic-fastembed")]
static SEMANTIC_REAPER: Once = Once::new();

#[cfg(feature = "semantic-fastembed")]
struct SemanticModelCache {
    state: Mutex<SemanticModelState>,
    wake: Condvar,
}

#[cfg(feature = "semantic-fastembed")]
#[derive(Default)]
struct SemanticModelState {
    entry: Option<CachedSemanticProvider>,
}

#[cfg(feature = "semantic-fastembed")]
struct CachedSemanticProvider {
    key: String,
    provider: vesc_knowledge_index::FastEmbedProvider,
    last_used: Instant,
    idle_timeout: Duration,
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_model_cache() -> &'static SemanticModelCache {
    let cache = SEMANTIC_PROVIDER.get_or_init(|| SemanticModelCache {
        state: Mutex::new(SemanticModelState::default()),
        wake: Condvar::new(),
    });
    SEMANTIC_REAPER.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("vesc-semantic-model-reaper".into())
            .spawn(reap_idle_semantic_model);
    });
    cache
}

#[cfg(feature = "semantic-fastembed")]
fn initialize_semantic_model(
    config: &KnowledgeConfig,
) -> Result<MutexGuard<'static, SemanticModelState>, String> {
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
    let key = format!("{}\0{}\0{}", model_dir.display(), model_id, model_revision);
    let cache = semantic_model_cache();
    let mut state = cache
        .state
        .lock()
        .map_err(|_| "semantic provider cache is poisoned".to_string())?;
    if state.entry.as_ref().is_none_or(|entry| entry.key != key) {
        let profile = vesc_knowledge_index::EmbeddingProfile::for_model_id(model_id)
            .ok_or_else(|| format!("no embedding profile is registered for {model_id}"))?;
        let provider =
            vesc_knowledge_index::FastEmbedProvider::from_model_dir_with_profile_and_threads(
                model_dir,
                None,
                profile,
                Some(vesc_knowledge_index::default_semantic_intra_threads()),
            )
            .map_err(|error| format!("semantic provider unavailable: {error}"))?;
        state.entry = Some(CachedSemanticProvider {
            key,
            provider,
            last_used: Instant::now(),
            idle_timeout: Duration::from_secs(config.semantic_idle_timeout_secs),
        });
    }
    if let Some(entry) = state.entry.as_mut() {
        entry.last_used = Instant::now();
        entry.idle_timeout = Duration::from_secs(config.semantic_idle_timeout_secs);
    }
    cache.wake.notify_one();
    Ok(state)
}

#[cfg(feature = "semantic-fastembed")]
fn reap_idle_semantic_model() {
    let cache = SEMANTIC_PROVIDER
        .get()
        .expect("semantic cache initialized before reaper");
    let mut state = cache
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        let Some(entry) = state.entry.as_ref() else {
            state = cache
                .wake
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            continue;
        };
        let remaining = entry.idle_timeout.saturating_sub(entry.last_used.elapsed());
        if remaining.is_zero() {
            state.entry = None;
            continue;
        }
        let (next, _) = cache
            .wake
            .wait_timeout(state, remaining)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state = next;
    }
}

fn load_vector_artifact(
    config: &KnowledgeConfig,
    chunks: &BTreeMap<vesc_knowledge_index::ChunkId, vesc_knowledge_index::Chunk>,
) -> Result<VectorArtifact, String> {
    let root = config
        .artifact_path
        .as_deref()
        .ok_or_else(|| "vector artifact is not configured".to_string())?;
    let manifest =
        vesc_knowledge_index::inspect_manifest(&vesc_knowledge_index::active_manifest_path(root))
            .map_err(|_| "configured vector artifact unavailable".to_string())?;
    let vector_path = root
        .join("generations")
        .join(manifest.corpus.content_digest.to_string())
        .join("vectors.bin");
    let vector = VectorArtifact::open_artifact(&vector_path)
        .map_err(|_| "configured vector artifact unavailable".to_string())?;
    let model_id = config
        .semantic_model_id
        .as_deref()
        .ok_or_else(|| "semantic model identity is not configured".to_string())?;
    let model_revision = config
        .semantic_model_revision
        .as_deref()
        .ok_or_else(|| "semantic model revision is not configured".to_string())?;
    let chunk_ids = chunks.keys().cloned().collect::<BTreeSet<_>>();
    vector
        .validate_for_corpus(
            &manifest.corpus.content_digest,
            &chunk_ids,
            model_id,
            model_revision,
        )
        .map_err(|error| format!("semantic artifact incompatible: {error}"))?;
    Ok(vector)
}

#[cfg(any(feature = "semantic-fastembed", test))]
fn semantic_hits_with_provider<P: EmbeddingProvider + ?Sized>(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
    vector: &VectorArtifact,
    chunks: &BTreeMap<vesc_knowledge_index::ChunkId, vesc_knowledge_index::Chunk>,
    provider: &mut P,
) -> Result<Vec<SemanticHit>, String> {
    let query = provider
        .embed_query(&semantic_query_text(query))
        .map_err(|error| format!("query embedding failed: {error}"))?;
    vector
        .search(&query, limit)
        .map(|hits| {
            hits.into_iter()
                .filter(|hit| {
                    chunks
                        .get(&hit.chunk_id)
                        .is_some_and(|chunk| filters.matches(chunk))
                })
                .collect()
        })
        .map_err(|error| format!("semantic search failed: {error}"))
}

fn fused_result(
    hit: vesc_knowledge_index::FusedHit,
    context: &ExpandedContext,
    filters: &vesc_knowledge_index::LexicalFilters,
) -> SearchVescKnowledgeResult {
    let chunk = hit.chunk;
    let id = chunk
        .legacy_ids
        .first()
        .cloned()
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
        summary: passage.clone(),
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
    }
}

fn embedded_lexical_results(
    query: &str,
    filters: &vesc_knowledge_index::LexicalFilters,
    limit: usize,
) -> Result<Vec<SearchVescKnowledgeResult>, String> {
    vesc_knowledge_index::lexical_index()
        .search(query, filters, limit)
        .map_err(|error| error.to_string())
        .map(|results| {
            results
                .into_iter()
                .enumerate()
                .map(|(rank, hit)| lexical_result(hit, rank, filters))
                .collect()
        })
}

fn active_lexical_path(root: &Path) -> Result<std::path::PathBuf, String> {
    if root.is_file() {
        return Ok(root.to_owned());
    }
    let manifest =
        vesc_knowledge_index::inspect_manifest(&vesc_knowledge_index::active_manifest_path(root))
            .map_err(|_| "configured lexical artifact unavailable".to_string())?;
    Ok(root
        .join("generations")
        .join(manifest.corpus.content_digest.to_string())
        .join("lexical.json"))
}

fn legacy_result(hit: vesc_knowledge_index::KnowledgeSearchHit) -> SearchVescKnowledgeResult {
    SearchVescKnowledgeResult {
        id: hit.id,
        name: hit.name,
        category: category_label(hit.category).into(),
        summary: hit.summary,
        source: SearchVescKnowledgeSource {
            repo: hit.source.repo,
            path: hit.source.path,
            line: hit.source.line,
            end_line: None,
            start_byte: None,
            end_byte: None,
            revision: None,
        },
        score: hit.score,
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
        explanation: None,
    }
}

fn lexical_result(
    hit: vesc_knowledge_index::LexicalHit,
    rank: usize,
    filters: &vesc_knowledge_index::LexicalFilters,
) -> SearchVescKnowledgeResult {
    let chunk = hit.chunk;
    let name = chunk.title.clone();
    let id = chunk
        .legacy_ids
        .first()
        .cloned()
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
    serialize_search_response(&response, params.detail)
}

/// Serialize a search response using the resolved server configuration.
#[must_use]
pub fn search_vesc_knowledge_json_with_config(
    params: &SearchVescKnowledgeParams,
    config: &KnowledgeConfig,
) -> String {
    let response = search_vesc_knowledge_tool_with_config(params, config);
    serialize_search_response(&response, params.detail)
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
    if response.ok {
        if let Some(store) = feedback {
            let limit = if params.limit == 0 {
                default_search_limit()
            } else {
                params.limit
            };
            let feedback = parse_filters(params)
                .map_err(|error| format!("feedback filters unavailable: {error}"))
                .and_then(|(_, filters)| {
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
    }
    serialize_search_response(&response, params.detail)
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
        category: None,
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
) -> String {
    match detail {
        SearchResponseDetail::Compact => serde_json::to_string(&compact_response(response)),
        SearchResponseDetail::Full => serde_json::to_string(response),
    }
    .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_category_returns_error_response() {
        let resp = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "nvm".into(),
            category: Some("not_a_category".into()),
            limit: 10,
            mode: Some(SearchMode::Legacy),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        });
        assert!(!resp.ok);
        assert!(resp.error.is_some());
        assert!(resp.results.is_empty());
    }

    #[test]
    fn zero_limit_uses_default() {
        let resp = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "pkg".into(),
            category: None,
            limit: 0,
            mode: Some(SearchMode::Legacy),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        });
        assert!(resp.ok);
        assert!(!resp.results.is_empty());
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
            artifact_path: None,
            semantic_model_dir: None,
            semantic_model_id: None,
            semantic_model_revision: None,
            semantic_idle_timeout_secs: 300,
            max_limit: 1,
            max_query_bytes: 32,
            max_response_bytes: 1024,
            max_passage_bytes: 128,
        };
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "nvm".into(),
                category: None,
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
    }

    #[test]
    fn explicit_hybrid_without_semantics_returns_structured_error() {
        let response = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "nvm".into(),
            category: None,
            limit: 1,
            mode: Some(SearchMode::Hybrid),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        });

        assert!(!response.ok);
        assert_eq!(response.mode, SearchMode::Hybrid);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("vector artifact"))
        );
    }

    #[test]
    fn auto_semantic_failure_returns_lexical_warning() {
        let response = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "nvm".into(),
            category: None,
            limit: 1,
            mode: Some(SearchMode::Auto),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        });

        assert!(response.ok);
        assert!(!response.results.is_empty());
        assert!(
            response
                .warnings
                .iter()
                .any(|warning| { warning.contains("semantic retrieval unavailable") })
        );
    }

    #[test]
    fn configured_artifact_is_loaded_for_lexical_search() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("artifact build");
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm_add_extension".into(),
                category: None,
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
        assert_eq!(response.results[0].id, "vesc_c_if.lbm_add_extension");
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
            category: None,
            limit: 3,
            mode: Some(SearchMode::Hybrid),
            filters: SearchVescKnowledgeFilters::default(),
            max_response_bytes: None,
            max_context_bytes: None,
            detail: SearchResponseDetail::Full,
        };
        let mut query_provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        let filters = vesc_knowledge_index::LexicalFilters::default();
        let results =
            hybrid_results_with_provider(&params, &filters, 3, &config, &mut query_provider)
                .expect("hybrid results");

        assert!(!results.is_empty());
        assert!(results.iter().any(|result| {
            result
                .explanation
                .as_ref()
                .is_some_and(|explanation| explanation.semantic_rank.is_some())
        }));
    }

    #[test]
    fn filtered_result_explains_filter_effects() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("artifact build");
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm_add_extension".into(),
                category: None,
                limit: 1,
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters {
                    category: Some("firmware_api".into()),
                    revision: Some("legacy".into()),
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
            vec!["category=firmware_api", "revision=legacy"]
        );
    }

    #[test]
    fn response_budget_is_enforced_after_evidence_bounding() {
        let temp = tempfile::tempdir().expect("tempdir");
        vesc_knowledge_index::build_embedded_artifacts(temp.path()).expect("artifact build");
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: "lbm".into(),
                category: None,
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
    }
}
