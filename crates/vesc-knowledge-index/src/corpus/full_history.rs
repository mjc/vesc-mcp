//! Incremental, content-addressed ingestion of complete reachable Git history.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::sync::Arc;

use gix::bstr::ByteSlice;
use serde::{
    Deserialize, Serialize,
    de::{Deserializer, SeqAccess, Visitor},
};
use sha2::{Digest, Sha256};

use super::chunking::{ChunkingConfig, chunk_document_drafts, for_each_chunk_draft};
use super::git::{
    CachedGitBlob, Candidate, GitCorpusBudget, GitCorpusPolicy, GitCorpusSource, GitIngestionError,
    GitIngestionObservations, MAX_IDENTIFIERS, commit_message_size, document_from_git_blob,
    document_from_git_commit, identifier_refs, identifier_values, is_selected, load_git_blob,
    validate_policy,
};
use super::{
    Category, Chunk, ChunkId, ChunkIdentity, ContentDigest, CorpusError, DocumentId,
    NormalizedDocument, RepositoryId, Revision, SourceKind, SourceSpan, TrustTier,
};
use crate::semantic::{embedding_text_digest_from_metadata, embedding_text_digest_from_parts};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHistoryTip {
    pub repository: RepositoryId,
    pub revision: Revision,
}

/// Work performed by one refresh. These counters do not affect artifact identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHistoryRefreshObservations {
    pub reachable_commits: usize,
    pub reused_commits: usize,
    pub ingested_commits: usize,
    pub ingested_blobs: usize,
    pub reused_blobs: usize,
    pub ingested_commit_messages: usize,
    pub reused_commit_messages: usize,
    pub removed_documents: usize,
    pub removed_commit_messages: usize,
    pub rejected_commit_messages: usize,
    pub budget_rejections: usize,
    pub reused_contents: usize,
    pub candidate_chunks: usize,
    pub materialized_chunks: usize,
    pub candidate_identifier_count_histogram: [u64; MAX_IDENTIFIERS + 1],
    pub materialized_identifier_count_histogram: [u64; MAX_IDENTIFIERS + 1],
    pub git: GitIngestionObservations,
}

const MIN_DELTA_CHUNK_INDEX_RESERVE: usize = 4_096;
const CHUNK_INDEX_RESERVE_BATCH: usize = 65_536;

const fn delta_chunk_index_capacity(cached_chunks: usize) -> usize {
    if cached_chunks >= MIN_DELTA_CHUNK_INDEX_RESERVE {
        cached_chunks
    } else {
        0
    }
}

const fn chunk_index_reserve_target(candidate_chunks: usize) -> usize {
    if candidate_chunks < MIN_DELTA_CHUNK_INDEX_RESERVE {
        0
    } else {
        candidate_chunks.saturating_add(CHUNK_INDEX_RESERVE_BATCH - 1) / CHUNK_INDEX_RESERVE_BATCH
            * CHUNK_INDEX_RESERVE_BATCH
    }
}

fn join_tree_path(prefix: &str, filename: &str) -> String {
    if prefix.is_empty() {
        return filename.to_owned();
    }
    let mut path = String::with_capacity(prefix.len() + 1 + filename.len());
    path.push_str(prefix);
    path.push('/');
    path.push_str(filename);
    path
}

impl Default for GitHistoryRefreshObservations {
    fn default() -> Self {
        Self {
            reachable_commits: 0,
            reused_commits: 0,
            ingested_commits: 0,
            ingested_blobs: 0,
            reused_blobs: 0,
            ingested_commit_messages: 0,
            reused_commit_messages: 0,
            removed_documents: 0,
            removed_commit_messages: 0,
            rejected_commit_messages: 0,
            budget_rejections: 0,
            reused_contents: 0,
            candidate_chunks: 0,
            materialized_chunks: 0,
            candidate_identifier_count_histogram: [0; MAX_IDENTIFIERS + 1],
            materialized_identifier_count_histogram: [0; MAX_IDENTIFIERS + 1],
            git: GitIngestionObservations::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitHistoryError {
    #[error(transparent)]
    GitIngestion(#[from] GitIngestionError),
    #[error("cannot traverse Git history: {0}")]
    Git(String),
    #[error("cannot chunk Git history: {0}")]
    Chunking(String),
    #[error("invalid Git history artifact: {0}")]
    Invalid(String),
}

#[derive(Debug)]
struct ReachableCommit {
    id: gix::ObjectId,
    first_parent: Option<gix::ObjectId>,
    message_admitted: bool,
}

struct CommitAdmissions {
    revisions: HashMap<gix::ObjectId, CommitAdmission>,
}

struct CommitAdmission {
    message_admitted: bool,
    retained: bool,
}

impl CommitAdmissions {
    fn is_reachable(&self, revision: &gix::ObjectId) -> bool {
        self.revisions.contains_key(revision)
    }

    fn message_is_admitted(&self, revision: &gix::ObjectId) -> bool {
        self.revisions
            .get(revision)
            .is_some_and(|admission| admission.message_admitted)
    }

    fn len(&self) -> usize {
        self.revisions.len()
    }

    fn retained_count(&self) -> usize {
        self.revisions
            .values()
            .filter(|admission| admission.retained)
            .count()
    }

    fn mark_retained(
        &mut self,
        repo: &gix::Repository,
        tips: &[gix::ObjectId],
    ) -> Result<(), GitHistoryError> {
        if tips.is_empty() {
            return Ok(());
        }
        let walk = repo
            .rev_walk(tips.iter().copied())
            .all()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        for info in walk {
            let info = info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
            if let Some(admission) = self.revisions.get_mut(&info.id) {
                admission.retained = true;
            }
        }
        Ok(())
    }
}

struct ProcessedHistory<'a> {
    source: &'a GitCorpusSource,
    represented_tips: HashMap<Vec<gix::ObjectId>, usize>,
    reusable_blobs: HistoryBlobDeduper,
}

struct SourceHistory {
    repository: gix::Repository,
    current_ids: Vec<gix::ObjectId>,
    previous_ids: Vec<gix::ObjectId>,
}

struct TipAdmissions {
    blobs: HashMap<String, Vec<gix::ObjectId>>,
}

impl TipAdmissions {
    fn is_admitted(&self, path: &str, id: gix::ObjectId) -> bool {
        self.blobs.get(path).is_some_and(|ids| ids.contains(&id))
    }
}

/// Borrowed history projection used by lexical and sidecar writers.
///
/// It keeps the document and draft storage borrowed while exposing the exact
/// fields those writers need. The public `Chunk` remains available through
/// `try_for_each_chunk` for callers that need owned passages.
pub(crate) struct GitHistoryChunkView<'a> {
    document: &'a NormalizedDocument,
    draft: &'a super::chunking::ChunkDraft<'a>,
    chunk_id: ChunkId,
    identifiers: Vec<compact_str::CompactString>,
    previous_chunk: Option<ChunkId>,
    next_chunk: Option<ChunkId>,
}

impl<'a> GitHistoryChunkView<'a> {
    fn new(
        drafts: &'a super::chunking::ChunkDrafts<'a>,
        index: usize,
        document: &'a NormalizedDocument,
        identifiers: Vec<compact_str::CompactString>,
    ) -> Self {
        let draft = drafts.get(index);
        Self {
            document,
            draft,
            chunk_id: draft.chunk_id(),
            identifiers,
            previous_chunk: index
                .checked_sub(1)
                .map(|previous| drafts.get(previous).chunk_id()),
            next_chunk: drafts
                .get_optional(index + 1)
                .map(super::chunking::ChunkDraft::chunk_id),
        }
    }

    pub(crate) const fn chunk_id(&self) -> &ChunkId {
        &self.chunk_id
    }

    pub(crate) const fn document_id(&self) -> &DocumentId {
        &self.document.document_id
    }

    pub(crate) const fn ordinal(&self) -> u32 {
        self.draft.ordinal()
    }

    pub(crate) fn title(&self) -> &str {
        &self.document.title
    }

    pub(crate) const fn source_kind(&self) -> SourceKind {
        self.document.source_kind
    }

    pub(crate) const fn repository(&self) -> &RepositoryId {
        &self.document.repository
    }

    pub(crate) const fn revision(&self) -> &Revision {
        &self.document.revision
    }

    pub(crate) fn path(&self) -> &str {
        &self.document.path
    }

    pub(crate) fn headings(&self) -> &[&str] {
        self.draft.headings()
    }

    pub(crate) fn text(&self) -> &str {
        self.draft.text()
    }

    pub(crate) const fn source_span(&self) -> Option<SourceSpan> {
        self.draft.source_span()
    }

    pub(crate) fn identifiers(&self) -> &[compact_str::CompactString] {
        &self.identifiers
    }

    pub(crate) const fn tags(&self) -> &BTreeSet<String> {
        &self.document.tags
    }

    pub(crate) const fn category(&self) -> Option<Category> {
        self.document.category
    }

    pub(crate) fn registered_id(&self) -> Option<&str> {
        self.document.registered_id.as_deref()
    }

    pub(crate) const fn trust_tier(&self) -> TrustTier {
        self.document.trust_tier
    }

    pub(crate) const fn previous_chunk(&self) -> Option<&ChunkId> {
        self.previous_chunk.as_ref()
    }

    pub(crate) const fn next_chunk(&self) -> Option<&ChunkId> {
        self.next_chunk.as_ref()
    }

    pub(crate) const fn content_digest(&self) -> &ContentDigest {
        self.draft.content_digest()
    }

    pub(crate) fn char_count(&self) -> u32 {
        u32::try_from(self.text().chars().count()).expect("chunking config bounds character count")
    }

    pub(crate) fn byte_count(&self) -> u64 {
        self.text().len() as u64
    }

    pub(crate) fn history_content_key(&self) -> ContentDigest {
        history_content_key_for_fields(
            self.repository(),
            self.path(),
            self.title(),
            self.headings().iter().copied(),
            self.identifiers(),
            self.tags(),
            self.text(),
        )
    }

    fn into_chunk(self) -> Result<Chunk, CorpusError> {
        let mut chunk = Chunk::from_document_identity(
            self.document,
            self.ordinal(),
            self.text().to_owned(),
            self.headings()
                .iter()
                .map(|heading| (*heading).to_owned())
                .collect(),
            self.source_span(),
            ChunkIdentity::from_sha256(self.content_digest().clone(), *self.chunk_id().as_bytes()),
        )?;
        chunk.identifiers = self.identifiers;
        chunk.previous_chunk = self.previous_chunk;
        chunk.next_chunk = self.next_chunk;
        Ok(chunk)
    }
}

#[derive(Clone)]
pub(crate) struct CachedGitHistoryChunk<'a> {
    pub document_id: &'a str,
    pub repository: &'a str,
    pub revision: &'a str,
    pub path: &'a str,
    pub ordinal: u32,
    pub has_previous: bool,
    pub has_next: bool,
    pub blob: Option<gix::ObjectId>,
    pub source_kind: SourceKind,
    pub content_key: Option<ContentDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedGitHistoryRecord {
    pub document_id: String,
    pub repository: String,
    pub revision: String,
    pub path: String,
    pub ordinal: u32,
    pub has_previous: bool,
    pub has_next: bool,
    pub blob: Option<String>,
    pub source_kind: SourceKind,
    pub content_key: Option<ContentDigest>,
}

impl CachedGitHistoryRecord {
    pub(crate) fn as_chunk(&self) -> Result<CachedGitHistoryChunk<'_>, String> {
        let blob = self
            .blob
            .as_deref()
            .map(|value| gix::ObjectId::from_hex(value.as_bytes()))
            .transpose()
            .map_err(|error| format!("invalid Git object ID in history sidecar: {error}"))?;
        Ok(CachedGitHistoryChunk {
            document_id: &self.document_id,
            repository: &self.repository,
            revision: &self.revision,
            path: &self.path,
            ordinal: self.ordinal,
            has_previous: self.has_previous,
            has_next: self.has_next,
            blob,
            source_kind: self.source_kind,
            content_key: self.content_key.clone(),
        })
    }
}

/// Compact membership index over the persisted history projection.
///
/// Keep only the identity fields needed during an incremental refresh. The
/// full sidecar records can then be released before Git planning starts.
pub(crate) struct CachedGitHistoryMembership {
    records: Vec<CachedGitHistoryMembershipRecord>,
}

struct CachedGitHistoryMembershipRecord {
    document_id: String,
    revision: String,
    content_key: Option<ContentDigest>,
}

impl CachedGitHistoryMembership {
    fn push_owned(&mut self, record: CachedGitHistoryRecord) {
        self.records.push(CachedGitHistoryMembershipRecord {
            document_id: record.document_id,
            revision: record.revision,
            content_key: record.content_key,
        });
    }

    fn sort(&mut self) {
        self.records.sort_unstable_by(|left, right| {
            left.content_key
                .cmp(&right.content_key)
                .then_with(|| left.revision.cmp(&right.revision))
        });
    }

    pub(crate) fn contains_retained(
        &self,
        key: &ContentDigest,
        revision: &gix::ObjectId,
        removed_document_ids: &BTreeSet<String>,
    ) -> bool {
        let mut revision_hex = gix::hash::Kind::hex_buf();
        let revision = revision.as_ref().hex_to_buf(&mut revision_hex);
        let compare = |index: usize| {
            let record = &self.records[index];
            record
                .content_key
                .as_ref()
                .cmp(&Some(key))
                .then_with(|| record.revision.as_bytes().cmp(revision.as_bytes()))
        };
        let first = self.records.partition_point(|record| {
            compare_index(record, key, revision.as_bytes()) == Ordering::Less
        });
        let mut cursor = first;
        while cursor < self.records.len() && compare(cursor) == Ordering::Equal {
            let record = &self.records[cursor];
            if !removed_document_ids.contains(&record.document_id) {
                return true;
            }
            cursor += 1;
        }
        false
    }
}

fn compare_index(
    record: &CachedGitHistoryMembershipRecord,
    key: &ContentDigest,
    revision: &[u8],
) -> Ordering {
    record
        .content_key
        .as_ref()
        .cmp(&Some(key))
        .then_with(|| record.revision.as_bytes().cmp(revision))
}

#[cfg(test)]
mod history_membership_tests {
    use super::*;

    #[test]
    fn history_membership_finds_retained_content_without_document_hydration() {
        let key = ContentDigest::of(b"content");
        let revision =
            gix::ObjectId::from_hex(b"0123456789012345678901234567890123456789").expect("revision");
        let records = vec![
            CachedGitHistoryRecord {
                document_id: "removed".into(),
                repository: "repo".into(),
                revision: revision.to_string(),
                path: "removed.c".into(),
                ordinal: 0,
                has_previous: false,
                has_next: false,
                blob: None,
                source_kind: SourceKind::GitBlob,
                content_key: Some(key.clone()),
            },
            CachedGitHistoryRecord {
                document_id: "retained".into(),
                repository: "repo".into(),
                revision: revision.to_string(),
                path: "retained.c".into(),
                ordinal: 0,
                has_previous: false,
                has_next: false,
                blob: None,
                source_kind: SourceKind::GitBlob,
                content_key: Some(key.clone()),
            },
        ];
        let json = serde_json::to_vec(&records).expect("serialize history records");
        let projection = CachedGitHistoryProjection::from_json_reader(std::io::Cursor::new(json))
            .expect("valid history JSON")
            .expect("valid history records");
        let (_, membership) = projection.into_parts();

        assert!(membership.contains_retained(
            &key,
            &revision,
            &BTreeSet::from(["removed".to_owned()]),
        ));
        assert!(!membership.contains_retained(
            &key,
            &revision,
            &BTreeSet::from(["removed".to_owned(), "retained".to_owned()]),
        ));
    }

    #[test]
    fn persisted_history_projection_builds_cache_and_membership_together() {
        let key = ContentDigest::of(b"content");
        let revision =
            gix::ObjectId::from_hex(b"0123456789012345678901234567890123456789").expect("revision");
        let records = vec![CachedGitHistoryRecord {
            document_id: "document".into(),
            repository: "repo".into(),
            revision: revision.to_string(),
            path: "file.c".into(),
            ordinal: 0,
            has_previous: false,
            has_next: false,
            blob: None,
            source_kind: SourceKind::GitBlob,
            content_key: Some(key.clone()),
        }];

        let json = serde_json::to_vec(&records).expect("serialize history records");
        let projection = CachedGitHistoryProjection::from_json_reader(std::io::Cursor::new(json))
            .expect("valid history JSON")
            .expect("valid history record");
        let (history, membership) = projection.into_parts();

        assert_eq!(history.chunk_count(), 1);
        assert!(membership.contains_retained(&key, &revision, &BTreeSet::new()));
    }

    #[test]
    fn persisted_history_projection_streams_json_records() {
        let record = CachedGitHistoryRecord {
            document_id: "document".into(),
            repository: "repo".into(),
            revision: "0123456789012345678901234567890123456789".into(),
            path: "file.c".into(),
            ordinal: 0,
            has_previous: false,
            has_next: false,
            blob: None,
            source_kind: SourceKind::GitBlob,
            content_key: Some(ContentDigest::of(b"content")),
        };
        let json = serde_json::to_vec(&[record]).expect("serialize history records");

        let projection = CachedGitHistoryProjection::from_json_reader(std::io::Cursor::new(json))
            .expect("valid history JSON")
            .expect("valid history record");

        let (history, _) = projection.into_parts();
        assert_eq!(history.chunk_count(), 1);
    }
}

#[derive(Default)]
pub(crate) struct CachedGitHistory {
    repositories: HashMap<String, HashMap<String, CachedGitHistoryDocument>>,
}

struct CachedGitHistoryDocument {
    revision: Box<str>,
    path: Box<str>,
    blob: Option<gix::ObjectId>,
    source_kind: SourceKind,
    chunk_count: u32,
    maximum_ordinal: u32,
    last_ordinal: Option<u32>,
    starts_at_zero: bool,
    consistent: bool,
    content_keys: Vec<(ContentDigest, u32)>,
}

#[derive(Default)]
struct HistoryReconciliation {
    removed_documents: usize,
    removed_blob_documents: usize,
    removed_commit_messages: usize,
    reused_commit_messages: usize,
}

impl CachedGitHistory {
    fn chunk_count(&self) -> usize {
        self.repositories
            .values()
            .flat_map(HashMap::values)
            .map(|document| document.chunk_count as usize)
            .fold(0, usize::saturating_add)
    }

    pub(crate) fn observe(&mut self, chunk: CachedGitHistoryChunk<'_>) {
        let documents = self
            .repositories
            .entry(chunk.repository.to_owned())
            .or_default();
        let document = documents
            .entry(chunk.document_id.to_owned())
            .or_insert_with(|| CachedGitHistoryDocument {
                revision: chunk.revision.into(),
                path: chunk.path.into(),
                blob: chunk.blob,
                source_kind: chunk.source_kind,
                chunk_count: 0,
                maximum_ordinal: 0,
                last_ordinal: None,
                starts_at_zero: false,
                consistent: true,
                content_keys: Vec::new(),
            });
        document.consistent &= document.revision.as_ref() == chunk.revision
            && document.path.as_ref() == chunk.path
            && document.blob == chunk.blob
            && document.source_kind == chunk.source_kind
            && (chunk.ordinal != 0 || !chunk.has_previous);
        document.chunk_count = document.chunk_count.saturating_add(1);
        document.maximum_ordinal = document.maximum_ordinal.max(chunk.ordinal);
        document.starts_at_zero |= chunk.ordinal == 0 && !chunk.has_previous;
        if let Some(content_key) = chunk.content_key {
            document.content_keys.push((content_key, chunk.ordinal));
        }
        if !chunk.has_next {
            document.consistent &= document
                .last_ordinal
                .is_none_or(|ordinal| ordinal == chunk.ordinal);
            document.last_ordinal = Some(chunk.ordinal);
        }
    }

    fn take_reusable_blobs(
        &mut self,
        source: &GitCorpusSource,
        repo: &gix::Repository,
    ) -> Result<HistoryBlobDeduper, GitHistoryError> {
        let Some(documents) = self.repositories.remove(source.repository_id.as_str()) else {
            return Ok(HistoryBlobDeduper::default());
        };
        let mut blobs = HistoryBlobDeduper::default();
        for document in documents.into_values() {
            if document.source_kind != SourceKind::GitBlob {
                continue;
            }
            let complete = document.consistent
                && document.starts_at_zero
                && document.last_ordinal == Some(document.maximum_ordinal)
                && document.chunk_count == document.maximum_ordinal.saturating_add(1);
            if !complete {
                continue;
            }
            let revision = gix::ObjectId::from_hex(document.revision.as_bytes())
                .map_err(|error| GitHistoryError::Invalid(error.to_string()))?;
            let blob = document.blob.map_or_else(
                || {
                    let commit = repo
                        .find_commit(revision)
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                    let tree = commit
                        .tree()
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                    let entry = tree
                        .lookup_entry_by_path(document.path.as_ref())
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?
                        .ok_or_else(|| {
                            GitHistoryError::Invalid(format!(
                                "cached history path is absent: {}@{}:{}",
                                source.repository_id, document.revision, document.path
                            ))
                        })?;
                    entry
                        .mode()
                        .is_blob()
                        .then(|| entry.object_id())
                        .ok_or_else(|| {
                            GitHistoryError::Invalid(format!(
                                "cached history path is not a blob: {}@{}:{}",
                                source.repository_id, document.revision, document.path
                            ))
                        })
                },
                Ok,
            )?;
            blobs.insert_with_keys(
                document.path.as_ref(),
                blob,
                revision,
                true,
                document.content_keys,
            );
        }
        Ok(blobs)
    }

    fn reconcile_documents(
        &mut self,
        source: &GitCorpusSource,
        commits: &CommitAdmissions,
    ) -> Result<(BTreeSet<String>, HistoryReconciliation), GitHistoryError> {
        let Some(documents) = self.repositories.get_mut(source.repository_id.as_str()) else {
            return Ok((BTreeSet::new(), HistoryReconciliation::default()));
        };
        let mut reconciliation = HistoryReconciliation::default();
        let mut invalid_revision = None;
        let removed = documents
            .extract_if(|_, document| {
                let revision = gix::ObjectId::from_hex(document.revision.as_bytes())
                    .map_err(|error| GitHistoryError::Invalid(error.to_string()));
                let Ok(revision) = revision else {
                    invalid_revision = revision.err();
                    return false;
                };
                let reachable = commits.is_reachable(&revision);
                let admitted = document.source_kind != SourceKind::GitCommit
                    || commits.message_is_admitted(&revision);
                if reachable && admitted {
                    reconciliation.reused_commit_messages +=
                        usize::from(document.source_kind == SourceKind::GitCommit);
                    false
                } else {
                    reconciliation.removed_commit_messages +=
                        usize::from(document.source_kind == SourceKind::GitCommit);
                    reconciliation.removed_blob_documents +=
                        usize::from(document.source_kind == SourceKind::GitBlob);
                    true
                }
            })
            .map(|(document_id, _)| document_id)
            .collect::<BTreeSet<_>>();
        if let Some(error) = invalid_revision {
            return Err(error);
        }
        reconciliation.removed_documents = removed.len();
        Ok((removed, reconciliation))
    }
}

/// Owned projections used by persisted-history reconciliation.
///
/// Build the reusable-history cache and compact membership index while the
/// sidecar records are available, then release the full records together.
pub(crate) struct CachedGitHistoryProjection {
    history: CachedGitHistory,
    membership: CachedGitHistoryMembership,
}

impl CachedGitHistoryProjection {
    pub(crate) fn from_json_reader<R: Read>(
        reader: R,
    ) -> Result<Result<Self, String>, serde_json::Error> {
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        deserializer.deserialize_seq(HistoryProjectionVisitor)
    }

    fn push_record(&mut self, record: CachedGitHistoryRecord) -> Result<(), String> {
        let chunk = record.as_chunk()?;
        self.history.observe(chunk);
        self.membership.push_owned(record);
        Ok(())
    }

    fn finish(mut self) -> Self {
        self.membership.sort();
        self
    }

    pub(crate) fn into_parts(self) -> (CachedGitHistory, CachedGitHistoryMembership) {
        (self.history, self.membership)
    }
}

struct HistoryProjectionVisitor;

impl<'de> Visitor<'de> for HistoryProjectionVisitor {
    type Value = Result<CachedGitHistoryProjection, String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON array of persisted Git-history records")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut projection = CachedGitHistoryProjection {
            history: CachedGitHistory::default(),
            membership: CachedGitHistoryMembership {
                records: Vec::new(),
            },
        };
        let mut invalid_record = None;
        while let Some(record) = sequence.next_element::<CachedGitHistoryRecord>()? {
            if invalid_record.is_none()
                && let Err(error) = projection.push_record(record)
            {
                invalid_record = Some(error);
            }
        }
        Ok(invalid_record.map_or_else(|| Ok(projection.finish()), Err))
    }
}

#[derive(Debug)]
struct PendingChange {
    path: String,
    id: gix::ObjectId,
    size: u64,
}

#[derive(Default)]
struct HistoryBlobDeduper {
    path_ids: HashMap<String, usize>,
    // Each path/blob heads a shared proof arena instead of allocating its own Vec.
    blobs: HashMap<(usize, gix::ObjectId), u32>,
    proofs: Vec<IndexedBlob>,
    content_keys: HashMap<(usize, gix::ObjectId), Vec<(ContentDigest, u32)>>,
}

const NO_BLOB_PROOF: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct IndexedBlob {
    revision: gix::ObjectId,
    next: u32,
    reusable: bool,
}

impl HistoryBlobDeduper {
    fn contains(&self, path: &str, id: gix::ObjectId) -> bool {
        self.path_ids
            .get(path)
            .is_some_and(|&path_id| self.blobs.contains_key(&(path_id, id)))
    }

    fn reusable_revisions(
        &self,
        path: &str,
        id: gix::ObjectId,
    ) -> impl Iterator<Item = gix::ObjectId> + '_ {
        let mut proof = self
            .path_ids
            .get(path)
            .and_then(|&path_id| self.blobs.get(&(path_id, id)).copied())
            .unwrap_or(NO_BLOB_PROOF);
        std::iter::from_fn(move || {
            while proof != NO_BLOB_PROOF {
                let indexed = self.proofs[proof as usize];
                proof = indexed.next;
                if indexed.reusable {
                    return Some(indexed.revision);
                }
            }
            None
        })
    }

    fn content_keys(&self, path: &str, id: gix::ObjectId) -> Option<&[(ContentDigest, u32)]> {
        let path_id = *self.path_ids.get(path)?;
        self.content_keys.get(&(path_id, id)).map(Vec::as_slice)
    }

    fn path_id(&mut self, path: &str) -> usize {
        if let Some(&path_id) = self.path_ids.get(path) {
            path_id
        } else {
            let path_id = self.path_ids.len();
            self.path_ids.insert(path.to_owned(), path_id);
            path_id
        }
    }

    fn insert_with_keys(
        &mut self,
        path: &str,
        id: gix::ObjectId,
        revision: gix::ObjectId,
        reusable: bool,
        keys: Vec<(ContentDigest, u32)>,
    ) {
        let path_id = self.path_id(path);
        self.insert_proof(path_id, id, revision, reusable);
        // Empty keys from a rejected blob are a negative cache entry. A
        // reusable cached blob with no keys remains lazy-hydratable.
        if !keys.is_empty() || !reusable {
            self.content_keys.entry((path_id, id)).or_insert(keys);
        }
    }

    fn insert_proof(
        &mut self,
        path_id: usize,
        id: gix::ObjectId,
        revision: gix::ObjectId,
        reusable: bool,
    ) {
        let key = (path_id, id);
        let mut proof = self.blobs.get(&key).copied().unwrap_or(NO_BLOB_PROOF);
        while proof != NO_BLOB_PROOF {
            let indexed = &mut self.proofs[proof as usize];
            if indexed.revision == revision {
                indexed.reusable |= reusable;
                return;
            }
            proof = indexed.next;
        }
        let next = self.blobs.get(&key).copied().unwrap_or(NO_BLOB_PROOF);
        let proof = u32::try_from(self.proofs.len())
            .expect("configured Git limits keep blob proof indices in u32");
        self.proofs.push(IndexedBlob {
            revision,
            next,
            reusable,
        });
        self.blobs.insert(key, proof);
    }

    fn merge_reusable(&mut self, other: Self) {
        let Self {
            path_ids: other_paths,
            blobs: other_blobs,
            proofs: other_proofs,
            content_keys: other_content_keys,
        } = other;
        let mut reusable_paths = vec![false; other_paths.len()];
        for ((path_id, _), &head) in &other_blobs {
            let mut proof = head;
            while proof != NO_BLOB_PROOF {
                let indexed = other_proofs[proof as usize];
                reusable_paths[*path_id] |= indexed.reusable;
                proof = indexed.next;
            }
        }
        let mut path_ids = vec![usize::MAX; other_paths.len()];
        for (path, old_id) in other_paths {
            if !reusable_paths[old_id] {
                continue;
            }
            let new_id = if let Some(&new_id) = self.path_ids.get(&path) {
                new_id
            } else {
                let new_id = self.path_ids.len();
                self.path_ids.insert(path, new_id);
                new_id
            };
            path_ids[old_id] = new_id;
        }
        for ((old_path_id, id), head) in other_blobs {
            let mut proof = head;
            while proof != NO_BLOB_PROOF {
                let indexed = other_proofs[proof as usize];
                if indexed.reusable {
                    self.insert_proof(path_ids[old_path_id], id, indexed.revision, true);
                }
                proof = indexed.next;
            }
        }
        for ((old_path_id, id), keys) in other_content_keys {
            if path_ids[old_path_id] != usize::MAX {
                self.content_keys
                    .entry((path_ids[old_path_id], id))
                    .or_insert(keys);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct CommitCoverage {
    selected: bool,
    reused_from_prior: bool,
}

impl Default for CommitCoverage {
    fn default() -> Self {
        Self {
            selected: false,
            reused_from_prior: true,
        }
    }
}

#[derive(Debug)]
struct GitHistoryDocument {
    source_index: u32,
    revision: u32,
    object: u32,
    path: u32,
}

const HISTORY_DOCUMENT_COMMIT_PATH_BIT: u32 = 1 << 31;

impl GitHistoryDocument {
    fn packed_path(path_id: u32, source_kind: SourceKind) -> Result<u32, GitHistoryError> {
        if path_id >= HISTORY_DOCUMENT_COMMIT_PATH_BIT {
            return Err(GitHistoryError::Invalid(
                "too many Git history paths for the packed document locator".into(),
            ));
        }
        match source_kind {
            SourceKind::GitBlob => Ok(path_id),
            SourceKind::GitCommit => Ok(path_id | HISTORY_DOCUMENT_COMMIT_PATH_BIT),
            _ => Err(GitHistoryError::Invalid(
                "history document must be a Git source".into(),
            )),
        }
    }

    const fn source_kind(&self) -> SourceKind {
        if self.path & HISTORY_DOCUMENT_COMMIT_PATH_BIT == 0 {
            SourceKind::GitBlob
        } else {
            SourceKind::GitCommit
        }
    }

    const fn path_id(&self) -> u32 {
        self.path & !HISTORY_DOCUMENT_COMMIT_PATH_BIT
    }
}

#[derive(Clone, Copy)]
struct GitHistoryDocumentLocator<'a> {
    source_index: usize,
    revision: gix::ObjectId,
    object: gix::ObjectId,
    source_kind: SourceKind,
    path: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct GitHistoryChunk {
    document: u32,
    ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct HistoryChunkKey {
    content: ContentDigest,
    revision: u32,
}

const CHUNK_INDEX_COLLISION: u32 = u32::MAX;
const HISTORY_CHUNK_FINGERPRINT_SHARDS: usize = 64;
const HISTORY_CHUNK_ENTRY_BLOCK: usize = 1_048_576;

type HistoryChunkEntry = (u32, GitHistoryChunk);

#[derive(Debug, Default)]
struct HistoryContentPool {
    values: Vec<ContentDigest>,
    ids: HashMap<ContentDigest, u32>,
}

impl HistoryContentPool {
    fn intern(&mut self, content: ContentDigest) -> u32 {
        if let Some(&content_id) = self.ids.get(&content) {
            return content_id;
        }
        let content_id = u32::try_from(self.values.len())
            .expect("configured Git limits keep history content IDs in u32");
        self.values.push(content.clone());
        self.ids.insert(content, content_id);
        content_id
    }

    fn get(&self, content_id: u32) -> Option<&ContentDigest> {
        self.values.get(content_id as usize)
    }

    fn compact(&mut self, entries: &mut HistoryChunkEntries) {
        let mut remap = vec![u32::MAX; self.values.len()];
        for (content_id, _) in entries.iter() {
            remap[*content_id as usize] = 0;
        }
        for (next, mapped) in remap
            .iter_mut()
            .filter(|mapped| **mapped != u32::MAX)
            .enumerate()
        {
            *mapped = u32::try_from(next).expect("history content index fits in u32");
        }
        for (content_id, _) in entries.iter_mut() {
            *content_id = remap[*content_id as usize];
        }
        let mut old_content = 0_usize;
        self.values.retain(|_| {
            let keep = remap[old_content] != u32::MAX;
            old_content += 1;
            keep
        });
        self.ids.clear();
        for (index, content) in self.values.iter().enumerate() {
            self.ids.insert(
                content.clone(),
                u32::try_from(index).expect("history content index fits in u32"),
            );
        }
    }
}

#[derive(Debug, Default)]
struct HistoryChunkEntries {
    blocks: Vec<Vec<HistoryChunkEntry>>,
    offsets: Vec<(usize, usize)>,
    len: usize,
}

impl HistoryChunkEntries {
    fn with_capacity(capacity: usize) -> Self {
        let mut entries = Self::default();
        entries.reserve(capacity);
        entries
    }

    const fn len(&self) -> usize {
        self.len
    }

    fn capacity(&self) -> usize {
        let mut capacity = 0;
        let mut index = 0;
        while index < self.blocks.len() {
            capacity += self.blocks[index].capacity();
            index += 1;
        }
        capacity
    }

    fn reserve(&mut self, additional: usize) {
        let required = self.len.saturating_add(additional);
        let mut capacity = self.capacity();
        while capacity < required {
            let block_capacity = (required - capacity).min(HISTORY_CHUNK_ENTRY_BLOCK);
            self.blocks.push(Vec::with_capacity(block_capacity));
            capacity += block_capacity;
        }
        self.rebuild_offsets();
    }

    fn push(&mut self, entry: HistoryChunkEntry) {
        if self
            .blocks
            .last()
            .is_none_or(|block| block.len() == block.capacity())
        {
            self.blocks
                .push(Vec::with_capacity(HISTORY_CHUNK_ENTRY_BLOCK));
        }
        self.blocks
            .last_mut()
            .expect("history entry block reserved")
            .push(entry);
        self.len += 1;
        if self.blocks.last().is_some_and(|block| block.len() == 1) {
            self.rebuild_offsets();
        }
    }

    fn get(&self, index: usize) -> Option<&HistoryChunkEntry> {
        let offset = self
            .offsets
            .partition_point(|(offset, _)| *offset <= index)
            .checked_sub(1)?;
        let (start, block) = self.offsets[offset];
        self.blocks.get(block)?.get(index.saturating_sub(start))
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut HistoryChunkEntry> {
        let offset = self
            .offsets
            .partition_point(|(offset, _)| *offset <= index)
            .checked_sub(1)?;
        let (start, block) = self.offsets[offset];
        self.blocks
            .get_mut(block)?
            .get_mut(index.saturating_sub(start))
    }

    fn retain_mut(&mut self, mut keep: impl FnMut(&mut u32, &mut GitHistoryChunk) -> bool) {
        for block in &mut self.blocks {
            block.retain_mut(|(content_id, chunk)| keep(content_id, chunk));
        }
        let mut target = 0;
        for source in 0..self.blocks.len() {
            if self.blocks[source].is_empty() {
                continue;
            }
            loop {
                while target < source && self.blocks[target].len() == self.blocks[target].capacity()
                {
                    target += 1;
                }
                if target >= source {
                    break;
                }
                let available = self.blocks[target].capacity() - self.blocks[target].len();
                let to_move = available.min(self.blocks[source].len());
                let (before_source, source_and_after) = self.blocks.split_at_mut(source);
                before_source[target].extend(source_and_after[0].drain(..to_move));
                if source_and_after[0].is_empty() {
                    break;
                }
            }
        }
        self.blocks.retain(|block| !block.is_empty());
        self.len = self.blocks.iter().map(Vec::len).sum();
        self.rebuild_offsets();
    }

    fn iter(&self) -> impl Iterator<Item = &HistoryChunkEntry> {
        self.blocks.iter().flat_map(|block| block.iter())
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut HistoryChunkEntry> {
        self.blocks.iter_mut().flat_map(|block| block.iter_mut())
    }

    fn rebuild_offsets(&mut self) {
        self.offsets.clear();
        self.offsets.reserve(self.blocks.len());
        let mut offset = 0;
        for (block_index, block) in self.blocks.iter().enumerate() {
            if !block.is_empty() {
                self.offsets.push((offset, block_index));
            }
            offset += block.len();
        }
    }
}

#[derive(Debug)]
struct HistoryChunkFingerprints {
    shards: Vec<HashMap<u64, u32>>,
}

impl Default for HistoryChunkFingerprints {
    fn default() -> Self {
        Self {
            shards: (0..HISTORY_CHUNK_FINGERPRINT_SHARDS)
                .map(|_| HashMap::new())
                .collect(),
        }
    }
}

impl HistoryChunkFingerprints {
    fn with_capacity(capacity: usize) -> Self {
        let per_shard = capacity.saturating_add(HISTORY_CHUNK_FINGERPRINT_SHARDS - 1)
            / HISTORY_CHUNK_FINGERPRINT_SHARDS;
        let mut fingerprints = Self::default();
        for shard in &mut fingerprints.shards {
            shard.reserve(per_shard);
        }
        fingerprints
    }

    #[allow(clippy::cast_possible_truncation)]
    const fn shard(fingerprint: u64) -> usize {
        // The mask leaves only the 6-bit shard number before this cast.
        (fingerprint & (HISTORY_CHUNK_FINGERPRINT_SHARDS as u64 - 1)) as usize
    }

    fn reserve(&mut self, additional: usize) {
        let per_shard = additional.saturating_add(HISTORY_CHUNK_FINGERPRINT_SHARDS - 1)
            / HISTORY_CHUNK_FINGERPRINT_SHARDS;
        for shard in &mut self.shards {
            shard.reserve(per_shard);
        }
    }

    fn get(&self, fingerprint: u64) -> Option<&u32> {
        self.shards[Self::shard(fingerprint)].get(&fingerprint)
    }

    fn insert(&mut self, fingerprint: u64, index: u32) -> Option<u32> {
        let shard = Self::shard(fingerprint);
        self.shards[shard].insert(fingerprint, index)
    }

    fn clear(&mut self) {
        for shard in &mut self.shards {
            shard.clear();
        }
    }

    fn freeze(self) -> FrozenHistoryChunkFingerprints {
        FrozenHistoryChunkFingerprints {
            shards: self
                .shards
                .into_iter()
                .map(|shard| {
                    let mut entries = shard.into_iter().collect::<Vec<_>>();
                    entries.sort_unstable_by_key(|(fingerprint, _)| *fingerprint);
                    entries
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
struct FrozenHistoryChunkFingerprints {
    shards: Vec<Vec<(u64, u32)>>,
}

impl FrozenHistoryChunkFingerprints {
    fn get(&self, fingerprint: u64) -> Option<u32> {
        let shard = self
            .shards
            .get(HistoryChunkFingerprints::shard(fingerprint))?;
        shard
            .binary_search_by_key(&fingerprint, |(value, _)| *value)
            .ok()
            .map(|index| shard[index].1)
    }
}

#[derive(Debug, Default)]
struct HistoryChunkLookup {
    mutable: HistoryChunkFingerprints,
    frozen_seed: Option<FrozenHistoryChunkFingerprints>,
    collisions: HashMap<HistoryChunkKey, u32>,
}

impl HistoryChunkLookup {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            mutable: HistoryChunkFingerprints::with_capacity(capacity),
            ..Self::default()
        }
    }

    fn reserve(&mut self, additional: usize) {
        self.mutable.reserve(additional);
    }

    fn index_of(
        &self,
        key: &HistoryChunkKey,
        entries: &HistoryChunkEntries,
        contents: &HistoryContentPool,
        documents: &[GitHistoryDocument],
    ) -> Option<usize> {
        let fingerprint = HistoryChunkIndex::fingerprint(key);
        let index = self.mutable.get(fingerprint).copied().or_else(|| {
            self.frozen_seed
                .as_ref()
                .and_then(|lookup| lookup.get(fingerprint))
        });
        match index {
            Some(CHUNK_INDEX_COLLISION) => self
                .collisions
                .get(key)
                .copied()
                .map(|index| index as usize),
            Some(index) => {
                let index = index as usize;
                entries
                    .get(index)
                    .filter(|entry| {
                        contents.get(entry.0) == Some(&key.content)
                            && documents
                                .get(entry.1.document as usize)
                                .is_some_and(|document| document.revision == key.revision)
                    })
                    .map(|_| index)
            }
            None => None,
        }
    }

    fn freeze_seed(&mut self) {
        self.frozen_seed = Some(std::mem::take(&mut self.mutable).freeze());
    }

    fn register(
        &mut self,
        fingerprint: u64,
        index: u32,
        key: HistoryChunkKey,
        entries: &HistoryChunkEntries,
        contents: &HistoryContentPool,
        documents: &[GitHistoryDocument],
    ) {
        match self.mutable.get(fingerprint).copied() {
            None => {
                if let Some(existing) = self
                    .frozen_seed
                    .as_ref()
                    .and_then(|lookup| lookup.get(fingerprint))
                {
                    self.mutable.insert(fingerprint, CHUNK_INDEX_COLLISION);
                    if existing != CHUNK_INDEX_COLLISION {
                        let existing_entry =
                            entries.get(existing as usize).expect("history entry index");
                        let existing_key = HistoryChunkKey {
                            content: contents
                                .get(existing_entry.0)
                                .expect("history content ID")
                                .clone(),
                            revision: documents
                                .get(existing_entry.1.document as usize)
                                .expect("history document index")
                                .revision,
                        };
                        self.collisions.insert(existing_key, existing);
                    }
                    self.collisions.insert(key, index);
                } else {
                    self.mutable.insert(fingerprint, index);
                }
            }
            Some(CHUNK_INDEX_COLLISION) => {
                self.collisions.insert(key, index);
            }
            Some(existing) => {
                self.mutable.insert(fingerprint, CHUNK_INDEX_COLLISION);
                let existing_entry = entries.get(existing as usize).expect("history entry index");
                let existing_key = HistoryChunkKey {
                    content: contents
                        .get(existing_entry.0)
                        .expect("history content ID")
                        .clone(),
                    revision: documents
                        .get(existing_entry.1.document as usize)
                        .expect("history document index")
                        .revision,
                };
                self.collisions.insert(existing_key, existing);
                self.collisions.insert(key, index);
            }
        }
    }

    fn clear(&mut self) {
        self.frozen_seed = None;
        self.mutable.clear();
        self.collisions.clear();
    }
}

#[derive(Debug, Default)]
struct HistoryChunkIndex {
    entries: HistoryChunkEntries,
    contents: HistoryContentPool,
    lookup: HistoryChunkLookup,
}

impl HistoryChunkIndex {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HistoryChunkEntries::with_capacity(capacity),
            contents: HistoryContentPool::default(),
            lookup: HistoryChunkLookup::with_capacity(capacity),
        }
    }

    const fn len(&self) -> usize {
        self.entries.len()
    }

    fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
        self.lookup.reserve(additional);
    }

    fn fingerprint(key: &HistoryChunkKey) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn index_of(&self, key: &HistoryChunkKey, documents: &[GitHistoryDocument]) -> Option<usize> {
        self.lookup
            .index_of(key, &self.entries, &self.contents, documents)
    }

    fn contains_key(&self, key: &HistoryChunkKey, documents: &[GitHistoryDocument]) -> bool {
        self.index_of(key, documents).is_some()
    }

    fn insert(
        &mut self,
        key: HistoryChunkKey,
        chunk: GitHistoryChunk,
        documents: &[GitHistoryDocument],
    ) -> Option<GitHistoryChunk> {
        if let Some(index) = self.index_of(&key, documents) {
            return Some(std::mem::replace(
                &mut self.entries.get_mut(index).expect("history entry index").1,
                chunk,
            ));
        }
        let index = u32::try_from(self.entries.len())
            .expect("configured Git limits keep history chunk indices in u32");
        let fingerprint = Self::fingerprint(&key);
        let content_id = self.contents.intern(key.content.clone());
        self.entries.push((content_id, chunk));
        self.register_index(fingerprint, index, key, documents);
        None
    }

    fn freeze_seed_lookup(&mut self) {
        self.lookup.freeze_seed();
    }

    fn register_index(
        &mut self,
        fingerprint: u64,
        index: u32,
        key: HistoryChunkKey,
        documents: &[GitHistoryDocument],
    ) {
        self.lookup.register(
            fingerprint,
            index,
            key,
            &self.entries,
            &self.contents,
            documents,
        );
    }

    fn retain(
        &mut self,
        mut keep: impl FnMut(&ContentDigest, &mut GitHistoryChunk) -> bool,
        documents: &[GitHistoryDocument],
    ) {
        let contents = &self.contents;
        self.entries.retain_mut(|content_id, chunk| {
            keep(
                contents.get(*content_id).expect("history content ID"),
                chunk,
            )
        });
        self.contents.compact(&mut self.entries);
        self.rebuild_index(documents);
    }

    fn rebuild_index(&mut self, documents: &[GitHistoryDocument]) {
        self.lookup.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            let key = HistoryChunkKey {
                content: self
                    .contents
                    .get(entry.0)
                    .expect("history content ID")
                    .clone(),
                revision: documents
                    .get(entry.1.document as usize)
                    .expect("history document index")
                    .revision,
            };
            let fingerprint = Self::fingerprint(&key);
            self.lookup.register(
                fingerprint,
                u32::try_from(index)
                    .expect("configured Git limits keep history chunk indices in u32"),
                key,
                &self.entries,
                &self.contents,
                documents,
            );
        }
    }

    fn values(&self) -> impl Iterator<Item = &GitHistoryChunk> {
        self.entries.iter().map(|(_, chunk)| chunk)
    }

    fn values_mut(&mut self) -> impl Iterator<Item = &mut GitHistoryChunk> {
        self.entries.iter_mut().map(|(_, chunk)| chunk)
    }

    #[cfg(test)]
    fn storage_capacity(&self) -> usize {
        self.entries.capacity()
    }

    #[cfg(test)]
    const fn storage_block_count(&self) -> usize {
        self.entries.blocks.len()
    }

    #[cfg(test)]
    const fn content_count(&self) -> usize {
        self.contents.values.len()
    }
}

#[derive(Debug, Default)]
struct HistoryRevisionPool {
    values: Vec<gix::ObjectId>,
    ids: HashMap<gix::ObjectId, u32>,
}

impl HistoryRevisionPool {
    fn intern(&mut self, revision: gix::ObjectId) -> u32 {
        if let Some(&revision_id) = self.ids.get(&revision) {
            return revision_id;
        }
        let revision_id = u32::try_from(self.values.len())
            .expect("configured Git limits keep revision IDs in u32");
        self.values.push(revision);
        self.ids.insert(revision, revision_id);
        revision_id
    }

    fn get(&self, revision_id: u32) -> Option<gix::ObjectId> {
        self.values.get(revision_id as usize).copied()
    }

    fn compact(&mut self, documents: &mut [GitHistoryDocument], chunks: &mut HistoryChunkIndex) {
        let mut remap = vec![u32::MAX; self.values.len()];
        for document in documents.iter() {
            remap[document.revision as usize] = 0;
        }
        for (next, mapped) in remap
            .iter_mut()
            .filter(|mapped| **mapped != u32::MAX)
            .enumerate()
        {
            *mapped = u32::try_from(next).expect("selected history revision index fits in u32");
        }
        for document in &mut *documents {
            document.revision = remap[document.revision as usize];
        }
        let mut old_revision = 0_usize;
        self.values.retain(|_| {
            let keep = remap[old_revision] != u32::MAX;
            old_revision += 1;
            keep
        });
        self.ids.clear();
        for (index, revision) in self.values.iter().enumerate() {
            self.ids.insert(
                *revision,
                u32::try_from(index).expect("selected history revision index fits in u32"),
            );
        }
        if remap
            .iter()
            .enumerate()
            .any(|(old, &new)| new != u32::try_from(old).expect("revision pool index fits in u32"))
        {
            chunks.rebuild_index(documents);
        }
    }
}

#[derive(Debug, Default)]
struct HistoryObjectPool {
    values: Vec<gix::ObjectId>,
    ids: HashMap<gix::ObjectId, u32>,
}

impl HistoryObjectPool {
    fn intern(&mut self, object: gix::ObjectId) -> u32 {
        if let Some(&object_id) = self.ids.get(&object) {
            return object_id;
        }
        let object_id =
            u32::try_from(self.values.len()).expect("configured Git limits keep object IDs in u32");
        self.values.push(object);
        self.ids.insert(object, object_id);
        object_id
    }

    fn get(&self, object_id: u32) -> Option<gix::ObjectId> {
        self.values.get(object_id as usize).copied()
    }

    fn compact(&mut self, documents: &mut [GitHistoryDocument]) {
        let mut remap = vec![None; self.values.len()];
        for document in documents.iter() {
            remap[document.object as usize] = Some(0);
        }
        for (next, mapped) in remap
            .iter_mut()
            .filter(|mapped| mapped.is_some())
            .enumerate()
        {
            *mapped = Some(u32::try_from(next).expect("selected history object index fits in u32"));
        }
        for document in documents {
            document.object = remap[document.object as usize]
                .expect("history document references a retained object");
        }
        let mut old_object = 0_usize;
        self.values.retain(|_| {
            let keep = remap[old_object].is_some();
            old_object += 1;
            keep
        });
        self.ids.clear();
    }
}

/// Compact cold-build state. Git remains the source of passage text and
/// per-chunk metadata until the exact chunk is written or embedded.
#[derive(Debug, Default)]
pub(crate) struct GitHistoryBuildPlan {
    documents: Vec<GitHistoryDocument>,
    revisions: HistoryRevisionPool,
    objects: HistoryObjectPool,
    paths: Vec<Arc<str>>,
    path_ids: HashMap<Arc<str>, u32>,
    chunks: HistoryChunkIndex,
    removed_document_ids: BTreeSet<String>,
}

impl GitHistoryBuildPlan {
    fn with_chunk_index_capacity(chunk_capacity: usize) -> Self {
        Self {
            chunks: HistoryChunkIndex::with_capacity(chunk_capacity),
            ..Self::default()
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.chunks.len()
    }

    fn contains_history_chunk(&self, key: &HistoryChunkKey) -> bool {
        self.chunks.contains_key(key, &self.documents)
    }

    fn insert_history_chunk(
        &mut self,
        key: HistoryChunkKey,
        chunk: GitHistoryChunk,
    ) -> Option<GitHistoryChunk> {
        self.chunks.insert(key, chunk, &self.documents)
    }

    fn retain_history_chunks(
        &mut self,
        keep: impl FnMut(&ContentDigest, &mut GitHistoryChunk) -> bool,
    ) {
        self.chunks.retain(keep, &self.documents);
    }

    fn reserve_chunk_index_for_candidates(&mut self, candidate_chunks: usize) {
        let target = chunk_index_reserve_target(candidate_chunks);
        if target > self.chunks.capacity() {
            self.chunks
                .reserve(target.saturating_sub(self.chunks.len()));
        }
    }

    pub(crate) fn removed_document_ids(&self) -> impl Iterator<Item = &str> {
        self.removed_document_ids.iter().map(String::as_str)
    }

    fn revision_id(&mut self, revision: gix::ObjectId) -> u32 {
        self.revisions.intern(revision)
    }

    fn object(&self, object_id: u32) -> Result<gix::ObjectId, GitHistoryError> {
        self.objects.get(object_id).ok_or_else(|| {
            GitHistoryError::Invalid("history document references an unknown object".into())
        })
    }

    fn reconcile_documents(
        &mut self,
        source_index: usize,
        commits: &CommitAdmissions,
    ) -> Result<HistoryReconciliation, GitHistoryError> {
        let source_index = u32::try_from(source_index)
            .map_err(|_| GitHistoryError::Invalid("too many Git history sources".into()))?;
        let removed = self
            .documents
            .iter()
            .enumerate()
            .filter_map(|(index, document)| {
                if document.source_index != source_index {
                    return None;
                }
                let Some(revision) = self.revisions.get(document.revision) else {
                    return Some(Err(GitHistoryError::Invalid(
                        "history document references an unknown revision".into(),
                    )));
                };
                let keep = commits.is_reachable(&revision)
                    && (document.source_kind() != SourceKind::GitCommit
                        || commits.message_is_admitted(&revision));
                (!keep).then(|| Ok(u32::try_from(index).expect("document index fits in u32")))
            })
            .collect::<Result<HashSet<_>, GitHistoryError>>()?;
        self.retain_history_chunks(|_, chunk| !removed.contains(&chunk.document));
        let reused_commit_messages = self
            .documents
            .iter()
            .enumerate()
            .filter(|(index, document)| {
                document.source_index == source_index
                    && document.source_kind() == SourceKind::GitCommit
                    && !removed
                        .contains(&u32::try_from(*index).expect("document index fits in u32"))
            })
            .count();
        let removed_commit_messages = removed
            .iter()
            .filter(|index| self.documents[**index as usize].source_kind() == SourceKind::GitCommit)
            .count();
        let removed_blob_documents = removed
            .iter()
            .filter(|index| self.documents[**index as usize].source_kind() == SourceKind::GitBlob)
            .count();
        Ok(HistoryReconciliation {
            removed_documents: removed.len(),
            removed_blob_documents,
            removed_commit_messages,
            reused_commit_messages,
        })
    }

    fn push_document(
        &mut self,
        source_index: usize,
        revision: gix::ObjectId,
        object: gix::ObjectId,
        source_kind: SourceKind,
        path: &str,
    ) -> Result<u32, GitHistoryError> {
        let index = u32::try_from(self.documents.len())
            .map_err(|_| GitHistoryError::Invalid("too many Git history documents".into()))?;
        let source_index = u32::try_from(source_index)
            .map_err(|_| GitHistoryError::Invalid("too many Git history sources".into()))?;
        let revision = self.revision_id(revision);
        let object = self.objects.intern(object);
        let path_id = if let Some(&path_id) = self.path_ids.get(path) {
            path_id
        } else {
            let path_id = u32::try_from(self.paths.len())
                .map_err(|_| GitHistoryError::Invalid("too many Git history paths".into()))?;
            let path: Arc<str> = Arc::from(path);
            self.paths.push(Arc::clone(&path));
            self.path_ids.insert(path, path_id);
            path_id
        };
        let path = GitHistoryDocument::packed_path(path_id, source_kind)?;
        self.documents.push(GitHistoryDocument {
            source_index,
            revision,
            object,
            path,
        });
        Ok(index)
    }

    fn compact(mut self) -> Self {
        let mut remap = vec![None; self.documents.len()];
        for chunk in self.chunks.values() {
            remap[chunk.document as usize] = Some(0);
        }
        for (next, mapped) in remap
            .iter_mut()
            .filter(|mapped| mapped.is_some())
            .enumerate()
        {
            *mapped = Some(
                u32::try_from(next).expect("selected history document index already fits in u32"),
            );
        }
        let mut old_index = 0_usize;
        self.documents.retain(|_| {
            let keep = remap[old_index].is_some();
            old_index += 1;
            keep
        });
        for chunk in self.chunks.values_mut() {
            chunk.document = remap[chunk.document as usize]
                .expect("history chunk references a retained document");
        }

        self.revisions
            .compact(&mut self.documents, &mut self.chunks);
        self.objects.compact(&mut self.documents);

        let mut path_remap = vec![None; self.paths.len()];
        for document in &self.documents {
            path_remap[document.path_id() as usize] = Some(0);
        }
        for (next, mapped) in path_remap
            .iter_mut()
            .filter(|mapped| mapped.is_some())
            .enumerate()
        {
            *mapped = Some(u32::try_from(next).expect("selected history path index fits in u32"));
        }
        for document in &mut self.documents {
            let kind_bit = document.path & HISTORY_DOCUMENT_COMMIT_PATH_BIT;
            document.path = path_remap[document.path_id() as usize]
                .expect("history document references a retained path")
                | kind_bit;
        }
        let mut old_path = 0_usize;
        self.paths.retain(|_| {
            let keep = path_remap[old_path].is_some();
            old_path += 1;
            keep
        });
        self.path_ids.clear();
        for (index, path) in self.paths.iter().enumerate() {
            self.path_ids.insert(
                Arc::clone(path),
                u32::try_from(index).expect("selected history path index fits in u32"),
            );
        }
        self
    }

    #[allow(clippy::too_many_lines)]
    fn from_chunks(
        sources: &[GitCorpusSource],
        chunks: impl IntoIterator<Item = Chunk>,
        chunk_capacity: usize,
    ) -> Result<(Self, CachedGitHistory), GitHistoryError> {
        let source_indices = sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.repository_id.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut repositories = HashMap::<usize, gix::Repository>::new();
        let mut documents = HashMap::<DocumentId, u32>::new();
        let mut plan = Self::with_chunk_index_capacity(chunk_capacity);
        let mut cached_history = CachedGitHistory::default();
        for chunk in chunks {
            if !chunk.source_kind.is_git() {
                continue;
            }
            let Some(&source_index) = source_indices.get(&chunk.repository) else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(entry) =
                repositories.entry(source_index)
            {
                let repository = gix::open(&sources[source_index].repository_path)
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                entry.insert(repository);
            }
            let revision = gix::ObjectId::from_hex(chunk.revision.as_str().as_bytes())
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let cache_key = chunk.document_id.clone();
            let document = if let Some(&document) = documents.get(&cache_key) {
                document
            } else {
                let object = if chunk.source_kind == SourceKind::GitBlob {
                    let repository = repositories
                        .get(&source_index)
                        .expect("repository inserted above");
                    if !repository.has_object(revision) {
                        continue;
                    }
                    let commit = repository
                        .find_commit(revision)
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                    let tree = commit
                        .tree()
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                    let entry = tree
                        .lookup_entry_by_path(&chunk.path)
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?
                        .ok_or_else(|| {
                            GitHistoryError::Invalid(format!(
                                "cached history path is absent: {}@{}:{}",
                                chunk.repository, chunk.revision, chunk.path
                            ))
                        })?;
                    if !entry.mode().is_blob() {
                        return Err(GitHistoryError::Invalid(format!(
                            "cached history path is not a blob: {}@{}:{}",
                            chunk.repository, chunk.revision, chunk.path
                        )));
                    }
                    entry.object_id()
                } else {
                    revision
                };
                let document = plan.push_document(
                    source_index,
                    revision,
                    object,
                    chunk.source_kind,
                    &chunk.path,
                )?;
                documents.insert(cache_key, document);
                document
            };
            let descriptor = &plan.documents[document as usize];
            let object = plan
                .objects
                .get(descriptor.object)
                .expect("history document references an object");
            if chunk.source_kind == SourceKind::GitBlob {
                cached_history.observe(CachedGitHistoryChunk {
                    document_id: chunk.document_id.as_str(),
                    repository: chunk.repository.as_str(),
                    revision: chunk.revision.as_str(),
                    path: &chunk.path,
                    ordinal: chunk.ordinal,
                    has_previous: chunk.previous_chunk.is_some(),
                    has_next: chunk.next_chunk.is_some(),
                    blob: Some(object),
                    source_kind: chunk.source_kind,
                    content_key: Some(
                        history_content_key_for_chunk(&chunk)
                            .expect("filtered Git-history chunk has a content key"),
                    ),
                });
            }
            let key = HistoryChunkKey {
                content: history_content_key_for_chunk(&chunk)
                    .expect("filtered Git-history chunk has a content key"),
                revision: plan.revision_id(revision),
            };
            plan.insert_history_chunk(
                key,
                GitHistoryChunk {
                    document,
                    ordinal: chunk.ordinal,
                },
            );
        }
        plan.chunks.freeze_seed_lookup();
        Ok((plan, cached_history))
    }

    pub(crate) fn try_for_each_chunk(
        &self,
        sources: &[GitCorpusSource],
        mut visit: impl FnMut(Chunk, gix::ObjectId),
    ) -> Result<(), GitHistoryError> {
        let mut materialization_error = None;
        self.try_for_each_history_view(sources, |view, object| {
            if materialization_error.is_some() {
                return;
            }
            match view.into_chunk() {
                Ok(chunk) => visit(chunk, object),
                Err(error) => materialization_error = Some(error),
            }
        })?;
        materialization_error.map_or(Ok(()), |error| {
            Err(GitHistoryError::Chunking(error.to_string()))
        })
    }

    #[allow(clippy::too_many_lines)] // One streaming pass shares repository and blob state.
    pub(crate) fn try_for_each_history_view(
        &self,
        sources: &[GitCorpusSource],
        mut visit: impl FnMut(GitHistoryChunkView<'_>, gix::ObjectId),
    ) -> Result<(), GitHistoryError> {
        let mut repositories = HashMap::<usize, gix::Repository>::new();
        let mut selected = self.chunks.entries.iter().peekable();
        while let Some(entry) = selected.peek() {
            let document_index = entry.1.document;
            let descriptor = self.documents.get(document_index as usize).ok_or_else(|| {
                GitHistoryError::Invalid("history chunk references an unknown document".into())
            })?;
            let path = self
                .paths
                .get(descriptor.path_id() as usize)
                .ok_or_else(|| {
                    GitHistoryError::Invalid("history document references an unknown path".into())
                })?;
            let source = sources
                .get(descriptor.source_index as usize)
                .ok_or_else(|| {
                    GitHistoryError::Invalid("history document references an unknown source".into())
                })?;
            if let std::collections::hash_map::Entry::Vacant(entry) =
                repositories.entry(descriptor.source_index as usize)
            {
                let repository = gix::open(&source.repository_path)
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                entry.insert(repository);
            }
            let repository = repositories
                .get(&(descriptor.source_index as usize))
                .expect("repository inserted above");
            let revision_id = self.revisions.get(descriptor.revision).ok_or_else(|| {
                GitHistoryError::Invalid("history document references an unknown revision".into())
            })?;
            let revision = Revision::try_from(revision_id.to_string())
                .map_err(|error| GitHistoryError::Invalid(error.to_string()))?;
            let object = self.object(descriptor.object)?;
            let document = match descriptor.source_kind() {
                SourceKind::GitBlob => {
                    let size = repository
                        .find_header(object)
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?
                        .size();
                    let candidate = Candidate {
                        path: path.to_string(),
                        id: object,
                        size,
                    };
                    let mut hydration = GitIngestionObservations::default();
                    let blob = load_git_blob(
                        repository,
                        &candidate,
                        source.policy.limits,
                        &mut hydration,
                        false,
                    )?;
                    if let CachedGitBlob::Rejected { code, message } = &blob {
                        return Err(GitHistoryError::Invalid(format!(
                            "pinned Git blob {}:{} became {code}: {message}",
                            source.repository_id, path
                        )));
                    }
                    document_from_git_blob(
                        path,
                        &source.repository_id,
                        &revision,
                        source.trust_tier,
                        &source.license,
                        blob,
                    )?
                }
                SourceKind::GitCommit => {
                    let commit = repository
                        .find_commit(object)
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                    document_from_git_commit(
                        &commit,
                        &source.repository_id,
                        source.trust_tier,
                        &source.license,
                        source.policy.limits.max_file_bytes(),
                    )?
                    .ok_or_else(|| {
                        GitHistoryError::Invalid(format!(
                            "pinned Git commit message {object} became unavailable"
                        ))
                    })?
                }
                _ => {
                    return Err(GitHistoryError::Invalid(
                        "history plan contains a non-Git source".into(),
                    ));
                }
            };
            let drafts = chunk_document_drafts(&document, ChunkingConfig::default())
                .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
            while selected
                .peek()
                .is_some_and(|entry| entry.1.document == document_index)
            {
                let entry = selected.next().expect("history entry peeked above");
                let expected_content = self.chunks.contents.get(entry.0).ok_or_else(|| {
                    GitHistoryError::Invalid("history chunk references unknown content".into())
                })?;
                let selection = &entry.1;
                let index = selection.ordinal as usize;
                if index >= drafts.len() {
                    return Err(GitHistoryError::Invalid(format!(
                        "pinned Git chunk ordinal {} is unavailable for {}:{}",
                        selection.ordinal, source.repository_id, path
                    )));
                }
                let draft = drafts.get(index);
                debug_assert_eq!(draft.ordinal(), selection.ordinal);
                let identifiers = identifier_values(&document.path, draft.text());
                let identifier_refs = identifiers
                    .iter()
                    .map(compact_str::CompactString::as_str)
                    .collect::<Vec<_>>();
                let embedding_key = embedding_text_digest_from_parts(
                    &document.title,
                    draft.headings().iter().copied(),
                    &identifier_refs,
                    &document.tags,
                    draft.text(),
                );
                let actual_key =
                    history_content_key(&source.repository_id, &document.path, &embedding_key);
                let actual_revision =
                    gix::ObjectId::from_hex(document.revision.as_str().as_bytes())
                        .map_err(|error| GitHistoryError::Invalid(error.to_string()))?;
                let expected_revision = self
                    .documents
                    .get(selection.document as usize)
                    .and_then(|document| self.revisions.get(document.revision))
                    .ok_or_else(|| {
                        GitHistoryError::Invalid(
                            "history chunk references an unknown revision".into(),
                        )
                    })?;
                if actual_key != *expected_content || actual_revision != expected_revision {
                    return Err(GitHistoryError::Invalid(format!(
                        "pinned Git chunk identity changed for {}:{}#{}",
                        source.repository_id, path, selection.ordinal
                    )));
                }
                let view = GitHistoryChunkView::new(&drafts, index, &document, identifiers);
                visit(view, object);
            }
        }
        Ok(())
    }

    fn into_chunks(
        self,
        sources: &[GitCorpusSource],
        observations: &mut GitHistoryRefreshObservations,
        spare_capacity: usize,
    ) -> Result<Vec<Chunk>, GitHistoryError> {
        let mut chunks = Vec::with_capacity(self.len().saturating_add(spare_capacity));
        self.try_for_each_chunk(sources, |chunk, _blob| {
            observations.materialized_identifier_count_histogram[chunk.identifiers.len()] =
                observations.materialized_identifier_count_histogram[chunk.identifiers.len()]
                    .saturating_add(1);
            observations.materialized_chunks = observations.materialized_chunks.saturating_add(1);
            chunks.push(chunk);
        })?;
        for chunk in &mut chunks {
            chunk
                .attach_derived_resource_uri()
                .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
        }
        chunks.sort_unstable_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        Ok(chunks)
    }
}

type PreviousContentLookup<'a> = dyn FnMut(&ContentDigest, &gix::ObjectId, &BTreeSet<String>) -> Result<bool, GitHistoryError>
    + 'a;

enum HistoryContents<'a> {
    All(GitHistoryBuildPlan),
    Delta {
        previous_contains: &'a mut PreviousContentLookup<'a>,
        plan: GitHistoryBuildPlan,
    },
}

impl<'a> HistoryContents<'a> {
    fn delta(previous_contains: &'a mut PreviousContentLookup<'a>, cached_chunks: usize) -> Self {
        Self::Delta {
            previous_contains,
            plan: GitHistoryBuildPlan::with_chunk_index_capacity(delta_chunk_index_capacity(
                cached_chunks,
            )),
        }
    }

    fn reconcile_documents(
        &mut self,
        cached_history: &mut CachedGitHistory,
        source_index: usize,
        source: &GitCorpusSource,
        commits: &CommitAdmissions,
    ) -> Result<HistoryReconciliation, GitHistoryError> {
        let (removed, cached) = cached_history.reconcile_documents(source, commits)?;
        match self {
            Self::All(plan) => plan.reconcile_documents(source_index, commits),
            Self::Delta { plan, .. } => {
                plan.removed_document_ids.extend(removed);
                Ok(cached)
            }
        }
    }

    fn insert_draft(
        &mut self,
        content: ContentDigest,
        ordinal: u32,
        locator: GitHistoryDocumentLocator<'_>,
        document: &mut Option<u32>,
        observations: &mut GitHistoryRefreshObservations,
    ) -> Result<bool, GitHistoryError> {
        let revision = match self {
            Self::All(plan) | Self::Delta { plan, .. } => plan.revision_id(locator.revision),
        };
        let key = HistoryChunkKey { content, revision };
        let inserted = match self {
            Self::All(plan) => {
                if plan.contains_history_chunk(&key) {
                    observations.reused_contents = observations.reused_contents.saturating_add(1);
                    false
                } else {
                    plan.reserve_chunk_index_for_candidates(plan.chunks.len().saturating_add(1));
                    let document = selected_document(plan, document, locator)?;
                    plan.insert_history_chunk(key, GitHistoryChunk { document, ordinal });
                    true
                }
            }
            Self::Delta {
                previous_contains,
                plan,
            } => {
                if plan.contains_history_chunk(&key)
                    || previous_contains(
                        &key.content,
                        &locator.revision,
                        &plan.removed_document_ids,
                    )?
                {
                    observations.reused_contents = observations.reused_contents.saturating_add(1);
                    false
                } else {
                    plan.reserve_chunk_index_for_candidates(plan.chunks.len().saturating_add(1));
                    let document = selected_document(plan, document, locator)?;
                    plan.insert_history_chunk(key, GitHistoryChunk { document, ordinal });
                    true
                }
            }
        };
        Ok(inserted)
    }

    fn into_plan(self) -> GitHistoryBuildPlan {
        match self {
            Self::All(plan) | Self::Delta { plan, .. } => plan,
        }
        .compact()
    }
}

fn selected_document(
    plan: &mut GitHistoryBuildPlan,
    selected: &mut Option<u32>,
    locator: GitHistoryDocumentLocator<'_>,
) -> Result<u32, GitHistoryError> {
    if let Some(document) = *selected {
        return Ok(document);
    }
    let document = plan.push_document(
        locator.source_index,
        locator.revision,
        locator.object,
        locator.source_kind,
        locator.path,
    )?;
    *selected = Some(document);
    Ok(document)
}

/// Reconcile cached Git chunks against the currently reachable history.
///
/// Repository file-count and total-byte limits bound only blobs considered by
/// this refresh: the whole reachable history for a cold build, or the new
/// reachable delta for an incremental build. Cached documents that are no
/// longer reachable are discarded. The per-file limit is also checked whenever
/// an already-selected blob is hydrated from Git.
///
/// Returns `Ok(None)` only when the cached source set cannot seed the current
/// source contracts, allowing the caller to fall back to a cold rebuild.
///
/// # Errors
///
/// Returns [`GitHistoryError`] for invalid policies, missing Git objects,
/// tree-diff failures, invalid source content, or chunking failures.
pub fn ingest_git_history_fast_forward(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_chunks: &[Chunk],
) -> Result<Option<(Vec<Chunk>, GitHistoryRefreshObservations)>, GitHistoryError> {
    let chunk_capacity = cached_chunks.len();
    let Some((plan, mut observations)) = plan_git_history_fast_forward_from_chunks(
        sources,
        previous_tips,
        cached_chunks.iter().cloned(),
        chunk_capacity,
    )?
    else {
        return Ok(None);
    };
    let chunks = plan.into_chunks(sources, &mut observations, 0)?;
    Ok(Some((chunks, observations)))
}

pub(crate) fn plan_git_history_fast_forward_owned(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_chunks: Vec<Chunk>,
) -> Result<Option<(GitHistoryBuildPlan, GitHistoryRefreshObservations)>, GitHistoryError> {
    let chunk_capacity = cached_chunks.len();
    plan_git_history_fast_forward_from_chunks(sources, previous_tips, cached_chunks, chunk_capacity)
}

fn plan_git_history_fast_forward_from_chunks(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_chunks: impl IntoIterator<Item = Chunk>,
    chunk_capacity: usize,
) -> Result<Option<(GitHistoryBuildPlan, GitHistoryRefreshObservations)>, GitHistoryError> {
    let repositories = sources
        .iter()
        .map(|source| source.repository_id.clone())
        .collect::<BTreeSet<_>>();
    let chunks = cached_chunks.into_iter().filter(|chunk| {
        chunk.source_kind.is_git()
            && repositories.contains(&chunk.repository)
            && previous_tips
                .iter()
                .any(|tip| tip.repository == chunk.repository)
    });
    let (plan, cached_history) = GitHistoryBuildPlan::from_chunks(sources, chunks, chunk_capacity)?;
    ingest_git_history_fast_forward_with_contents(
        sources,
        previous_tips,
        cached_history,
        HistoryContents::All(plan),
    )
}

pub(crate) fn plan_git_history_fast_forward_delta(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_history: CachedGitHistory,
    previous_contains: &mut PreviousContentLookup<'_>,
) -> Result<Option<(GitHistoryBuildPlan, GitHistoryRefreshObservations)>, GitHistoryError> {
    let cached_chunks = cached_history.chunk_count();
    ingest_git_history_fast_forward_with_contents(
        sources,
        previous_tips,
        cached_history,
        HistoryContents::delta(previous_contains, cached_chunks),
    )
}

#[allow(clippy::too_many_lines)] // One streaming pass shares per-source Git caches and budgets.
fn ingest_git_history_fast_forward_with_contents(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    mut cached_history: CachedGitHistory,
    mut contents: HistoryContents<'_>,
) -> Result<Option<(GitHistoryBuildPlan, GitHistoryRefreshObservations)>, GitHistoryError> {
    let Some(tips) = validated_history_tips(sources, previous_tips) else {
        return Ok(None);
    };
    let mut observations = GitHistoryRefreshObservations::default();
    let mut ordered_sources = sources.iter().enumerate().collect::<Vec<_>>();
    ordered_sources.sort_by(|(_, left), (_, right)| {
        tips.contains_key(&right.repository_id)
            .cmp(&tips.contains_key(&left.repository_id))
            .then_with(|| left.repository_id.cmp(&right.repository_id))
    });
    let mut processed = Vec::<ProcessedHistory<'_>>::new();
    for (source_index, source) in ordered_sources {
        let Some(SourceHistory {
            repository: repo,
            current_ids,
            previous_ids,
        }) = source_history(source, &tips)?
        else {
            return Ok(None);
        };
        let known_history = processed
            .iter()
            .position(|known| same_corpus_contract(source, known.source));
        if let Some(reachable) = known_history
            .and_then(|index| processed[index].represented_tips.get(&current_ids))
            .copied()
        {
            observations.reachable_commits =
                observations.reachable_commits.saturating_add(reachable);
            observations.reused_commits = observations.reused_commits.saturating_add(reachable);
            continue;
        }
        let reusable_blobs = known_history.map(|index| &processed[index].reusable_blobs);
        let mut commits = select_commit_messages(&repo, &current_ids, source.policy.limits)?;
        let source_reachable = commits.len();
        observations.reachable_commits =
            observations.reachable_commits.saturating_add(commits.len());
        let mut resource_cache = None;
        let reconciliation =
            contents.reconcile_documents(&mut cached_history, source_index, source, &commits)?;
        observations.reused_commit_messages = observations
            .reused_commit_messages
            .saturating_add(reconciliation.reused_commit_messages);
        observations.removed_documents = observations
            .removed_documents
            .saturating_add(reconciliation.removed_documents);
        observations.removed_commit_messages = observations
            .removed_commit_messages
            .saturating_add(reconciliation.removed_commit_messages);
        let reachable_previous_ids = previous_ids
            .iter()
            .copied()
            .filter(|revision| commits.is_reachable(revision))
            .collect::<Vec<_>>();
        commits.mark_retained(&repo, &reachable_previous_ids)?;
        let retained_commits = commits.retained_count();
        observations.reused_commits = observations.reused_commits.saturating_add(retained_commits);
        observations.ingested_commits = observations
            .ingested_commits
            .saturating_add(source_reachable.saturating_sub(retained_commits));
        let walk = if reachable_previous_ids.is_empty() || reconciliation.removed_blob_documents > 0
        {
            repo.rev_walk(current_ids.iter().copied())
        } else {
            repo.rev_walk(current_ids.iter().copied())
                .with_hidden(reachable_previous_ids.iter().copied())
        };
        let walk = walk
            .all()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let mut seen_blobs = cached_history.take_reusable_blobs(source, &repo)?;
        let mut revision_is_reachable = |revision| Ok(commits.is_reachable(&revision));
        let mut budget = GitCorpusBudget::new(source.policy.limits);
        let reservation_ids = if previous_ids.is_empty() {
            current_ids.clone()
        } else {
            current_ids
                .iter()
                .filter(|id| !previous_ids.contains(id))
                .copied()
                .collect()
        };
        let tip_admissions = if reservation_ids.is_empty() {
            None
        } else {
            Some(reserve_tip_blobs(
                &repo,
                source,
                &reservation_ids,
                &reachable_previous_ids,
                &seen_blobs,
                reusable_blobs,
                &mut revision_is_reachable,
                &mut budget,
            )?)
        };
        let mut commits_to_ingest = Vec::new();
        for info in walk {
            let info = info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
            #[cfg(feature = "coz-profile")]
            crate::profile_progress!("git_history_walk_commit");
            let commit = info
                .object()
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let commit = ReachableCommit {
                id: info.id,
                first_parent: commit.parent_ids().next().map(gix::Id::detach),
                message_admitted: commits.message_is_admitted(&info.id),
            };
            commits_to_ingest.push(commit);
        }
        commits_to_ingest.reverse();
        for commit in &commits_to_ingest {
            if resource_cache.is_none() {
                resource_cache = Some(
                    repo.diff_resource_cache_for_tree_diff()
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?,
                );
            }
            ingest_commit_changes(
                &repo,
                source_index,
                source,
                commit,
                resource_cache
                    .as_mut()
                    .expect("resource cache initialized above"),
                &mut seen_blobs,
                reusable_blobs,
                &mut revision_is_reachable,
                &mut budget,
                tip_admissions.as_ref(),
                &mut contents,
                &mut observations,
            )?;
            #[cfg(feature = "coz-profile")]
            crate::profile_progress!("git_history_ingested_commit");
        }
        if let Some(index) = known_history {
            let known = &mut processed[index];
            known.represented_tips.insert(current_ids, source_reachable);
            known.reusable_blobs.merge_reusable(seen_blobs);
        } else {
            let mut reusable_blobs = HistoryBlobDeduper::default();
            reusable_blobs.merge_reusable(seen_blobs);
            processed.push(ProcessedHistory {
                source,
                represented_tips: HashMap::from([(current_ids, source_reachable)]),
                reusable_blobs,
            });
        }
    }
    Ok(Some((contents.into_plan(), observations)))
}

fn validated_history_tips(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
) -> Option<BTreeMap<RepositoryId, Vec<Revision>>> {
    let mut tips = BTreeMap::<RepositoryId, Vec<Revision>>::new();
    for tip in previous_tips {
        tips.entry(tip.repository.clone())
            .or_default()
            .push(tip.revision.clone());
    }
    for revisions in tips.values_mut() {
        revisions.sort();
        revisions.dedup();
    }
    let repositories = sources
        .iter()
        .map(|source| source.repository_id.clone())
        .collect::<BTreeSet<_>>();
    (repositories.len() == sources.len()
        && tips
            .keys()
            .all(|repository| repositories.contains(repository)))
    .then_some(tips)
}

fn source_history(
    source: &GitCorpusSource,
    tips: &BTreeMap<RepositoryId, Vec<Revision>>,
) -> Result<Option<SourceHistory>, GitHistoryError> {
    validate_policy(&source.policy)?;
    let repository = gix::open(&source.repository_path)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut current_ids = source
        .history_tips
        .iter()
        .map(|revision| {
            gix::ObjectId::from_hex(revision.as_str().as_bytes())
                .map_err(|error| GitHistoryError::Git(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    current_ids.sort_unstable();
    current_ids.dedup();
    if current_ids.is_empty() {
        return Err(GitHistoryError::Invalid(
            "Git history source has no current tips".into(),
        ));
    }
    let selected_id = gix::ObjectId::from_hex(source.revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    if !current_ids.contains(&selected_id) {
        return Err(GitHistoryError::Invalid(
            "Git history source revision is not one of its current tips".into(),
        ));
    }
    let previous_ids = tips
        .get(&source.repository_id)
        .into_iter()
        .flatten()
        .map(|revision| {
            gix::ObjectId::from_hex(revision.as_str().as_bytes())
                .map_err(|error| GitHistoryError::Git(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(SourceHistory {
        repository,
        current_ids,
        previous_ids,
    }))
}

fn same_corpus_contract(left: &GitCorpusSource, right: &GitCorpusSource) -> bool {
    left.trust_tier == right.trust_tier
        && left.license == right.license
        && left.policy == right.policy
}

fn select_commit_messages(
    repo: &gix::Repository,
    tips: &[gix::ObjectId],
    limits: super::git::GitCorpusLimits,
) -> Result<CommitAdmissions, GitHistoryError> {
    let mut walk = repo
        .rev_walk(tips.iter().copied())
        .all()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut budget = GitCorpusBudget::new(limits);
    let mut revisions = HashMap::new();
    for info in &mut walk {
        let info = info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
        #[cfg(feature = "coz-profile")]
        crate::profile_progress!("git_history_walk_commit");
        let commit = info
            .object()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let admitted = commit_message_size(&commit, limits.max_file_bytes())
            .is_some_and(|size| budget.reserve(size).is_ok());
        revisions.insert(
            info.id,
            CommitAdmission {
                message_admitted: admitted,
                retained: false,
            },
        );
    }
    Ok(CommitAdmissions { revisions })
}

#[allow(clippy::too_many_arguments)]
fn reserve_tip_blobs(
    repo: &gix::Repository,
    source: &GitCorpusSource,
    current_ids: &[gix::ObjectId],
    previous_ids: &[gix::ObjectId],
    seen_blobs: &HistoryBlobDeduper,
    reusable_blobs: Option<&HistoryBlobDeduper>,
    revision_is_reachable: &mut dyn FnMut(gix::ObjectId) -> Result<bool, GitHistoryError>,
    budget: &mut GitCorpusBudget,
) -> Result<TipAdmissions, GitHistoryError> {
    let mut admissions = TipAdmissions {
        blobs: HashMap::new(),
    };
    if !previous_ids.is_empty() {
        reserve_incremental_tip_blobs(
            repo,
            source,
            current_ids,
            previous_ids,
            seen_blobs,
            reusable_blobs,
            revision_is_reachable,
            budget,
            &mut admissions,
        )?;
        return Ok(admissions);
    }
    let mut trees = Vec::new();
    for current_id in current_ids {
        let current = repo
            .find_commit(*current_id)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?
            .tree()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        trees.push((String::new(), current.id));
    }
    let mut seen_trees = HashSet::new();
    while let Some((prefix, tree_id)) = trees.pop() {
        if !seen_trees.insert((prefix.clone(), tree_id)) {
            continue;
        }
        let tree = repo
            .find_tree(tree_id)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        for entry in tree.iter() {
            let entry = entry.map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let filename = entry.filename().to_str_lossy();
            let path = join_tree_path(&prefix, filename.as_ref());
            let id = entry.object_id();
            if entry.mode().is_tree() {
                trees.push((path, id));
                continue;
            }
            if !entry.mode().is_blob() || !is_selected(&path, &source.policy) {
                continue;
            }
            reserve_tip_blob(
                repo,
                &path,
                id,
                seen_blobs,
                reusable_blobs,
                revision_is_reachable,
                budget,
                &mut admissions,
            )?;
        }
    }
    Ok(admissions)
}

#[allow(clippy::too_many_arguments)]
fn reserve_incremental_tip_blobs(
    repo: &gix::Repository,
    source: &GitCorpusSource,
    current_ids: &[gix::ObjectId],
    previous_ids: &[gix::ObjectId],
    seen_blobs: &HistoryBlobDeduper,
    reusable_blobs: Option<&HistoryBlobDeduper>,
    revision_is_reachable: &mut dyn FnMut(gix::ObjectId) -> Result<bool, GitHistoryError>,
    budget: &mut GitCorpusBudget,
    admissions: &mut TipAdmissions,
) -> Result<(), GitHistoryError> {
    let mut resource_cache = repo
        .diff_resource_cache_for_tree_diff()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let commit_graph = repo
        .commit_graph_if_enabled()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut graph = repo.revision_graph(commit_graph.as_ref());
    for current_id in current_ids {
        let baseline =
            match nearest_previous_merge_base(repo, *current_id, previous_ids, &mut graph)? {
                Some(baseline_id) => repo
                    .find_commit(baseline_id)
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?
                    .tree()
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?,
                None => repo.empty_tree(),
            };
        let current = repo
            .find_commit(*current_id)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?
            .tree()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let mut callback_error = None;
        let diff = baseline
            .changes()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?
            .options(|options| {
                options.track_path();
                options.track_rewrites(None);
            })
            .for_each_to_obtain_tree_with_cache(&current, &mut resource_cache, |change| {
                let Some((path, id)) = selected_change(change, &source.policy) else {
                    return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()));
                };
                if let Err(error) = reserve_tip_blob(
                    repo,
                    &path,
                    id,
                    seen_blobs,
                    reusable_blobs,
                    revision_is_reachable,
                    budget,
                    admissions,
                ) {
                    callback_error = Some(error);
                    return Ok(std::ops::ControlFlow::Break(()));
                }
                Ok(std::ops::ControlFlow::Continue(()))
            });
        resource_cache.clear_resource_cache_keep_allocation();
        diff.map_err(|error| GitHistoryError::Git(error.to_string()))?;
        if let Some(error) = callback_error {
            return Err(error);
        }
    }
    Ok(())
}

fn nearest_previous_merge_base(
    repo: &gix::Repository,
    current: gix::ObjectId,
    previous_ids: &[gix::ObjectId],
    graph: &mut gix::revwalk::Graph<
        '_,
        '_,
        gix::revwalk::graph::Commit<gix::revision::plumbing::merge_base::Flags>,
    >,
) -> Result<Option<gix::ObjectId>, GitHistoryError> {
    let mut merge_bases = HashSet::new();
    for previous in previous_ids {
        match repo.merge_base_with_graph(current, *previous, graph) {
            Ok(base) => {
                merge_bases.insert(base.detach());
            }
            Err(gix::repository::merge_base_with_graph::Error::NotFound { .. }) => {}
            Err(error) => return Err(GitHistoryError::Git(error.to_string())),
        }
    }
    if merge_bases.len() == 1 {
        return Ok(merge_bases.into_iter().next());
    }
    let walk = repo
        .rev_walk([current])
        .all()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    for info in walk {
        let info = info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
        if merge_bases.contains(&info.id) {
            return Ok(Some(info.id));
        }
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn reserve_tip_blob(
    repo: &gix::Repository,
    path: &str,
    id: gix::ObjectId,
    seen_blobs: &HistoryBlobDeduper,
    reusable_blobs: Option<&HistoryBlobDeduper>,
    revision_is_reachable: &mut dyn FnMut(gix::ObjectId) -> Result<bool, GitHistoryError>,
    budget: &mut GitCorpusBudget,
    admissions: &mut TipAdmissions,
) -> Result<(), GitHistoryError> {
    if seen_blobs.contains(path, id)
        || admissions
            .blobs
            .get(path)
            .is_some_and(|ids| ids.contains(&id))
        || has_reachable_reusable_blob(reusable_blobs, path, id, revision_is_reachable)?
    {
        return Ok(());
    }
    let size = repo
        .find_header(id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?
        .size();
    if budget.reserve(size).is_ok() {
        admissions
            .blobs
            .entry(path.to_owned())
            .or_default()
            .push(id);
    }
    Ok(())
}

fn has_reachable_reusable_blob(
    reusable_blobs: Option<&HistoryBlobDeduper>,
    path: &str,
    id: gix::ObjectId,
    revision_is_reachable: &mut dyn FnMut(gix::ObjectId) -> Result<bool, GitHistoryError>,
) -> Result<bool, GitHistoryError> {
    let Some(blobs) = reusable_blobs else {
        return Ok(false);
    };
    for revision in blobs.reusable_revisions(path, id) {
        if revision_is_reachable(revision)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn ingest_commit_changes(
    repo: &gix::Repository,
    source_index: usize,
    source: &GitCorpusSource,
    commit: &ReachableCommit,
    resource_cache: &mut gix::diff::blob::Platform,
    seen_blobs: &mut HistoryBlobDeduper,
    reusable_blobs: Option<&HistoryBlobDeduper>,
    revision_is_reachable: &mut dyn FnMut(gix::ObjectId) -> Result<bool, GitHistoryError>,
    budget: &mut GitCorpusBudget,
    tip_admissions: Option<&TipAdmissions>,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<CommitCoverage, GitHistoryError> {
    let current_commit = repo
        .find_commit(commit.id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let revision = Revision::try_from(commit.id.to_string())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut coverage = ingest_commit_message(
        source_index,
        source,
        commit.id,
        &current_commit,
        commit.message_admitted,
        contents,
        observations,
    )?;
    let current = current_commit
        .tree()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let previous = commit_parent_tree(repo, commit.first_parent)?;
    let mut pending = Vec::new();
    let mut callback_error = None;
    let diff = previous
        .changes()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?
        .options(|options| {
            options.track_path();
            options.track_rewrites(None);
        })
        .for_each_to_obtain_tree_with_cache(&current, resource_cache, |change| {
            let Some((path, id)) = selected_change(change, &source.policy) else {
                return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()));
            };
            coverage.selected = true;
            if seen_blobs.contains(&path, id) {
                coverage.reused_from_prior = false;
                observations.reused_blobs = observations.reused_blobs.saturating_add(1);
                return Ok(std::ops::ControlFlow::Continue(()));
            }
            match has_reachable_reusable_blob(reusable_blobs, &path, id, revision_is_reachable) {
                Ok(true) => {
                    observations.reused_blobs = observations.reused_blobs.saturating_add(1);
                    return Ok(std::ops::ControlFlow::Continue(()));
                }
                Ok(false) => {}
                Err(error) => {
                    callback_error = Some(error);
                    return Ok(std::ops::ControlFlow::Break(()));
                }
            }
            coverage.reused_from_prior = false;
            let reserved_at_tip =
                tip_admissions.is_some_and(|admissions| admissions.is_admitted(&path, id));
            let size = match repo.find_header(id) {
                Ok(header) => header.size(),
                Err(error) => {
                    callback_error = Some(GitHistoryError::Git(error.to_string()));
                    return Ok(std::ops::ControlFlow::Break(()));
                }
            };
            if !reserved_at_tip && budget.reserve(size).is_err() {
                observations.budget_rejections = observations.budget_rejections.saturating_add(1);
                return Ok(std::ops::ControlFlow::Continue(()));
            }
            pending.push(PendingChange { path, id, size });
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        });
    resource_cache.clear_resource_cache_keep_allocation();
    diff.map_err(|error| GitHistoryError::Git(error.to_string()))?;
    if let Some(error) = callback_error {
        return Err(error);
    }
    pending.sort_by(|left, right| pending_path(left).cmp(pending_path(right)));

    for PendingChange { path, id, size } in pending {
        ingest_upsert(
            repo,
            source_index,
            source,
            &revision,
            &path,
            id,
            size,
            seen_blobs,
            contents,
            observations,
        )?;
    }
    ingest_reused_blob_occurrences(
        repo,
        source_index,
        source,
        &revision,
        current.id,
        seen_blobs,
        contents,
        observations,
        &mut coverage,
    )?;
    Ok(coverage)
}

#[allow(clippy::too_many_arguments)]
fn ingest_reused_blob_occurrences(
    repo: &gix::Repository,
    source_index: usize,
    source: &GitCorpusSource,
    revision: &Revision,
    tree_id: gix::ObjectId,
    seen_blobs: &mut HistoryBlobDeduper,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
    coverage: &mut CommitCoverage,
) -> Result<(), GitHistoryError> {
    let mut trees = vec![(String::new(), tree_id)];
    let revision_id = gix::ObjectId::from_hex(revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    while let Some((prefix, tree_id)) = trees.pop() {
        let tree = repo
            .find_tree(tree_id)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        for entry in tree.iter() {
            let entry = entry.map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let filename = entry.filename().to_str_lossy();
            let path = join_tree_path(&prefix, filename.as_ref());
            let id = entry.object_id();
            if entry.mode().is_tree() {
                trees.push((path, id));
                continue;
            }
            if !entry.mode().is_blob() || !is_selected(&path, &source.policy) {
                continue;
            }
            if !seen_blobs.contains(&path, id) {
                continue;
            }
            if seen_blobs.content_keys(&path, id).is_none() {
                let size = repo
                    .find_header(id)
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?
                    .size();
                ingest_upsert(
                    repo,
                    source_index,
                    source,
                    revision,
                    &path,
                    id,
                    size,
                    seen_blobs,
                    contents,
                    observations,
                )?;
            }
            coverage.selected = true;
            coverage.reused_from_prior = false;
            let locator = GitHistoryDocumentLocator {
                source_index,
                revision: revision_id,
                object: id,
                source_kind: SourceKind::GitBlob,
                path: &path,
            };
            let mut document_index = None;
            for (content, ordinal) in seen_blobs.content_keys(&path, id).unwrap_or(&[]) {
                contents.insert_draft(
                    content.clone(),
                    *ordinal,
                    locator,
                    &mut document_index,
                    observations,
                )?;
            }
        }
    }
    Ok(())
}

fn commit_parent_tree(
    repo: &gix::Repository,
    parent: Option<gix::ObjectId>,
) -> Result<gix::Tree<'_>, GitHistoryError> {
    parent.map_or_else(
        || Ok(repo.empty_tree()),
        |parent| {
            repo.find_commit(parent)
                .map_err(|error| GitHistoryError::Git(error.to_string()))?
                .tree()
                .map_err(|error| GitHistoryError::Git(error.to_string()))
        },
    )
}

fn ingest_commit_message(
    source_index: usize,
    source: &GitCorpusSource,
    commit_id: gix::ObjectId,
    commit: &gix::Commit<'_>,
    admitted: bool,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<CommitCoverage, GitHistoryError> {
    let mut coverage = CommitCoverage::default();
    let document = admitted
        .then(|| {
            document_from_git_commit(
                commit,
                &source.repository_id,
                source.trust_tier,
                &source.license,
                source.policy.limits.max_file_bytes(),
            )
        })
        .transpose()?
        .flatten();
    if let Some(document) = document {
        coverage.selected = true;
        let inserted = insert_document_drafts(
            source_index,
            source,
            &document,
            commit_id,
            contents,
            observations,
            None,
        )?;
        coverage.reused_from_prior &= !inserted;
        if inserted {
            observations.ingested_commit_messages =
                observations.ingested_commit_messages.saturating_add(1);
        } else {
            observations.reused_commit_messages =
                observations.reused_commit_messages.saturating_add(1);
        }
    } else {
        observations.rejected_commit_messages =
            observations.rejected_commit_messages.saturating_add(1);
    }
    Ok(coverage)
}

#[allow(clippy::too_many_arguments)]
fn ingest_upsert(
    repo: &gix::Repository,
    source_index: usize,
    source: &GitCorpusSource,
    revision: &Revision,
    path: &str,
    id: gix::ObjectId,
    size: u64,
    seen_blobs: &mut HistoryBlobDeduper,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<(), GitHistoryError> {
    let candidate = Candidate {
        path: path.to_string(),
        id,
        size,
    };
    // History search uses chunk-local identifiers below. Avoid building and
    // cloning a file-wide identifier set into every chunk only to overwrite it.
    let blob = load_git_blob(
        repo,
        &candidate,
        source.policy.limits,
        &mut observations.git,
        false,
    )?;
    if matches!(blob, CachedGitBlob::Rejected { .. }) {
        return Ok(());
    }
    observations.ingested_blobs = observations.ingested_blobs.saturating_add(1);
    let document = document_from_git_blob(
        path,
        &source.repository_id,
        revision,
        source.trust_tier,
        &source.license,
        blob,
    )?;
    let revision_id = gix::ObjectId::from_hex(document.revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let path_id = seen_blobs.path_id(path);
    let inserted = {
        let keys = seen_blobs.content_keys.entry((path_id, id)).or_default();
        insert_document_drafts(
            source_index,
            source,
            &document,
            id,
            contents,
            observations,
            Some(keys),
        )?
    };
    if inserted
        && seen_blobs
            .content_keys
            .get(&(path_id, id))
            .is_some_and(Vec::is_empty)
    {
        seen_blobs.content_keys.remove(&(path_id, id));
    }
    seen_blobs.insert_proof(path_id, id, revision_id, inserted);
    #[cfg(feature = "coz-profile")]
    crate::profile_progress!("git_history_ingested_blob");
    Ok(())
}

fn insert_document_drafts(
    source_index: usize,
    source: &GitCorpusSource,
    document: &super::NormalizedDocument,
    object: gix::ObjectId,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
    mut keys: Option<&mut Vec<(ContentDigest, u32)>>,
) -> Result<bool, GitHistoryError> {
    let revision_id = gix::ObjectId::from_hex(document.revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let locator = GitHistoryDocumentLocator {
        source_index,
        revision: revision_id,
        object,
        source_kind: document.source_kind,
        path: &document.path,
    };
    let mut document_index = None;
    let mut all_inserted = true;
    let mut saw_draft = false;
    let mut ingest_error = None;
    for_each_chunk_draft(document, ChunkingConfig::default(), |draft| {
        if ingest_error.is_some() {
            return;
        }
        saw_draft = true;
        let mut identifier_buffer = [""; MAX_IDENTIFIERS];
        let identifiers = identifier_refs(&document.path, draft.text(), &mut identifier_buffer);
        observations.candidate_chunks = observations.candidate_chunks.saturating_add(1);
        observations.candidate_identifier_count_histogram[identifiers.len()] =
            observations.candidate_identifier_count_histogram[identifiers.len()].saturating_add(1);
        let embedding_key = embedding_text_digest_from_parts(
            &document.title,
            draft.headings().iter().copied(),
            identifiers,
            &document.tags,
            draft.text(),
        );
        let key = history_content_key(&source.repository_id, &document.path, &embedding_key);
        if let Some(keys) = keys.as_mut() {
            keys.push((key.clone(), draft.ordinal()));
        }
        match contents.insert_draft(
            key,
            draft.ordinal(),
            locator,
            &mut document_index,
            observations,
        ) {
            Ok(inserted) => all_inserted &= inserted,
            Err(error) => ingest_error = Some(error),
        }
    })
    .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
    if let Some(error) = ingest_error {
        return Err(error);
    }
    Ok(saw_draft && all_inserted)
}

fn history_content_key(
    repository: &RepositoryId,
    path: &str,
    embedding_key: &ContentDigest,
) -> ContentDigest {
    let encoded_embedding_key = embedding_key.encoded();
    let mut digest = Sha256::new();
    digest.update(repository.as_str().as_bytes());
    digest.update([0]);
    digest.update(path.as_bytes());
    digest.update([0]);
    digest.update(encoded_embedding_key.as_bytes());
    ContentDigest::from_sha256(digest.finalize().into())
}

pub(crate) fn history_content_key_for_chunk(chunk: &Chunk) -> Option<ContentDigest> {
    chunk.source_kind.is_git().then(|| {
        history_content_key_for_fields(
            &chunk.repository,
            &chunk.path,
            &chunk.title,
            chunk.heading_path.iter().map(String::as_str),
            &chunk.identifiers,
            &chunk.tags,
            &chunk.text,
        )
    })
}

fn history_content_key_for_fields<'heading>(
    repository: &RepositoryId,
    path: &str,
    title: &str,
    headings: impl Iterator<Item = &'heading str> + Clone,
    identifiers: &[compact_str::CompactString],
    tags: &BTreeSet<String>,
    text: &str,
) -> ContentDigest {
    let embedding_key =
        embedding_text_digest_from_metadata(title, headings, identifiers, tags, text);
    history_content_key(repository, path, &embedding_key)
}

fn selected_change(
    change: gix::object::tree::diff::Change<'_, '_, '_>,
    policy: &GitCorpusPolicy,
) -> Option<(String, gix::ObjectId)> {
    use gix::object::tree::diff::Change;
    match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } if entry_mode.is_blob() => selected_upsert(location.to_string(), id.detach(), policy),
        Change::Modification {
            location,
            entry_mode,
            id,
            ..
        } if entry_mode.is_blob() => selected_upsert(location.to_string(), id.detach(), policy),
        _ => None,
    }
}

fn selected_upsert(
    path: String,
    id: gix::ObjectId,
    policy: &GitCorpusPolicy,
) -> Option<(String, gix::ObjectId)> {
    if is_selected(&path, policy) {
        Some((path, id))
    } else {
        None
    }
}

fn pending_path(change: &PendingChange) -> &str {
    &change.path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_documents(count: usize) -> Vec<GitHistoryDocument> {
        (0..count)
            .map(|_| GitHistoryDocument {
                source_index: 0,
                revision: 0,
                object: 0,
                path: 0,
            })
            .collect()
    }

    #[test]
    fn join_tree_path_preserves_root_and_nested_paths() {
        assert_eq!(join_tree_path("", "main.rs"), "main.rs");
        assert_eq!(join_tree_path("src", "main.rs"), "src/main.rs");
        assert_eq!(join_tree_path("src/é", "文件.rs"), "src/é/文件.rs");
    }

    #[test]
    fn source_history_state_does_not_retain_complete_revision_sets() {
        type StreamedSourceHistory = (gix::Repository, gix::ObjectId, Option<gix::ObjectId>);

        assert_eq!(
            std::mem::size_of::<SourceHistory>(),
            std::mem::size_of::<StreamedSourceHistory>()
        );
    }

    #[test]
    fn blob_deduper_preserves_reusable_proofs_from_divergent_histories() {
        let blob = gix::ObjectId::from_hex("a".repeat(40).as_bytes()).expect("blob");
        let alpha = gix::ObjectId::from_hex("b".repeat(40).as_bytes()).expect("alpha revision");
        let beta = gix::ObjectId::from_hex("c".repeat(40).as_bytes()).expect("beta revision");
        let mut proofs = HistoryBlobDeduper::default();
        proofs.insert_with_keys("src/shared.rs", blob, alpha, true, Vec::new());
        let mut divergent = HistoryBlobDeduper::default();
        divergent.insert_with_keys("src/shared.rs", blob, beta, true, Vec::new());

        proofs.merge_reusable(divergent);
        let revisions = proofs
            .reusable_revisions("src/shared.rs", blob)
            .collect::<BTreeSet<_>>();

        assert_eq!(revisions, BTreeSet::from([alpha, beta]));
    }

    #[test]
    fn history_plan_selection_is_only_a_document_and_ordinal() {
        assert_eq!(std::mem::size_of::<GitHistoryChunk>(), 8);
    }

    #[test]
    fn history_chunk_entry_does_not_duplicate_document_revision() {
        assert!(
            std::mem::size_of::<HistoryChunkEntry>() <= 16,
            "history entries must keep pooled content and revision IDs outside each row"
        );
    }

    #[test]
    fn history_chunk_index_interns_shared_content_across_revisions() {
        let mut index = HistoryChunkIndex::default();
        let mut documents = test_documents(2);
        documents[1].revision = 1;
        let content = ContentDigest::of(b"shared content");
        for (document, revision) in [(0, 0), (1, 1)] {
            assert!(
                index
                    .insert(
                        HistoryChunkKey {
                            content: content.clone(),
                            revision,
                        },
                        GitHistoryChunk {
                            document,
                            ordinal: 0,
                        },
                        &documents,
                    )
                    .is_none()
            );
        }

        assert_eq!(index.len(), 2);
        assert_eq!(index.content_count(), 1);
    }

    #[test]
    fn history_chunk_key_uses_a_pooled_revision_id() {
        assert_eq!(
            std::mem::size_of::<HistoryChunkKey>(),
            std::mem::size_of::<ContentDigest>() + std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn history_chunk_index_deduplicates_exact_keys() {
        let mut index = HistoryChunkIndex::default();
        let documents = test_documents(5);
        let key = HistoryChunkKey {
            content: ContentDigest::of(b"chunk"),
            revision: 0,
        };
        assert!(
            index
                .insert(
                    key.clone(),
                    GitHistoryChunk {
                        document: 1,
                        ordinal: 2,
                    },
                    &documents,
                )
                .is_none()
        );
        assert!(index.contains_key(&key, &documents));
        assert_eq!(
            index
                .insert(
                    key,
                    GitHistoryChunk {
                        document: 4,
                        ordinal: 5,
                    },
                    &documents,
                )
                .expect("duplicate returns the previous value")
                .document,
            1
        );
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn frozen_seed_lookup_deduplicates_seed_and_new_keys() {
        let mut index = HistoryChunkIndex::default();
        let documents = test_documents(4);
        let seed = HistoryChunkKey {
            content: ContentDigest::of(b"seed"),
            revision: 0,
        };
        index.insert(
            seed.clone(),
            GitHistoryChunk {
                document: 1,
                ordinal: 0,
            },
            &documents,
        );
        index.freeze_seed_lookup();

        assert_eq!(
            index
                .insert(
                    seed,
                    GitHistoryChunk {
                        document: 2,
                        ordinal: 1,
                    },
                    &documents,
                )
                .expect("frozen seed lookup finds an exact duplicate")
                .document,
            1
        );

        let new_key = HistoryChunkKey {
            content: ContentDigest::of(b"new"),
            revision: 0,
        };
        assert!(
            index
                .insert(
                    new_key.clone(),
                    GitHistoryChunk {
                        document: 3,
                        ordinal: 2,
                    },
                    &documents,
                )
                .is_none()
        );
        assert_eq!(index.len(), 2);
        assert!(index.contains_key(&new_key, &documents));
    }

    #[test]
    fn history_plan_document_is_only_a_git_locator() {
        assert!(
            std::mem::size_of::<GitHistoryDocument>() <= 32,
            "history document locator grew beyond its compact representation"
        );
        let fields = std::mem::size_of::<usize>()
            + 2 * std::mem::size_of::<gix::ObjectId>()
            + std::mem::size_of::<SourceKind>()
            + std::mem::size_of::<Box<str>>();
        let alignment = std::mem::align_of::<GitHistoryDocument>();
        let locator_bytes = fields.next_multiple_of(alignment);

        assert!(
            std::mem::size_of::<GitHistoryDocument>() <= locator_bytes,
            "history documents must not retain candidate chunk recipes"
        );
    }

    #[test]
    fn history_plan_interns_repeated_revisions() {
        let revision = gix::ObjectId::from_hex("a".repeat(40).as_bytes()).expect("revision");
        let first_object = gix::ObjectId::from_hex("b".repeat(40).as_bytes()).expect("object");
        let second_object = gix::ObjectId::from_hex("c".repeat(40).as_bytes()).expect("object");
        let mut plan = GitHistoryBuildPlan::default();
        let first = plan
            .push_document(
                0,
                revision,
                first_object,
                SourceKind::GitBlob,
                "src/first.rs",
            )
            .expect("first document");
        let second = plan
            .push_document(
                0,
                revision,
                second_object,
                SourceKind::GitCommit,
                "src/second.rs",
            )
            .expect("second document");

        assert_eq!(plan.revisions.values, vec![revision]);
        assert_eq!(
            plan.documents[first as usize].revision,
            plan.documents[second as usize].revision
        );
        assert_eq!(
            plan.documents[first as usize].source_kind(),
            SourceKind::GitBlob
        );
        assert_eq!(
            plan.documents[second as usize].source_kind(),
            SourceKind::GitCommit
        );
    }

    #[test]
    fn history_plan_interns_repeated_objects() {
        let revision = gix::ObjectId::from_hex("a".repeat(40).as_bytes()).expect("revision");
        let object = gix::ObjectId::from_hex("b".repeat(40).as_bytes()).expect("object");
        let mut plan = GitHistoryBuildPlan::default();
        let first = plan
            .push_document(0, revision, object, SourceKind::GitBlob, "src/lib.rs")
            .expect("first document");
        let second = plan
            .push_document(0, revision, object, SourceKind::GitBlob, "src/lib.rs")
            .expect("second document");

        assert_eq!(plan.objects.values, vec![object]);
        assert_eq!(
            plan.documents[first as usize].object,
            plan.documents[second as usize].object
        );
    }

    #[test]
    fn history_chunk_stream_delivers_owned_chunks() {
        fn compile_owned_callback(
            plan: &GitHistoryBuildPlan,
            sources: &[GitCorpusSource],
        ) -> Result<(), GitHistoryError> {
            plan.try_for_each_chunk(sources, |chunk: Chunk, _| {
                let _ = chunk;
            })
        }

        let _ = compile_owned_callback;
    }

    #[test]
    fn history_chunk_stream_delivers_borrowed_projection() {
        fn compile_projection_callback(
            plan: &GitHistoryBuildPlan,
            sources: &[GitCorpusSource],
        ) -> Result<(), GitHistoryError> {
            plan.try_for_each_history_view(sources, |view, _| {
                let _ = (view.chunk_id(), view.text(), view.next_chunk());
            })
        }

        let _ = compile_projection_callback;
    }

    #[test]
    fn history_plan_preallocates_chunk_index() {
        let plan = GitHistoryBuildPlan::with_chunk_index_capacity(32);

        assert!(plan.chunks.capacity() >= 32);
    }

    #[test]
    fn delta_chunk_index_reserve_is_adaptive() {
        assert_eq!(delta_chunk_index_capacity(4_095), 0);
        assert_eq!(delta_chunk_index_capacity(4_096), 4_096);
    }

    #[test]
    fn chunk_index_reserves_large_candidate_batches_only() {
        assert_eq!(chunk_index_reserve_target(4_096), 65_536);
        assert_eq!(chunk_index_reserve_target(65_537), 131_072);

        let mut plan = GitHistoryBuildPlan::default();

        plan.reserve_chunk_index_for_candidates(4_095);
        assert_eq!(plan.chunks.capacity(), 0);

        plan.reserve_chunk_index_for_candidates(4_096);
        assert!(plan.chunks.capacity() >= 4_096);
    }

    #[test]
    fn chunk_index_small_reserve_does_not_allocate_a_full_block() {
        let index = HistoryChunkIndex::with_capacity(32);

        assert_eq!(index.capacity(), 32);
    }

    #[test]
    fn chunk_index_growth_uses_fixed_storage_blocks() {
        let mut index = HistoryChunkIndex::default();
        let documents = test_documents(HISTORY_CHUNK_ENTRY_BLOCK + 1);
        for value in 0..=HISTORY_CHUNK_ENTRY_BLOCK {
            let mut digest = [0_u8; 32];
            digest[..8].copy_from_slice(&(value as u64).to_le_bytes());
            index.insert(
                HistoryChunkKey {
                    content: ContentDigest::from_sha256(digest),
                    revision: 0,
                },
                GitHistoryChunk {
                    document: u32::try_from(value).expect("test document index fits in u32"),
                    ordinal: 0,
                },
                &documents,
            );
        }

        assert!(index.storage_capacity() >= index.len());
        assert_eq!(index.len(), HISTORY_CHUNK_ENTRY_BLOCK + 1);
        assert_eq!(index.storage_block_count(), 2);
    }

    #[test]
    fn chunk_index_keeps_entries_in_document_order() {
        let mut index = HistoryChunkIndex::default();
        let documents = test_documents(3);
        for (value, document, ordinal) in [(0_u64, 1, 0), (1, 1, 2), (2, 2, 1)] {
            let mut digest = [0_u8; 32];
            digest[..8].copy_from_slice(&value.to_le_bytes());
            index.insert(
                HistoryChunkKey {
                    content: ContentDigest::from_sha256(digest),
                    revision: 0,
                },
                GitHistoryChunk { document, ordinal },
                &documents,
            );
        }

        let sorted = index
            .entries
            .iter()
            .map(|(_, chunk)| (chunk.document, chunk.ordinal))
            .collect::<Vec<_>>();
        assert_eq!(sorted, vec![(1, 0), (1, 2), (2, 1)]);
    }

    #[test]
    fn chunk_index_append_after_retain_reuses_fixed_storage_capacity() {
        let mut index = HistoryChunkIndex::with_capacity(HISTORY_CHUNK_ENTRY_BLOCK * 2);
        let documents = test_documents(HISTORY_CHUNK_ENTRY_BLOCK * 2);
        let block = u32::try_from(HISTORY_CHUNK_ENTRY_BLOCK).expect("test block fits in u32");
        for value in 0..(HISTORY_CHUNK_ENTRY_BLOCK * 2) {
            let mut digest = [0_u8; 32];
            digest[..8].copy_from_slice(&(value as u64).to_le_bytes());
            index.insert(
                HistoryChunkKey {
                    content: ContentDigest::from_sha256(digest),
                    revision: 0,
                },
                GitHistoryChunk {
                    document: u32::try_from(value).expect("test document index fits in u32"),
                    ordinal: 0,
                },
                &documents,
            );
        }

        index.retain(
            |_, chunk| chunk.document < block / 2 || chunk.document >= block,
            &documents,
        );
        let capacity = index.storage_capacity();

        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        index.insert(
            HistoryChunkKey {
                content: ContentDigest::from_sha256(digest),
                revision: 0,
            },
            GitHistoryChunk {
                document: u32::MAX,
                ordinal: 0,
            },
            &documents,
        );

        assert_eq!(index.storage_capacity(), capacity);
    }

    #[test]
    fn chunk_index_lookup_skips_empty_storage_blocks_after_retain() {
        let mut index = HistoryChunkIndex::with_capacity(HISTORY_CHUNK_ENTRY_BLOCK * 2);
        let documents = test_documents(HISTORY_CHUNK_ENTRY_BLOCK + 1);
        for value in 0..=HISTORY_CHUNK_ENTRY_BLOCK {
            let mut digest = [0_u8; 32];
            digest[..8].copy_from_slice(&(value as u64).to_le_bytes());
            index.insert(
                HistoryChunkKey {
                    content: ContentDigest::from_sha256(digest),
                    revision: 0,
                },
                GitHistoryChunk {
                    document: u32::try_from(value).expect("test document index fits in u32"),
                    ordinal: 0,
                },
                &documents,
            );
        }

        index.retain(
            |_, chunk| {
                chunk.document
                    == u32::try_from(HISTORY_CHUNK_ENTRY_BLOCK).expect("test block fits in u32")
            },
            &documents,
        );

        assert_eq!(index.len(), 1);
        let retained = index.values().next().expect("retained history chunk");
        assert_eq!(
            retained.document,
            u32::try_from(HISTORY_CHUNK_ENTRY_BLOCK).expect("test block fits in u32")
        );
    }

    #[test]
    fn history_content_key_keeps_its_wire_value() {
        let repository = RepositoryId::try_from("repo").expect("repository");
        let embedding_key = ContentDigest::of(b"embedding");

        assert_eq!(
            history_content_key(&repository, "src/main.c", &embedding_key).to_string(),
            "sha256:48ac550de4d56ed17b3a974d757bc9a6d10131ac5afce180ea5071f9f7675342"
        );
    }

    #[test]
    fn borrowed_identifiers_keep_the_materialized_history_key() {
        let content = (0..40)
            .map(|index| format!("identifier_{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let document = super::super::NormalizedDocument::new(
            "Identifiers",
            SourceKind::GitBlob,
            RepositoryId::try_from("repo").expect("repository"),
            Revision::try_from("a".repeat(40)).expect("revision"),
            "src/identifiers.rs",
            "text/x-rust",
            content,
        )
        .expect("document");
        let drafts =
            chunk_document_drafts(&document, ChunkingConfig::default()).expect("chunk drafts");
        let draft = drafts.get(0);
        let mut identifier_buffer = [""; MAX_IDENTIFIERS];
        let identifiers = identifier_refs(&document.path, draft.text(), &mut identifier_buffer);
        assert_eq!(identifiers.len(), MAX_IDENTIFIERS);
        let borrowed_embedding = crate::semantic::embedding_text_from_parts(
            &document.title,
            draft.headings().iter().copied(),
            identifiers,
            &document.tags,
            draft.text(),
        );
        let borrowed_key = history_content_key(
            &document.repository,
            &document.path,
            &ContentDigest::of(borrowed_embedding.as_bytes()),
        );
        let chunk = drafts
            .materialize(0, Some(identifier_values(&document.path, draft.text())))
            .expect("materialized chunk");

        assert_eq!(Some(borrowed_key), history_content_key_for_chunk(&chunk));
    }
}
