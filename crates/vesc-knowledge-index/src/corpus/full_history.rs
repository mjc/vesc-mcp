//! Incremental, content-addressed ingestion of complete reachable Git history.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sha2::{Digest, Sha256};

use super::chunking::{ChunkingConfig, chunk_document_drafts};
use super::git::{
    CachedGitBlob, Candidate, GitCorpusBudget, GitCorpusPolicy, GitCorpusSource, GitIngestionError,
    GitIngestionObservations, MAX_IDENTIFIERS, document_from_git_blob, identifier_refs,
    identifier_values, is_selected, load_git_blob, validate_policy,
};
use super::{Chunk, ContentDigest, DocumentId, RepositoryId, Revision, SourceKind};
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
    pub budget_rejections: usize,
    pub reused_contents: usize,
    pub candidate_chunks: usize,
    pub materialized_chunks: usize,
    pub candidate_identifier_count_histogram: [u64; MAX_IDENTIFIERS + 1],
    pub materialized_identifier_count_histogram: [u64; MAX_IDENTIFIERS + 1],
    pub git: GitIngestionObservations,
}

impl Default for GitHistoryRefreshObservations {
    fn default() -> Self {
        Self {
            reachable_commits: 0,
            reused_commits: 0,
            ingested_commits: 0,
            ingested_blobs: 0,
            reused_blobs: 0,
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
}

struct ProcessedHistory<'a> {
    source: &'a GitCorpusSource,
    represented_tips: HashMap<gix::ObjectId, usize>,
    reusable_blobs: HistoryBlobDeduper,
}

struct SourceHistory {
    repository: gix::Repository,
    current_id: gix::ObjectId,
    previous_id: Option<gix::ObjectId>,
}

struct TipAdmissions<'repo> {
    tree: gix::Tree<'repo>,
    blobs: HashMap<String, gix::ObjectId>,
}

impl TipAdmissions<'_> {
    fn is_admitted(&self, path: &str, id: gix::ObjectId) -> bool {
        self.blobs.get(path).is_some_and(|admitted| *admitted == id)
    }

    fn contains_tip_blob(&self, path: &str, id: gix::ObjectId) -> Result<bool, GitHistoryError> {
        let entry = self
            .tree
            .lookup_entry_by_path(path)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        Ok(entry.is_some_and(|entry| entry.mode().is_blob() && entry.object_id() == id))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CachedGitHistoryChunk<'a> {
    pub document_id: &'a str,
    pub repository: &'a str,
    pub revision: &'a str,
    pub path: &'a str,
    pub ordinal: u32,
    pub has_previous: bool,
    pub has_next: bool,
    pub blob: Option<gix::ObjectId>,
}

#[derive(Default)]
pub(crate) struct CachedGitHistory {
    repositories: HashMap<String, HashMap<String, CachedGitHistoryDocument>>,
}

struct CachedGitHistoryDocument {
    revision: Box<str>,
    path: Box<str>,
    blob: Option<gix::ObjectId>,
    chunk_count: u32,
    maximum_ordinal: u32,
    last_ordinal: Option<u32>,
    starts_at_zero: bool,
    consistent: bool,
}

impl CachedGitHistory {
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
                chunk_count: 0,
                maximum_ordinal: 0,
                last_ordinal: None,
                starts_at_zero: false,
                consistent: true,
            });
        document.consistent &= document.revision.as_ref() == chunk.revision
            && document.path.as_ref() == chunk.path
            && document.blob == chunk.blob
            && (chunk.ordinal != 0 || !chunk.has_previous);
        document.chunk_count = document.chunk_count.saturating_add(1);
        document.maximum_ordinal = document.maximum_ordinal.max(chunk.ordinal);
        document.starts_at_zero |= chunk.ordinal == 0 && !chunk.has_previous;
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
            blobs.insert(document.path.as_ref(), blob, revision, true);
        }
        Ok(blobs)
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

    fn insert(&mut self, path: &str, id: gix::ObjectId, revision: gix::ObjectId, reusable: bool) {
        let path_id = if let Some(&path_id) = self.path_ids.get(path) {
            path_id
        } else {
            let path_id = self.path_ids.len();
            self.path_ids.insert(path.to_owned(), path_id);
            path_id
        };
        self.insert_proof(path_id, id, revision, reusable);
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

impl CommitCoverage {
    const fn reused_from_prior(self) -> bool {
        self.selected && self.reused_from_prior
    }
}

#[derive(Debug)]
struct GitHistoryDocument {
    source_index: usize,
    revision: gix::ObjectId,
    blob: gix::ObjectId,
    path: Box<str>,
}

#[derive(Clone, Copy)]
struct GitHistoryDocumentLocator<'a> {
    source_index: usize,
    repository: &'a RepositoryId,
    revision: gix::ObjectId,
    blob: gix::ObjectId,
    path: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct GitHistoryChunk {
    document: u32,
    ordinal: u32,
}

/// Compact cold-build state. Git remains the source of passage text and
/// per-chunk metadata until the exact chunk is written or embedded.
#[derive(Debug, Default)]
pub(crate) struct GitHistoryBuildPlan {
    documents: Vec<GitHistoryDocument>,
    chunks: HashMap<ContentDigest, GitHistoryChunk>,
}

impl GitHistoryBuildPlan {
    pub(crate) fn len(&self) -> usize {
        self.chunks.len()
    }

    fn push_document(
        &mut self,
        source_index: usize,
        revision: gix::ObjectId,
        blob: gix::ObjectId,
        path: &str,
    ) -> Result<u32, GitHistoryError> {
        let index = u32::try_from(self.documents.len())
            .map_err(|_| GitHistoryError::Invalid("too many Git history documents".into()))?;
        self.documents.push(GitHistoryDocument {
            source_index,
            revision,
            blob,
            path: path.into(),
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
        self
    }

    fn from_chunks(
        sources: &[GitCorpusSource],
        chunks: impl IntoIterator<Item = Chunk>,
    ) -> Result<(Self, CachedGitHistory), GitHistoryError> {
        let source_indices = sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.repository_id.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut repositories = HashMap::<usize, gix::Repository>::new();
        let mut documents = HashMap::<DocumentId, u32>::new();
        let mut plan = Self::default();
        let mut cached_history = CachedGitHistory::default();
        for chunk in chunks {
            if chunk.source_kind != SourceKind::GitBlob {
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
                let repository = repositories
                    .get(&source_index)
                    .expect("repository inserted above");
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
                let document =
                    plan.push_document(source_index, revision, entry.object_id(), &chunk.path)?;
                documents.insert(cache_key, document);
                document
            };
            let descriptor = &plan.documents[document as usize];
            cached_history.observe(CachedGitHistoryChunk {
                document_id: chunk.document_id.as_str(),
                repository: chunk.repository.as_str(),
                revision: chunk.revision.as_str(),
                path: &chunk.path,
                ordinal: chunk.ordinal,
                has_previous: chunk.previous_chunk.is_some(),
                has_next: chunk.next_chunk.is_some(),
                blob: Some(descriptor.blob),
            });
            let key = history_content_key_for_chunk(&chunk)
                .expect("filtered Git-history chunk has a content key");
            plan.chunks.insert(
                key,
                GitHistoryChunk {
                    document,
                    ordinal: chunk.ordinal,
                },
            );
        }
        Ok((plan, cached_history))
    }

    #[allow(clippy::too_many_lines)] // One streaming pass shares repository and blob state.
    pub(crate) fn try_for_each_chunk(
        &self,
        sources: &[GitCorpusSource],
        mut visit: impl FnMut(&Chunk, gix::ObjectId),
    ) -> Result<(), GitHistoryError> {
        let mut selected = self.chunks.iter().collect::<Vec<_>>();
        selected.sort_unstable_by(|(left_key, left), (right_key, right)| {
            left.document
                .cmp(&right.document)
                .then_with(|| left.ordinal.cmp(&right.ordinal))
                .then_with(|| left_key.cmp(right_key))
        });
        let mut repositories = HashMap::<usize, gix::Repository>::new();
        let mut offset = 0;
        while offset < selected.len() {
            let document_index = selected[offset].1.document;
            let end = selected[offset..]
                .iter()
                .position(|(_, chunk)| chunk.document != document_index)
                .map_or(selected.len(), |relative| offset + relative);
            let descriptor = self.documents.get(document_index as usize).ok_or_else(|| {
                GitHistoryError::Invalid("history chunk references an unknown document".into())
            })?;
            let source = sources.get(descriptor.source_index).ok_or_else(|| {
                GitHistoryError::Invalid("history document references an unknown source".into())
            })?;
            if let std::collections::hash_map::Entry::Vacant(entry) =
                repositories.entry(descriptor.source_index)
            {
                let repository = gix::open(&source.repository_path)
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?;
                entry.insert(repository);
            }
            let repository = repositories
                .get(&descriptor.source_index)
                .expect("repository inserted above");
            let size = repository
                .find_header(descriptor.blob)
                .map_err(|error| GitHistoryError::Git(error.to_string()))?
                .size();
            let candidate = Candidate {
                path: descriptor.path.to_string(),
                id: descriptor.blob,
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
                    source.repository_id, descriptor.path
                )));
            }
            let revision = Revision::try_from(descriptor.revision.to_string())
                .map_err(|error| GitHistoryError::Invalid(error.to_string()))?;
            let document = document_from_git_blob(
                &descriptor.path,
                &source.repository_id,
                &revision,
                source.trust_tier,
                &source.license,
                blob,
            )?;
            let drafts = chunk_document_drafts(&document, ChunkingConfig::default())
                .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
            for (expected_key, selection) in &selected[offset..end] {
                let index = selection.ordinal as usize;
                if index >= drafts.len() {
                    return Err(GitHistoryError::Invalid(format!(
                        "pinned Git chunk ordinal {} is unavailable for {}:{}",
                        selection.ordinal, source.repository_id, descriptor.path
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
                if &actual_key != *expected_key {
                    return Err(GitHistoryError::Invalid(format!(
                        "pinned Git chunk identity changed for {}:{}#{}",
                        source.repository_id, descriptor.path, selection.ordinal
                    )));
                }
                let chunk = drafts
                    .materialize(index, Some(identifiers))
                    .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
                visit(&chunk, descriptor.blob);
            }
            offset = end;
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
            chunks.push(chunk.clone());
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

enum HistoryContents<'a> {
    All(GitHistoryBuildPlan),
    Delta {
        previous_contains:
            &'a mut dyn FnMut(&RepositoryId, &str, &ContentDigest) -> Result<bool, GitHistoryError>,
        plan: GitHistoryBuildPlan,
    },
}

impl HistoryContents<'_> {
    fn insert_draft(
        &mut self,
        key: ContentDigest,
        ordinal: u32,
        locator: GitHistoryDocumentLocator<'_>,
        document: &mut Option<u32>,
        observations: &mut GitHistoryRefreshObservations,
    ) -> Result<bool, GitHistoryError> {
        let inserted = match self {
            Self::All(plan) => {
                if plan.chunks.contains_key(&key) {
                    observations.reused_contents = observations.reused_contents.saturating_add(1);
                    false
                } else {
                    let document = selected_document(plan, document, locator)?;
                    plan.chunks
                        .insert(key, GitHistoryChunk { document, ordinal });
                    true
                }
            }
            Self::Delta {
                previous_contains,
                plan,
            } => {
                if plan.chunks.contains_key(&key)
                    || previous_contains(locator.repository, locator.path, &key)?
                {
                    observations.reused_contents = observations.reused_contents.saturating_add(1);
                    false
                } else {
                    let document = selected_document(plan, document, locator)?;
                    plan.chunks
                        .insert(key, GitHistoryChunk { document, ordinal });
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
        locator.blob,
        locator.path,
    )?;
    *selected = Some(document);
    Ok(document)
}

/// Reuse cached Git chunks when every configured tip is a fast-forward.
///
/// Repository file-count and total-byte limits bound only blobs considered by
/// this refresh: the whole reachable history for a cold build, or the new
/// fast-forward delta for an incremental build. The per-file limit is also
/// checked whenever an already-selected blob is hydrated from Git.
///
/// Returns `Ok(None)` when the cached tips cannot safely seed the current
/// history, allowing the caller to fall back to a cold rebuild.
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
    let Some((plan, mut observations)) = plan_git_history_fast_forward_from_chunks(
        sources,
        previous_tips,
        cached_chunks.iter().cloned(),
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
    plan_git_history_fast_forward_from_chunks(sources, previous_tips, cached_chunks)
}

fn plan_git_history_fast_forward_from_chunks(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_chunks: impl IntoIterator<Item = Chunk>,
) -> Result<Option<(GitHistoryBuildPlan, GitHistoryRefreshObservations)>, GitHistoryError> {
    let repositories = sources
        .iter()
        .map(|source| source.repository_id.clone())
        .collect::<BTreeSet<_>>();
    let chunks = cached_chunks.into_iter().filter(|chunk| {
        chunk.source_kind == SourceKind::GitBlob
            && repositories.contains(&chunk.repository)
            && previous_tips
                .iter()
                .any(|tip| tip.repository == chunk.repository)
    });
    let (plan, cached_history) = GitHistoryBuildPlan::from_chunks(sources, chunks)?;
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
    previous_contains: &mut dyn FnMut(
        &RepositoryId,
        &str,
        &ContentDigest,
    ) -> Result<bool, GitHistoryError>,
) -> Result<Option<(GitHistoryBuildPlan, GitHistoryRefreshObservations)>, GitHistoryError> {
    ingest_git_history_fast_forward_with_contents(
        sources,
        previous_tips,
        cached_history,
        HistoryContents::Delta {
            previous_contains,
            plan: GitHistoryBuildPlan::default(),
        },
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
            current_id,
            previous_id,
        }) = source_history(source, &tips)?
        else {
            return Ok(None);
        };
        let known_history = processed
            .iter()
            .position(|known| same_corpus_contract(source, known.source));
        if let Some(reachable) = known_history
            .and_then(|index| processed[index].represented_tips.get(&current_id))
            .copied()
        {
            observations.reachable_commits =
                observations.reachable_commits.saturating_add(reachable);
            observations.reused_commits = observations.reused_commits.saturating_add(reachable);
            continue;
        }
        let reusable_blobs = known_history.map(|index| &processed[index].reusable_blobs);
        let mut source_reachable = 0_usize;
        if let Some(previous_id) = previous_id {
            let reused = count_reachable(&repo, previous_id)?;
            source_reachable = source_reachable.saturating_add(reused);
            observations.reachable_commits = observations.reachable_commits.saturating_add(reused);
            observations.reused_commits = observations.reused_commits.saturating_add(reused);
        }
        let walk = previous_id.map_or_else(
            || repo.rev_walk([current_id]),
            |previous_id| repo.rev_walk([current_id]).with_hidden([previous_id]),
        );
        let walk = walk
            .all()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let mut resource_cache = None;
        let mut seen_blobs = cached_history.take_reusable_blobs(source, &repo)?;
        let commit_graph = reusable_blobs
            .map(|_| repo.commit_graph_if_enabled())
            .transpose()
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let mut reuse_graph = commit_graph
            .as_ref()
            .map(|cache| repo.revision_graph(cache.as_ref()));
        let mut reuse_reachability = HashMap::new();
        let mut revision_is_reachable = |revision| {
            if let Some(reachable) = reuse_reachability.get(&revision).copied() {
                return Ok(reachable);
            }
            let reachable = if revision == current_id {
                true
            } else if !repo.has_object(revision) {
                false
            } else {
                let graph = reuse_graph
                    .as_mut()
                    .expect("reusable blobs initialize a revision graph");
                match repo.merge_base_with_graph(current_id, revision, graph) {
                    Ok(base) => base.detach() == revision,
                    Err(gix::repository::merge_base_with_graph::Error::NotFound { .. }) => false,
                    Err(error) => return Err(GitHistoryError::Git(error.to_string())),
                }
            };
            reuse_reachability.insert(revision, reachable);
            Ok(reachable)
        };
        let mut budget = GitCorpusBudget::new(source.policy.limits);
        let mut ingested_commits = 0_usize;
        let tip_admissions = if previous_id.is_none() {
            resource_cache = Some(
                repo.diff_resource_cache_for_tree_diff()
                    .map_err(|error| GitHistoryError::Git(error.to_string()))?,
            );
            Some(reserve_tip_blobs(
                &repo,
                source,
                current_id,
                resource_cache
                    .as_mut()
                    .expect("resource cache initialized above"),
                reusable_blobs,
                &mut revision_is_reachable,
                &mut budget,
                &mut observations,
            )?)
        } else {
            None
        };
        let mut commits = Vec::new();
        for info in walk {
            let info = info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
            source_reachable = source_reachable.saturating_add(1);
            observations.reachable_commits = observations.reachable_commits.saturating_add(1);
            #[cfg(feature = "coz-profile")]
            coz::progress!("git_history_walk_commit");
            let commit = info
                .object()
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let commit = ReachableCommit {
                id: info.id,
                first_parent: commit.parent_ids().next().map(gix::Id::detach),
            };
            commits.push(commit);
        }
        commits.reverse();
        for commit in &commits {
            if resource_cache.is_none() {
                resource_cache = Some(
                    repo.diff_resource_cache_for_tree_diff()
                        .map_err(|error| GitHistoryError::Git(error.to_string()))?,
                );
            }
            let coverage = ingest_commit_changes(
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
            if coverage.reused_from_prior() {
                observations.reused_commits = observations.reused_commits.saturating_add(1);
            } else {
                ingested_commits = ingested_commits.saturating_add(1);
            }
            #[cfg(feature = "coz-profile")]
            coz::progress!("git_history_ingested_commit");
        }
        observations.ingested_commits = observations
            .ingested_commits
            .saturating_add(ingested_commits);
        if let Some(index) = known_history {
            let known = &mut processed[index];
            known.represented_tips.insert(current_id, source_reachable);
            known.reusable_blobs.merge_reusable(seen_blobs);
        } else {
            let mut reusable_blobs = HistoryBlobDeduper::default();
            reusable_blobs.merge_reusable(seen_blobs);
            processed.push(ProcessedHistory {
                source,
                represented_tips: HashMap::from([(current_id, source_reachable)]),
                reusable_blobs,
            });
        }
    }
    Ok(Some((contents.into_plan(), observations)))
}

fn validated_history_tips(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
) -> Option<BTreeMap<RepositoryId, Revision>> {
    let tips = previous_tips
        .iter()
        .map(|tip| (tip.repository.clone(), tip.revision.clone()))
        .collect::<BTreeMap<_, _>>();
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
    tips: &BTreeMap<RepositoryId, Revision>,
) -> Result<Option<SourceHistory>, GitHistoryError> {
    validate_policy(&source.policy)?;
    let repository = gix::open(&source.repository_path)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let current_id = gix::ObjectId::from_hex(source.revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let previous_id = tips
        .get(&source.repository_id)
        .map(|revision| {
            gix::ObjectId::from_hex(revision.as_str().as_bytes())
                .map_err(|error| GitHistoryError::Git(error.to_string()))
        })
        .transpose()?;
    if let Some(previous) = previous_id
        && !is_ancestor(&repository, current_id, previous)?
    {
        return Ok(None);
    }
    Ok(Some(SourceHistory {
        repository,
        current_id,
        previous_id,
    }))
}

fn same_corpus_contract(left: &GitCorpusSource, right: &GitCorpusSource) -> bool {
    left.trust_tier == right.trust_tier
        && left.license == right.license
        && left.policy == right.policy
}

fn is_ancestor(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    ancestor: gix::ObjectId,
) -> Result<bool, GitHistoryError> {
    if tip == ancestor {
        return Ok(true);
    }
    match repo.merge_base(tip, ancestor) {
        Ok(base) => Ok(base.detach() == ancestor),
        Err(gix::repository::merge_base::Error::NotFound { .. }) => Ok(false),
        Err(error) => Err(GitHistoryError::Git(error.to_string())),
    }
}

fn count_reachable(repo: &gix::Repository, tip: gix::ObjectId) -> Result<usize, GitHistoryError> {
    let mut walk = repo
        .rev_walk([tip])
        .all()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    walk.try_fold(0_usize, |count, info| {
        info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
        #[cfg(feature = "coz-profile")]
        coz::progress!("git_history_walk_commit");
        Ok(count.saturating_add(1))
    })
}

#[allow(clippy::too_many_arguments)]
fn reserve_tip_blobs<'repo>(
    repo: &'repo gix::Repository,
    source: &GitCorpusSource,
    current_id: gix::ObjectId,
    resource_cache: &mut gix::diff::blob::Platform,
    reusable_blobs: Option<&HistoryBlobDeduper>,
    revision_is_reachable: &mut dyn FnMut(gix::ObjectId) -> Result<bool, GitHistoryError>,
    budget: &mut GitCorpusBudget,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<TipAdmissions<'repo>, GitHistoryError> {
    let current = repo
        .find_commit(current_id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?
        .tree()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut blobs = HashMap::new();
    let mut callback_error = None;
    let diff = repo
        .empty_tree()
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
            match has_reachable_reusable_blob(reusable_blobs, &path, id, revision_is_reachable) {
                Ok(true) => return Ok(std::ops::ControlFlow::Continue(())),
                Ok(false) => {}
                Err(error) => {
                    callback_error = Some(error);
                    return Ok(std::ops::ControlFlow::Break(()));
                }
            }
            let size = match repo.find_header(id) {
                Ok(header) => header.size(),
                Err(error) => {
                    callback_error = Some(GitHistoryError::Git(error.to_string()));
                    return Ok(std::ops::ControlFlow::Break(()));
                }
            };
            if budget.reserve(size).is_err() {
                observations.budget_rejections = observations.budget_rejections.saturating_add(1);
                return Ok(std::ops::ControlFlow::Continue(()));
            }
            blobs.insert(path, id);
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        });
    resource_cache.clear_resource_cache_keep_allocation();
    diff.map_err(|error| GitHistoryError::Git(error.to_string()))?;
    if let Some(error) = callback_error {
        return Err(error);
    }
    Ok(TipAdmissions {
        tree: current,
        blobs,
    })
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
    tip_admissions: Option<&TipAdmissions<'_>>,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<CommitCoverage, GitHistoryError> {
    let current_commit = repo
        .find_commit(commit.id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let current = current_commit
        .tree()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let previous = commit.first_parent.map_or_else(
        || Ok(repo.empty_tree()),
        |parent| {
            let parent = repo
                .find_commit(parent)
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            parent
                .tree()
                .map_err(|error| GitHistoryError::Git(error.to_string()))
        },
    )?;
    let mut pending = Vec::new();
    let mut callback_error = None;
    let mut coverage = CommitCoverage::default();
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
                if let Some(admissions) = tip_admissions {
                    match admissions.contains_tip_blob(&path, id) {
                        Ok(true) => return Ok(std::ops::ControlFlow::Continue(())),
                        Ok(false) => {}
                        Err(error) => {
                            callback_error = Some(error);
                            return Ok(std::ops::ControlFlow::Break(()));
                        }
                    }
                }
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

    let revision = Revision::try_from(commit.id.to_string())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    for PendingChange { path, id, size } in pending {
        let reusable = ingest_upsert(
            repo,
            source_index,
            source,
            &revision,
            &path,
            id,
            size,
            contents,
            observations,
        )?;
        seen_blobs.insert(&path, id, commit.id, reusable);
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
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<bool, GitHistoryError> {
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
        return Ok(false);
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
    let drafts = chunk_document_drafts(&document, ChunkingConfig::default())
        .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
    let revision_id = gix::ObjectId::from_hex(revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let locator = GitHistoryDocumentLocator {
        source_index,
        repository: &source.repository_id,
        revision: revision_id,
        blob: id,
        path,
    };
    let mut document_index = None;
    let mut all_inserted = drafts.len() != 0;
    for index in 0..drafts.len() {
        let draft = drafts.get(index);
        let mut identifier_buffer = [""; MAX_IDENTIFIERS];
        let identifiers = identifier_refs(path, draft.text(), &mut identifier_buffer);
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
        let key = history_content_key(&source.repository_id, path, &embedding_key);
        all_inserted &= contents.insert_draft(
            key,
            draft.ordinal(),
            locator,
            &mut document_index,
            observations,
        )?;
    }
    #[cfg(feature = "coz-profile")]
    coz::progress!("git_history_ingested_blob");
    Ok(all_inserted)
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
    (chunk.source_kind == SourceKind::GitBlob).then(|| {
        let embedding_key = embedding_text_digest_from_metadata(
            &chunk.title,
            chunk.heading_path.iter().map(String::as_str),
            &chunk.identifiers,
            &chunk.tags,
            &chunk.text,
        );
        history_content_key(&chunk.repository, &chunk.path, &embedding_key)
    })
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
        proofs.insert("src/shared.rs", blob, alpha, true);
        let mut divergent = HistoryBlobDeduper::default();
        divergent.insert("src/shared.rs", blob, beta, true);

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
    fn history_plan_document_is_only_a_git_locator() {
        let locator_bytes = std::mem::size_of::<usize>()
            + 2 * std::mem::size_of::<gix::ObjectId>()
            + std::mem::size_of::<Box<str>>();

        assert!(
            std::mem::size_of::<GitHistoryDocument>() <= locator_bytes,
            "history documents must not retain candidate chunk recipes"
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
