//! Durable learned notes and VESC-resource-backed corrections.

use crate::resources::ResourceRegistry;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
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

static STORE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
    /// Important limitations or conditions on the corrected fact.
    #[serde(default)]
    pub qualifiers: Vec<String>,
    /// Result IDs or resource URIs affected by the correction.
    #[serde(default)]
    pub affected_resources: Vec<String>,
    /// Registered VESC catalog or knowledge resource URIs supporting the correction.
    pub evidence_resources: Vec<String>,
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
    pub qualifiers: Vec<String>,
    pub affected_resources: Vec<String>,
    pub evidence: Vec<ResourceEvidence>,
    pub related_queries: Vec<String>,
    pub identifiers: Vec<String>,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// Durable knowledge record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KnowledgeRecord {
    LearnedNote(LearnedNote),
    Correction(KnowledgeCorrection),
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
            .filter(|record| record.superseded_by().is_none())
            .collect())
    }

    fn insert(
        &self,
        record: KnowledgeRecord,
        supersedes: Option<&str>,
    ) -> Result<bool, FeedbackError> {
        let lock = STORE_WRITE_LOCK.get_or_init(|| Mutex::new(()));
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let temp = self
            .root
            .join(format!(".feedback.tmp-{}", std::process::id()));
        fs::write(&temp, bytes)?;
        fs::rename(temp, self.path())?;
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

/// Correction rendered separately from authoritative search passages.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct KnowledgeCorrectionResult {
    pub id: String,
    pub state: String,
    pub question: String,
    pub mistaken_conclusion: String,
    pub correction: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<String>,
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
            KnowledgeRecord::LearnedNote(record),
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
            KnowledgeRecord::Correction(record),
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
    limit: usize,
) -> Result<FeedbackMatches, FeedbackError> {
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
        .search(query, &LexicalFilters::default(), limit.max(1))
        .map_err(|error| FeedbackError::Index(error.to_string()))?;
    let mut notes = Vec::new();
    let mut corrections = Vec::new();
    for hit in hits {
        let Some(id) = hit.chunk.legacy_ids.first() else {
            continue;
        };
        match by_id.get(id) {
            Some(KnowledgeRecord::LearnedNote(note)) => notes.push(FeedbackNoteMatch {
                note: note.clone(),
                score: hit.score,
            }),
            Some(KnowledgeRecord::Correction(correction))
                if correction_is_current(correction, resources) =>
            {
                corrections.push(KnowledgeCorrectionResult {
                    id: correction.id.clone(),
                    state: "resource_backed".into(),
                    question: correction.question.clone(),
                    mistaken_conclusion: correction.mistaken_conclusion.clone(),
                    correction: correction.correction.clone(),
                    qualifiers: correction.qualifiers.clone(),
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
    validate_lists(&[
        ("qualifiers", &params.qualifiers),
        ("affected_resources", &params.affected_resources),
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
    let qualifiers = normalized_list(&params.qualifiers);
    let affected_resources = normalized_list(&params.affected_resources);
    let evidence_resources = normalized_list(&params.evidence_resources);
    let evidence = evidence_resources
        .iter()
        .map(|uri| resolve_evidence(uri, resources))
        .collect::<Result<Vec<_>, _>>()?;
    let related_queries = normalized_list(&params.related_queries);
    let identifiers = normalized_list(&params.identifiers);
    let tags = normalized_list(&params.tags);
    let supersedes = params.supersedes.clone();
    let identity = serde_json::to_vec(&(
        &question,
        params.authorization,
        &mistaken_conclusion,
        &correction,
        &qualifiers,
        &affected_resources,
        &evidence,
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
        qualifiers,
        affected_resources,
        evidence,
        related_queries,
        identifiers,
        tags,
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
                "Question: {}

Lesson: {}

Related: {}",
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
                "Question: {}

Mistaken conclusion: {}

Correction: {}

Qualifiers: {}

Related: {}

Affected: {}",
                correction.question,
                correction.mistaken_conclusion,
                correction.correction,
                correction.qualifiers.join("; "),
                correction.related_queries.join("; "),
                correction.affected_resources.join("; ")
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

fn validate_lists(lists: &[(&str, &Vec<String>)]) -> Result<(), FeedbackError> {
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
