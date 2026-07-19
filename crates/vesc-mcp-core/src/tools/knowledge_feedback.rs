//! Durable learned notes and VESC-resource-backed corrections.

use crate::resources::ResourceRegistry;
use fs4::fs_std::FileExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;
use vesc_knowledge_index::{
    Chunk, LexicalFilters, LexicalIndex, LicenseStatus, NormalizedDocument, RepositoryId,
    ResourceUri, Revision, SourceKind, TrustTier,
};

const STORE_SCHEMA: u32 = 1;
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_MAX_RECORDS: usize = 1_000;
const MAX_QUESTION_BYTES: usize = 4 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024;
const MAX_LIST_ITEMS: usize = 32;
const MAX_ITEM_BYTES: usize = 512;
const MAX_EVIDENCE_ITEMS: usize = 16;
const EVIDENCE_EXCERPT_CHARS: usize = 512;

/// Bounded learned-note submission.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SubmitKnowledgeFeedbackParams {
    /// Original VESC question whose resolution produced the lesson.
    pub question: String,
    /// Concise reusable lesson; never include the full conversation.
    pub lesson: String,
    /// Paraphrases that should retrieve the lesson later.
    #[serde(default)]
    pub related_queries: Vec<String>,
    /// Exact technical identifiers, symbols, or commands.
    #[serde(default)]
    pub identifiers: Vec<String>,
    /// Short topical tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Informational references only; these are never fetched or read.
    #[serde(default)]
    pub source_references: Vec<String>,
    /// Older feedback or correction made inactive by this record.
    #[serde(default)]
    pub supersedes: Option<String>,
}

/// How the user authorized a durable correction write.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionAuthorization {
    /// The user explicitly directed the model to record the correction.
    ExplicitUserRequest,
    /// The model asked whether to record the correction and the user confirmed.
    ConfirmedAfterPrompt,
}

/// Why the knowledge response failed to steer the model correctly.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum GapDiagnosis {
    MissingAuthoritativeSource,
    IndexedButNotRetrieved,
    DecisiveEvidenceBelowCutoff,
    ContextDilution,
    ResponseBudgetTruncation,
    ChunkFragmentation,
    MisleadingSummaryOrTitle,
    RetrievedButNotSalient,
    MisinterpretedRelationship,
    MissingProjectDecision,
    ConflictingOrStaleData,
    MissingRegressionEvaluation,
}

/// Bounded follow-up work suggested by a diagnosed knowledge gap.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedDataAction {
    IngestAuthoritativeSource,
    ImproveQueryExpansion,
    PromoteDecisiveEvidence,
    ReduceContextDilution,
    PreserveDecisiveExcerpt,
    RepairChunkBoundaries,
    ImproveRetrievalText,
    EmphasizeQualifier,
    LinkRelatedEvidence,
    AddProjectDecision,
    ResolveSourceConflict,
    AddRegressionEvaluation,
}

impl From<GapDiagnosis> for RecommendedDataAction {
    fn from(diagnosis: GapDiagnosis) -> Self {
        match diagnosis {
            GapDiagnosis::MissingAuthoritativeSource => Self::IngestAuthoritativeSource,
            GapDiagnosis::IndexedButNotRetrieved => Self::ImproveQueryExpansion,
            GapDiagnosis::DecisiveEvidenceBelowCutoff => Self::PromoteDecisiveEvidence,
            GapDiagnosis::ContextDilution => Self::ReduceContextDilution,
            GapDiagnosis::ResponseBudgetTruncation => Self::PreserveDecisiveExcerpt,
            GapDiagnosis::ChunkFragmentation => Self::RepairChunkBoundaries,
            GapDiagnosis::MisleadingSummaryOrTitle => Self::ImproveRetrievalText,
            GapDiagnosis::RetrievedButNotSalient => Self::EmphasizeQualifier,
            GapDiagnosis::MisinterpretedRelationship => Self::LinkRelatedEvidence,
            GapDiagnosis::MissingProjectDecision => Self::AddProjectDecision,
            GapDiagnosis::ConflictingOrStaleData => Self::ResolveSourceConflict,
            GapDiagnosis::MissingRegressionEvaluation => Self::AddRegressionEvaluation,
        }
    }
}

/// One result returned by the original failed knowledge search.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RetrievalTraceResult {
    /// Result, chunk, document, or resource identifier.
    pub id: String,
    /// Original rendered score, retained as bounded text to avoid float identity drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<String>,
    /// Bounded excerpt that influenced the model.
    pub excerpt: String,
    /// Readable resource URI when one was returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
}

/// Reproducible bounded snapshot of the search response that led to the mistake.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RetrievalTrace {
    /// Original search query.
    pub query: String,
    /// Retrieval mode used by the failed search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Requested result limit.
    pub limit: usize,
    /// Requested response byte budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_response_bytes: Option<usize>,
    /// Requested context byte budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_bytes: Option<usize>,
    /// Bounded key=value filters used by the search.
    #[serde(default)]
    pub filters: Vec<String>,
    /// Ordered results as originally returned.
    #[serde(default)]
    pub results: Vec<RetrievalTraceResult>,
    /// Evidence that should have appeared in the bounded top context.
    pub decisive_evidence: Vec<String>,
    /// Returned IDs or facts that diluted or encouraged the wrong inference.
    #[serde(default)]
    pub distractors: Vec<String>,
    /// Targeted next reads/searches when the evidence was insufficient.
    #[serde(default)]
    pub insufficient_evidence_next: Vec<String>,
}

/// Evidence-backed correction submission.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CorrectVescKnowledgeParams {
    /// Original VESC question or bounded technical context.
    pub question: String,
    /// Proof that the user authorized this durable correction write.
    pub authorization: CorrectionAuthorization,
    /// Earlier MCP-derived conclusion that was wrong or incomplete.
    pub mistaken_conclusion: String,
    /// Corrected fact supported by the supplied VESC resources.
    pub correction: String,
    /// Why the earlier inference or analogy failed.
    pub reasoning_failure: String,
    /// Structured causes of the failed knowledge response.
    pub gap_diagnoses: Vec<GapDiagnosis>,
    /// Original bounded search response used to replay the retrieval failure.
    pub retrieval_trace: RetrievalTrace,
    /// Important limitations or conditions on the corrected fact.
    #[serde(default)]
    pub qualifiers: Vec<String>,
    /// Result IDs or resource URIs affected by the correction.
    #[serde(default)]
    pub affected_resources: Vec<String>,
    /// Registered VESC catalog or knowledge resource URIs supporting the correction.
    pub evidence_resources: Vec<String>,
    /// Bounded project decision, test, or commit references for human curation.
    #[serde(default)]
    pub project_references: Vec<String>,
    /// Paraphrases that should retrieve the correction later.
    #[serde(default)]
    pub related_queries: Vec<String>,
    /// Exact technical identifiers, symbols, or commands.
    #[serde(default)]
    pub identifiers: Vec<String>,
    /// Short topical tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Older feedback or correction made inactive by this record.
    #[serde(default)]
    pub supersedes: Option<String>,
}

/// Resolved evidence retained with a correction.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ResourceEvidence {
    pub uri: String,
    pub content_digest: String,
    pub excerpt: String,
}

/// Shared JSON response for feedback writes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct FeedbackWriteResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub duplicate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ResourceEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A low-trust reusable lesson.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearnedNote {
    pub id: String,
    pub question: String,
    pub lesson: String,
    pub related_queries: Vec<String>,
    pub identifiers: Vec<String>,
    pub tags: Vec<String>,
    pub source_references: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// A correction grounded in registered VESC resources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeCorrection {
    pub id: String,
    pub question: String,
    pub authorization: CorrectionAuthorization,
    pub mistaken_conclusion: String,
    pub correction: String,
    #[serde(default)]
    pub reasoning_failure: String,
    #[serde(default)]
    pub gap_diagnoses: Vec<GapDiagnosis>,
    #[serde(default)]
    pub recommended_data_actions: Vec<RecommendedDataAction>,
    #[serde(default)]
    pub retrieval_trace: RetrievalTrace,
    pub qualifiers: Vec<String>,
    pub affected_resources: Vec<String>,
    pub evidence: Vec<ResourceEvidence>,
    #[serde(default)]
    pub project_references: Vec<String>,
    pub related_queries: Vec<String>,
    pub identifiers: Vec<String>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub covered_by_base_knowledge: bool,
    #[serde(default)]
    pub coverage_evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// Durable knowledge record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KnowledgeRecord {
    LearnedNote(Box<LearnedNote>),
    Correction(Box<KnowledgeCorrection>),
}

impl KnowledgeRecord {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::LearnedNote(note) => &note.id,
            Self::Correction(correction) => &correction.id,
        }
    }

    fn superseded_by(&self) -> Option<&str> {
        match self {
            Self::LearnedNote(note) => note.superseded_by.as_deref(),
            Self::Correction(correction) => correction.superseded_by.as_deref(),
        }
    }

    fn set_superseded_by(&mut self, id: String) {
        match self {
            Self::LearnedNote(note) => note.superseded_by = Some(id),
            Self::Correction(correction) => correction.superseded_by = Some(id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FeedbackSnapshot {
    schema: u32,
    records: Vec<KnowledgeRecord>,
}

impl Default for FeedbackSnapshot {
    fn default() -> Self {
        Self {
            schema: STORE_SCHEMA,
            records: Vec::new(),
        }
    }
}

/// Persistent feedback store rooted at an explicitly configured directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackStore {
    root: PathBuf,
    max_records: usize,
}

impl FeedbackStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_records: DEFAULT_MAX_RECORDS,
        }
    }

    #[must_use]
    pub fn with_max_records(mut self, max_records: usize) -> Self {
        self.max_records = max_records.max(1);
        self
    }

    /// Return a record by stable ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read or validated.
    pub fn get(&self, id: &str) -> Result<Option<KnowledgeRecord>, FeedbackError> {
        Ok(self
            .load()?
            .records
            .into_iter()
            .find(|record| record.id() == id))
    }

    /// Return active records in deterministic ID order.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read or validated.
    pub fn active_records(&self) -> Result<Vec<KnowledgeRecord>, FeedbackError> {
        Ok(self
            .load()?
            .records
            .into_iter()
            .filter(|record| {
                record.superseded_by().is_none()
                    && !matches!(
                        record,
                        KnowledgeRecord::Correction(correction)
                            if correction.covered_by_base_knowledge
                    )
            })
            .collect())
    }

    /// Mark a correction covered after a base-only replay finds all decisive evidence.
    ///
    /// # Errors
    ///
    /// Returns an error for empty evidence, an unknown/non-correction ID, or a
    /// store read/write failure.
    pub fn mark_correction_covered(
        &self,
        id: &str,
        coverage_evidence: &[String],
    ) -> Result<(), FeedbackError> {
        if coverage_evidence.is_empty() {
            return Err(FeedbackError::Invalid(
                "coverage_evidence must not be empty".into(),
            ));
        }
        validate_lists(&[("coverage_evidence", coverage_evidence)])?;
        let _guard = self.lock_for_write()?;
        let mut snapshot = self.load()?;
        let record = snapshot
            .records
            .iter_mut()
            .find(|record| record.id() == id)
            .ok_or_else(|| FeedbackError::Invalid(format!("unknown correction {id}")))?;
        let KnowledgeRecord::Correction(correction) = record else {
            return Err(FeedbackError::Invalid(format!(
                "record {id} is not a correction"
            )));
        };
        correction.covered_by_base_knowledge = true;
        correction.coverage_evidence = normalized_list(coverage_evidence);
        self.save(&snapshot)
    }

    fn insert(
        &self,
        record: KnowledgeRecord,
        supersedes: Option<&str>,
    ) -> Result<bool, FeedbackError> {
        let _guard = self.lock_for_write()?;
        let mut snapshot = self.load()?;
        if snapshot
            .records
            .iter()
            .any(|existing| existing.id() == record.id())
        {
            return Ok(true);
        }
        if snapshot.records.len() >= self.max_records {
            return Err(FeedbackError::Limit(format!(
                "feedback store contains the maximum {} records",
                self.max_records
            )));
        }
        if let Some(old_id) = supersedes {
            if old_id == record.id() {
                return Err(FeedbackError::Invalid(
                    "record cannot supersede itself".into(),
                ));
            }
            let old = snapshot
                .records
                .iter_mut()
                .find(|existing| existing.id() == old_id)
                .ok_or_else(|| {
                    FeedbackError::Invalid(format!("unknown superseded record {old_id}"))
                })?;
            if old.superseded_by().is_some() {
                return Err(FeedbackError::Invalid(format!(
                    "record {old_id} is already superseded"
                )));
            }
            old.set_superseded_by(record.id().to_owned());
        }
        snapshot.records.push(record);
        snapshot
            .records
            .sort_by(|left, right| left.id().cmp(right.id()));
        self.save(&snapshot)?;
        Ok(false)
    }

    fn path(&self) -> PathBuf {
        self.root.join("feedback.json")
    }

    fn lock_for_write(&self) -> Result<File, FeedbackError> {
        reject_symlink(&self.root)?;
        fs::create_dir_all(&self.root)?;
        reject_symlink(&self.root)?;
        let path = self.root.join(".feedback.lock");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(FeedbackError::Invalid(format!(
                    "feedback lock {} must be a regular file",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        FileExt::lock_exclusive(&lock)?;
        Ok(lock)
    }

    fn load(&self) -> Result<FeedbackSnapshot, FeedbackError> {
        reject_symlink(&self.root)?;
        let path = self.path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(FeedbackError::Invalid(format!(
                        "feedback store {} must be a regular file",
                        path.display()
                    )));
                }
                if metadata.len() > MAX_STORE_BYTES {
                    return Err(FeedbackError::Limit(format!(
                        "feedback store exceeds {MAX_STORE_BYTES} bytes"
                    )));
                }
                let bytes = fs::read(&path)?;
                let snapshot: FeedbackSnapshot = serde_json::from_slice(&bytes)?;
                if snapshot.schema != STORE_SCHEMA {
                    return Err(FeedbackError::Invalid(format!(
                        "unsupported feedback schema {}",
                        snapshot.schema
                    )));
                }
                if snapshot.records.len() > self.max_records {
                    return Err(FeedbackError::Limit(format!(
                        "feedback store exceeds {} records",
                        self.max_records
                    )));
                }
                Ok(snapshot)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(FeedbackSnapshot::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, snapshot: &FeedbackSnapshot) -> Result<(), FeedbackError> {
        reject_symlink(&self.root)?;
        fs::create_dir_all(&self.root)?;
        reject_symlink(&self.root)?;
        let bytes = serde_json::to_vec_pretty(snapshot)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STORE_BYTES {
            return Err(FeedbackError::Limit(format!(
                "feedback store exceeds {MAX_STORE_BYTES} bytes"
            )));
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(self.path())
            .map_err(|error| FeedbackError::Io(error.error))?;
        Ok(())
    }
}

/// Feedback persistence and validation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FeedbackError {
    #[error("feedback I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("feedback JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid feedback: {0}")]
    Invalid(String),
    #[error("feedback limit exceeded: {0}")]
    Limit(String),
    #[error("evidence resource unavailable: {0}")]
    Evidence(String),
    #[error("feedback index failed: {0}")]
    Index(String),
}

/// Ranked feedback matches used to augment normal knowledge search.
#[derive(Debug, Clone)]
pub struct FeedbackMatches {
    pub notes: Vec<FeedbackNoteMatch>,
    pub corrections: Vec<KnowledgeCorrectionResult>,
}

/// Ranked learned-note match.
#[derive(Debug, Clone)]
pub struct FeedbackNoteMatch {
    pub note: LearnedNote,
    pub score: f32,
}

/// Correction rendered as a learned advisory before ordinary search passages.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct KnowledgeCorrectionResult {
    pub id: String,
    pub state: String,
    pub question: String,
    pub what_we_know: String,
    pub common_mistake: String,
    pub reasoning_failure: String,
    pub correction: String,
    pub mistaken_conclusion: String,
    pub gap_diagnoses: Vec<GapDiagnosis>,
    pub recommended_data_actions: Vec<RecommendedDataAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_next: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_resources: Vec<String>,
    pub evidence: Vec<ResourceEvidence>,
    pub resource_uri: String,
    pub retrieval_score: f64,
}

/// Persist a low-trust learned note.
#[must_use]
pub fn submit_vesc_knowledge_feedback_with_store(
    params: &SubmitKnowledgeFeedbackParams,
    store: &FeedbackStore,
) -> FeedbackWriteResponse {
    match learned_note(params).and_then(|record| {
        let id = record.id.clone();
        let duplicate = store.insert(
            KnowledgeRecord::LearnedNote(Box::new(record)),
            params.supersedes.as_deref(),
        )?;
        Ok((id, duplicate))
    }) {
        Ok((id, duplicate)) => FeedbackWriteResponse {
            ok: true,
            id: Some(id),
            duplicate,
            state: Some("unverified_model_feedback".into()),
            evidence: Vec::new(),
            next_actions: vec![
                "Continue gathering registered VESC evidence before creating a correction.".into(),
            ],
            error: None,
        },
        Err(error) => error_response(&error),
    }
}

/// Persist a correction after resolving every supplied VESC resource.
#[must_use]
pub fn correct_vesc_knowledge_tool_with_store(
    params: &CorrectVescKnowledgeParams,
    store: &FeedbackStore,
    resources: &ResourceRegistry,
) -> FeedbackWriteResponse {
    match correction_record(params, resources).and_then(|record| {
        let id = record.id.clone();
        let evidence = record.evidence.clone();
        let duplicate = store.insert(
            KnowledgeRecord::Correction(Box::new(record)),
            params.supersedes.as_deref(),
        )?;
        Ok((id, duplicate, evidence))
    }) {
        Ok((id, duplicate, evidence)) => FeedbackWriteResponse {
            ok: true,
            id: Some(id),
            duplicate,
            state: Some("resource_backed".into()),
            evidence,
            next_actions: vec![
                "Use the correction ID on related answers and cite or read its VESC evidence resources."
                    .into(),
                "Do not describe the model-authored correction wording as first-party text.".into(),
            ],
            error: None,
        },
        Err(error) => error_response(&error),
    }
}

/// Search active notes and current resource-backed corrections.
///
/// # Errors
///
/// Returns an error when the store or feedback lexical index is invalid.
pub fn search_feedback(
    query: &str,
    store: &FeedbackStore,
    resources: &ResourceRegistry,
    filters: &LexicalFilters,
    limit: usize,
) -> Result<FeedbackMatches, FeedbackError> {
    const MIN_ADVISORY_SCORE: f32 = 0.5;

    let records = store.active_records()?;
    if records.is_empty() {
        return Ok(FeedbackMatches {
            notes: Vec::new(),
            corrections: Vec::new(),
        });
    }
    let by_id = records
        .iter()
        .map(|record| (record.id().to_owned(), record.clone()))
        .collect::<BTreeMap<_, _>>();
    let chunks = records
        .iter()
        .map(record_chunk)
        .collect::<Result<Vec<_>, _>>()?;
    let index =
        LexicalIndex::build(&chunks).map_err(|error| FeedbackError::Index(error.to_string()))?;
    let hits = index
        .search(query, filters, limit.max(1))
        .map_err(|error| FeedbackError::Index(error.to_string()))?;
    let mut notes = Vec::new();
    let mut corrections = Vec::new();
    for hit in hits {
        let Some(id) = hit.chunk.legacy_ids.first() else {
            continue;
        };
        match by_id.get(id) {
            Some(KnowledgeRecord::LearnedNote(note)) => notes.push(FeedbackNoteMatch {
                note: note.as_ref().clone(),
                score: hit.score,
            }),
            Some(KnowledgeRecord::Correction(correction))
                if hit.score >= MIN_ADVISORY_SCORE
                    && correction_is_current(correction, resources) =>
            {
                let mut check_next = correction
                    .evidence
                    .iter()
                    .map(|evidence| evidence.uri.clone())
                    .collect::<Vec<_>>();
                check_next.extend(
                    correction
                        .retrieval_trace
                        .insufficient_evidence_next
                        .iter()
                        .cloned(),
                );
                check_next.sort();
                check_next.dedup();
                corrections.push(KnowledgeCorrectionResult {
                    id: correction.id.clone(),
                    state: "resource_backed_gap".into(),
                    question: correction.question.clone(),
                    what_we_know: correction.correction.clone(),
                    common_mistake: correction.mistaken_conclusion.clone(),
                    reasoning_failure: correction.reasoning_failure.clone(),
                    correction: correction.correction.clone(),
                    mistaken_conclusion: correction.mistaken_conclusion.clone(),
                    gap_diagnoses: correction.gap_diagnoses.clone(),
                    recommended_data_actions: correction.recommended_data_actions.clone(),
                    qualifiers: correction.qualifiers.clone(),
                    check_next,
                    affected_resources: correction.affected_resources.clone(),
                    evidence: correction.evidence.clone(),
                    resource_uri: feedback_resource_uri(&correction.id),
                    retrieval_score: f64::from(hit.score),
                });
            }
            _ => {}
        }
    }
    Ok(FeedbackMatches { notes, corrections })
}

fn learned_note(params: &SubmitKnowledgeFeedbackParams) -> Result<LearnedNote, FeedbackError> {
    validate_text("question", &params.question, MAX_QUESTION_BYTES)?;
    validate_text("lesson", &params.lesson, MAX_BODY_BYTES)?;
    validate_lists(&[
        ("related_queries", &params.related_queries),
        ("identifiers", &params.identifiers),
        ("tags", &params.tags),
        ("source_references", &params.source_references),
    ])?;
    validate_optional_id(params.supersedes.as_deref())?;

    let question = params.question.trim().to_owned();
    let lesson = params.lesson.trim().to_owned();
    let related_queries = normalized_list(&params.related_queries);
    let identifiers = normalized_list(&params.identifiers);
    let tags = normalized_list(&params.tags);
    let source_references = normalized_list(&params.source_references);
    let supersedes = params.supersedes.clone();
    let identity = serde_json::to_vec(&(
        &question,
        &lesson,
        &related_queries,
        &identifiers,
        &tags,
        &source_references,
        &supersedes,
    ))?;

    Ok(LearnedNote {
        id: stable_id("feedback", &identity),
        question,
        lesson,
        related_queries,
        identifiers,
        tags,
        source_references,
        supersedes,
        superseded_by: None,
    })
}

fn correction_record(
    params: &CorrectVescKnowledgeParams,
    resources: &ResourceRegistry,
) -> Result<KnowledgeCorrection, FeedbackError> {
    validate_text("question", &params.question, MAX_QUESTION_BYTES)?;
    validate_text(
        "mistaken_conclusion",
        &params.mistaken_conclusion,
        MAX_BODY_BYTES,
    )?;
    validate_text("correction", &params.correction, MAX_BODY_BYTES)?;
    validate_text(
        "reasoning_failure",
        &params.reasoning_failure,
        MAX_BODY_BYTES,
    )?;
    if params.gap_diagnoses.is_empty() || params.gap_diagnoses.len() > MAX_LIST_ITEMS {
        return Err(FeedbackError::Invalid(format!(
            "gap_diagnoses must contain 1 to {MAX_LIST_ITEMS} items"
        )));
    }
    validate_retrieval_trace(&params.retrieval_trace)?;
    validate_lists(&[
        ("qualifiers", &params.qualifiers),
        ("affected_resources", &params.affected_resources),
        ("project_references", &params.project_references),
        ("related_queries", &params.related_queries),
        ("identifiers", &params.identifiers),
        ("tags", &params.tags),
    ])?;
    if params.evidence_resources.is_empty() || params.evidence_resources.len() > MAX_EVIDENCE_ITEMS
    {
        return Err(FeedbackError::Invalid(format!(
            "evidence_resources must contain 1 to {MAX_EVIDENCE_ITEMS} items"
        )));
    }
    validate_optional_id(params.supersedes.as_deref())?;

    let question = params.question.trim().to_owned();
    let mistaken_conclusion = params.mistaken_conclusion.trim().to_owned();
    let correction = params.correction.trim().to_owned();
    let reasoning_failure = params.reasoning_failure.trim().to_owned();
    let mut gap_diagnoses = params.gap_diagnoses.clone();
    gap_diagnoses.sort();
    gap_diagnoses.dedup();
    let recommended_data_actions = gap_diagnoses
        .iter()
        .copied()
        .map(RecommendedDataAction::from)
        .collect();
    let retrieval_trace = normalized_retrieval_trace(&params.retrieval_trace);
    let qualifiers = normalized_list(&params.qualifiers);
    let affected_resources = normalized_list(&params.affected_resources);
    let evidence_resources = normalized_list(&params.evidence_resources);
    let evidence = evidence_resources
        .iter()
        .map(|uri| resolve_evidence(uri, resources))
        .collect::<Result<Vec<_>, _>>()?;
    let project_references = normalized_list(&params.project_references);
    let related_queries = normalized_list(&params.related_queries);
    let identifiers = normalized_list(&params.identifiers);
    let tags = normalized_list(&params.tags);
    let supersedes = params.supersedes.clone();
    let identity = serde_json::to_vec(&(
        &question,
        params.authorization,
        &mistaken_conclusion,
        &correction,
        &reasoning_failure,
        &gap_diagnoses,
        &retrieval_trace,
        &qualifiers,
        &affected_resources,
        &evidence,
        &project_references,
        &related_queries,
        &identifiers,
        &tags,
        &supersedes,
    ))?;

    Ok(KnowledgeCorrection {
        id: stable_id("correction", &identity),
        question,
        authorization: params.authorization,
        mistaken_conclusion,
        correction,
        reasoning_failure,
        gap_diagnoses,
        recommended_data_actions,
        retrieval_trace,
        qualifiers,
        affected_resources,
        evidence,
        project_references,
        related_queries,
        identifiers,
        tags,
        covered_by_base_knowledge: false,
        coverage_evidence: Vec::new(),
        supersedes,
        superseded_by: None,
    })
}

fn resolve_evidence(
    uri: &str,
    resources: &ResourceRegistry,
) -> Result<ResourceEvidence, FeedbackError> {
    validate_text("evidence resource", uri, MAX_ITEM_BYTES)?;
    if !(uri.starts_with("vesc://catalog/")
        || uri.starts_with("vesc://knowledge/chunk/")
        || uri.starts_with("vesc://knowledge/document/"))
    {
        return Err(FeedbackError::Evidence(format!(
            "unsupported evidence URI {uri:?}"
        )));
    }
    let text = resources
        .read(uri)
        .map_err(|error| FeedbackError::Evidence(error.to_string()))?;
    let mut excerpt = text
        .chars()
        .take(EVIDENCE_EXCERPT_CHARS)
        .collect::<String>();
    if text.chars().count() > EVIDENCE_EXCERPT_CHARS {
        excerpt.push('…');
    }
    Ok(ResourceEvidence {
        uri: uri.into(),
        content_digest: vesc_knowledge_index::ContentDigest::of(text.as_bytes()).to_string(),
        excerpt,
    })
}

fn correction_is_current(correction: &KnowledgeCorrection, resources: &ResourceRegistry) -> bool {
    correction.evidence.iter().all(|stored| {
        resolve_evidence(&stored.uri, resources)
            .is_ok_and(|current| current.content_digest == stored.content_digest)
    })
}

fn record_chunk(record: &KnowledgeRecord) -> Result<Chunk, FeedbackError> {
    let (title, content, identifiers, tags) = match record {
        KnowledgeRecord::LearnedNote(note) => (
            format!("Learned note: {}", note.question),
            format!(
                "Question: {}\n\nLesson: {}\n\nRelated: {}",
                note.question,
                note.lesson,
                note.related_queries.join("; ")
            ),
            &note.identifiers,
            &note.tags,
        ),
        KnowledgeRecord::Correction(correction) => (
            format!("Correction: {}", correction.question),
            format!(
                "Question: {}\n\nWhat we know: {}\n\nCommon mistake: {}\n\nWhy the reasoning failed: {}\n\nQualifiers: {}\n\nRelated: {}\n\nIdentifiers: {}\n\nAffected: {}\n\nDecisive evidence: {}\n\nProject references: {}",
                correction.question,
                correction.correction,
                correction.mistaken_conclusion,
                correction.reasoning_failure,
                correction.qualifiers.join("; "),
                correction.related_queries.join("; "),
                correction.identifiers.join("; "),
                correction.affected_resources.join("; "),
                correction.retrieval_trace.decisive_evidence.join("; "),
                correction.project_references.join("; "),
            ),
            &correction.identifiers,
            &correction.tags,
        ),
    };
    let mut document = NormalizedDocument::new(
        title,
        SourceKind::ModelFeedback,
        RepositoryId::try_from("vesc-mcp-feedback")
            .map_err(|error| FeedbackError::Invalid(error.to_string()))?,
        Revision::try_from("runtime-feedback-v1")
            .map_err(|error| FeedbackError::Invalid(error.to_string()))?,
        format!("feedback/{}.md", record.id()),
        "text/markdown",
        content,
    )
    .map_err(|error| FeedbackError::Invalid(error.to_string()))?;
    document.trust_tier = TrustTier::UnverifiedModelFeedback;
    document.license = LicenseStatus::ReferenceOnly;
    document.identifiers.extend(identifiers.iter().cloned());
    document
        .tags
        .extend(tags.iter().map(|tag| tag.to_ascii_lowercase()));
    document.legacy_ids.push(record.id().to_owned());
    document.canonical_uri = Some(
        ResourceUri::try_from(feedback_resource_uri(record.id()))
            .map_err(|error| FeedbackError::Invalid(error.to_string()))?,
    );
    let mut chunk = Chunk::from_document(&document, 0, document.content.clone(), Vec::new(), None)
        .map_err(|error| FeedbackError::Invalid(error.to_string()))?;
    chunk.resource_uri = document.canonical_uri;
    Ok(chunk)
}

fn validate_text(name: &str, value: &str, max_bytes: usize) -> Result<(), FeedbackError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(FeedbackError::Invalid(format!("{name} must not be empty")));
    }
    if value.len() > max_bytes {
        return Err(FeedbackError::Limit(format!(
            "{name} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn validate_retrieval_trace(trace: &RetrievalTrace) -> Result<(), FeedbackError> {
    validate_text("retrieval_trace.query", &trace.query, MAX_QUESTION_BYTES)?;
    if trace.limit == 0 || trace.limit > 100 {
        return Err(FeedbackError::Invalid(
            "retrieval_trace.limit must be between 1 and 100".into(),
        ));
    }
    if trace.results.len() > trace.limit {
        return Err(FeedbackError::Limit(format!(
            "retrieval_trace.results exceeds retrieval_trace.limit ({})",
            trace.limit
        )));
    }
    if trace.decisive_evidence.is_empty() {
        return Err(FeedbackError::Invalid(
            "retrieval_trace.decisive_evidence must not be empty".into(),
        ));
    }
    validate_lists(&[
        ("retrieval_trace.filters", &trace.filters),
        (
            "retrieval_trace.decisive_evidence",
            &trace.decisive_evidence,
        ),
        ("retrieval_trace.distractors", &trace.distractors),
        (
            "retrieval_trace.insufficient_evidence_next",
            &trace.insufficient_evidence_next,
        ),
    ])?;
    for result in &trace.results {
        validate_text("retrieval_trace.results.id", &result.id, MAX_ITEM_BYTES)?;
        validate_text(
            "retrieval_trace.results.excerpt",
            &result.excerpt,
            MAX_BODY_BYTES,
        )?;
        if let Some(score) = &result.score {
            validate_text("retrieval_trace.results.score", score, MAX_ITEM_BYTES)?;
        }
        if let Some(uri) = &result.resource_uri {
            validate_text("retrieval_trace.results.resource_uri", uri, MAX_ITEM_BYTES)?;
        }
    }
    Ok(())
}

fn normalized_retrieval_trace(trace: &RetrievalTrace) -> RetrievalTrace {
    RetrievalTrace {
        query: trace.query.trim().to_owned(),
        mode: trace.mode.as_ref().map(|value| value.trim().to_owned()),
        limit: trace.limit,
        max_response_bytes: trace.max_response_bytes,
        max_context_bytes: trace.max_context_bytes,
        filters: normalized_list(&trace.filters),
        results: trace
            .results
            .iter()
            .map(|result| RetrievalTraceResult {
                id: result.id.trim().to_owned(),
                score: result.score.as_ref().map(|value| value.trim().to_owned()),
                excerpt: result.excerpt.trim().to_owned(),
                resource_uri: result
                    .resource_uri
                    .as_ref()
                    .map(|value| value.trim().to_owned()),
            })
            .collect(),
        decisive_evidence: normalized_list(&trace.decisive_evidence),
        distractors: normalized_list(&trace.distractors),
        insufficient_evidence_next: normalized_list(&trace.insufficient_evidence_next),
    }
}

fn validate_lists(lists: &[(&str, &[String])]) -> Result<(), FeedbackError> {
    for (name, values) in lists {
        if values.len() > MAX_LIST_ITEMS {
            return Err(FeedbackError::Limit(format!(
                "{name} exceeds {MAX_LIST_ITEMS} items"
            )));
        }
        for value in *values {
            validate_text(name, value, MAX_ITEM_BYTES)?;
        }
    }
    Ok(())
}

fn validate_optional_id(id: Option<&str>) -> Result<(), FeedbackError> {
    if let Some(id) = id {
        validate_text("supersedes", id, MAX_ITEM_BYTES)?;
    }
    Ok(())
}

fn normalized_list(values: &[String]) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| value.trim().to_owned())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn stable_id(prefix: &str, bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut id = format!("{prefix}-");
    for byte in &digest[..12] {
        write!(&mut id, "{byte:02x}").expect("writing to String cannot fail");
    }
    id
}

fn feedback_resource_uri(id: &str) -> String {
    format!("vesc://knowledge/feedback/{id}")
}

fn reject_symlink(path: &Path) -> Result<(), FeedbackError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(FeedbackError::Invalid(format!(
            "feedback root {} must not be a symlink",
            path.display()
        ))),
        Ok(metadata) if !metadata.is_dir() => Err(FeedbackError::Invalid(format!(
            "feedback root {} must be a directory",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn error_response(error: &FeedbackError) -> FeedbackWriteResponse {
    FeedbackWriteResponse {
        ok: false,
        id: None,
        duplicate: false,
        state: None,
        evidence: Vec::new(),
        next_actions: Vec::new(),
        error: Some(error.to_string()),
    }
}
