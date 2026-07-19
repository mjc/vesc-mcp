//! Content-addressed ingestion of every tagged or `release_*` repository release.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::chunking::{ChunkingConfig, ChunkingError, chunk_document};
use super::git::{
    GitCorpusPolicy, GitIngestionCache, GitIngestionError, GitIngestionObservations,
    ingest_git_commit_cached,
};
use super::{
    ChunkId, ContentDigest, LicenseStatus, NormalizedDocument, RepositoryId, Revision, SourceSpan,
    TrustTier,
};
use crate::hardware::{JINA_CODE_FP16_SHA256, JINA_CODE_INT8_SHA256};
use crate::semantic::{
    EmbeddingError, EmbeddingProfile, EmbeddingProvider, OutputNormalization,
    embedding_identifiers, embedding_text,
};

/// A repository whose complete tagged history should be indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedHistorySource {
    pub repository_path: PathBuf,
    pub repository_id: RepositoryId,
    pub trust_tier: TrustTier,
    pub license: LicenseStatus,
    pub policy: GitCorpusPolicy,
    pub chunking: ChunkingConfig,
}

/// One unique tagged commit and all tag aliases which point to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRelease {
    pub revision: Revision,
    pub tags: Vec<String>,
    pub primary_tag: String,
    pub commit_time: i64,
    pub commit_parents: Vec<Revision>,
    pub release_parents: Vec<Revision>,
}

/// The unique input presented to the embedding provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryContent {
    pub vector_key: ContentDigest,
    pub embedding_text: String,
    pub identifiers: BTreeSet<String>,
}

/// Version-specific provenance pointing at one shared embedding input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryOccurrence {
    pub repository: RepositoryId,
    pub tag: String,
    pub revision: Revision,
    pub path: String,
    pub source_span: Option<SourceSpan>,
    pub chunk_id: ChunkId,
    pub vector_key: ContentDigest,
}

/// The kind of source change on one release edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
    Moved,
}

/// Exact, versioned evidence for a source change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeEvent {
    pub repository: RepositoryId,
    pub kind: ChangeKind,
    pub from_revision: Option<Revision>,
    pub from_tags: Vec<String>,
    pub to_revision: Revision,
    pub to_tags: Vec<String>,
    pub before_path: Option<String>,
    pub after_path: Option<String>,
    pub before_content: Option<ContentDigest>,
    pub after_content: Option<ContentDigest>,
    pub identifiers: BTreeSet<String>,
    pub evidence: String,
}

/// A compact history artifact: unique content plus complete version occurrences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaggedHistory {
    pub schema: u16,
    pub repository: RepositoryId,
    pub releases: Vec<HistoryRelease>,
    pub contents: Vec<HistoryContent>,
    pub occurrences: Vec<HistoryOccurrence>,
    pub changes: Vec<ChangeEvent>,
    pub observations: TaggedHistoryObservations,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TaggedHistoryObservations {
    pub tag_count: usize,
    pub release_count: usize,
    pub occurrence_count: usize,
    pub unique_embedding_inputs: usize,
    pub git: GitIngestionObservations,
}

impl TaggedHistory {
    #[must_use]
    pub fn release_for_tag(&self, tag: &str) -> Option<&HistoryRelease> {
        self.releases
            .iter()
            .find(|release| release.tags.iter().any(|candidate| candidate == tag))
    }

    #[must_use]
    pub fn tags_for_identifier(&self, identifier: &str) -> Vec<&str> {
        let keys = self
            .contents
            .iter()
            .filter(|content| content.identifiers.contains(identifier))
            .map(|content| &content.vector_key)
            .collect::<BTreeSet<_>>();
        self.occurrences
            .iter()
            .filter(|occurrence| keys.contains(&occurrence.vector_key))
            .map(|occurrence| occurrence.tag.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub fn first_seen(&self, identifier: &str) -> Option<&str> {
        self.boundary_seen(identifier, false)
    }

    #[must_use]
    pub fn last_seen(&self, identifier: &str) -> Option<&str> {
        self.boundary_seen(identifier, true)
    }

    fn boundary_seen(&self, identifier: &str, reverse: bool) -> Option<&str> {
        let keys = self
            .contents
            .iter()
            .filter(|content| content.identifiers.contains(identifier))
            .map(|content| &content.vector_key)
            .collect::<BTreeSet<_>>();
        let mut releases = self.releases.iter();
        if reverse {
            releases
                .rfind(|release| self.release_contains_any(release, &keys))
                .map(|release| release.primary_tag.as_str())
        } else {
            releases
                .find(|release| self.release_contains_any(release, &keys))
                .map(|release| release.primary_tag.as_str())
        }
    }

    fn release_contains_any(
        &self,
        release: &HistoryRelease,
        keys: &BTreeSet<&ContentDigest>,
    ) -> bool {
        self.occurrences.iter().any(|occurrence| {
            occurrence.revision == release.revision && keys.contains(&occurrence.vector_key)
        })
    }

    #[must_use]
    pub fn changes_for_identifier(&self, identifier: &str) -> Vec<&ChangeEvent> {
        self.changes
            .iter()
            .filter(|change| change.identifiers.contains(identifier))
            .collect()
    }

    #[must_use]
    pub fn changes_in_tag(&self, tag: &str) -> Vec<&ChangeEvent> {
        self.changes
            .iter()
            .filter(|change| change.to_tags.iter().any(|candidate| candidate == tag))
            .collect()
    }

    #[must_use]
    pub fn changes_between(&self, from_tag: &str, to_tag: &str) -> Vec<&ChangeEvent> {
        self.changes
            .iter()
            .filter(|change| {
                change
                    .from_tags
                    .iter()
                    .any(|candidate| candidate == from_tag)
                    && change.to_tags.iter().any(|candidate| candidate == to_tag)
            })
            .collect()
    }

    /// Writes a complete history artifact with an atomic same-directory rename.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when serialization, writing, syncing, or renaming fails.
    pub fn write_artifact(&self, path: &Path) -> Result<(), HistoryError> {
        write_json_atomically(path, self)
    }

    /// Reads a complete history artifact.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when the file cannot be read or decoded.
    pub fn read_artifact(path: &Path) -> Result<Self, HistoryError> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HistoryError {
    #[error(transparent)]
    GitIngestion(#[from] GitIngestionError),
    #[error(transparent)]
    Chunking(#[from] ChunkingError),
    #[error(transparent)]
    Embedding(#[from] EmbeddingError),
    #[error("cannot inspect tagged Git history: {0}")]
    Git(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("embedding contract does not match the existing cache namespace")]
    CacheContractMismatch,
    #[error("query embedding contract is incompatible with the history vector artifact")]
    QueryContractMismatch,
    #[error("cached vector {key} has dimension {actual}, expected {expected}")]
    CacheDimension {
        key: ContentDigest,
        expected: usize,
        actual: usize,
    },
    #[error("embedding provider returned {actual} vectors for {expected} inputs")]
    ProviderCount { expected: usize, actual: usize },
    #[error("embedding vector cannot be normalized")]
    InvalidVector,
    #[error("invalid history vector artifact: {0}")]
    InvalidArtifact(String),
}

#[derive(Debug, Clone)]
struct RawRelease {
    revision: Revision,
    tags: Vec<String>,
    commit_time: i64,
    commit_parents: Vec<Revision>,
    release_parents: Vec<Revision>,
}

#[derive(Debug, Clone)]
struct SnapshotDocument {
    path: String,
    content: Arc<str>,
    content_digest: ContentDigest,
    identifiers: BTreeSet<String>,
}

impl SnapshotDocument {
    fn from_document(
        document: NormalizedDocument,
        content_pool: &mut BTreeMap<ContentDigest, Arc<str>>,
    ) -> Self {
        let content_digest = document.content_digest;
        let content = content_pool
            .entry(content_digest.clone())
            .or_insert_with(|| Arc::from(document.content))
            .clone();
        Self {
            path: document.path,
            content,
            content_digest,
            identifiers: document.identifiers,
        }
    }
}

/// Ingests each unique tagged commit once and records every tag alias.
///
/// # Errors
///
/// Returns [`HistoryError`] when Git history cannot be resolved, a tagged
/// snapshot cannot be ingested, or its documents cannot be chunked.
#[allow(clippy::too_many_lines)]
pub fn ingest_tagged_history(source: &TaggedHistorySource) -> Result<TaggedHistory, HistoryError> {
    let repo =
        gix::open(&source.repository_path).map_err(|error| HistoryError::Git(error.to_string()))?;
    let mut releases = tagged_releases(&repo)?;
    let tagged_revisions = releases
        .iter()
        .map(|release| release.revision.as_str().to_owned())
        .collect::<HashSet<_>>();
    for release in &mut releases {
        release.release_parents =
            nearest_tagged_ancestors(&repo, release.revision.as_str(), &tagged_revisions)?;
    }
    releases = topological_releases(releases);

    let mut snapshots = BTreeMap::<Revision, BTreeMap<String, SnapshotDocument>>::new();
    let mut remaining_children = BTreeMap::<Revision, usize>::new();
    for release in &releases {
        for parent in &release.release_parents {
            *remaining_children.entry(parent.clone()).or_default() += 1;
        }
    }
    let mut snapshot_content = BTreeMap::<ContentDigest, Arc<str>>::new();
    let mut contents = BTreeMap::<ContentDigest, HistoryContent>::new();
    let mut occurrences = Vec::new();
    let mut changes = Vec::new();
    let mut git_cache = GitIngestionCache::default();
    let mut git_observations = GitIngestionObservations::default();
    for release in &releases {
        let mut report = ingest_git_commit_cached(
            &source.repository_path,
            &source.repository_id,
            &release.revision,
            source.trust_tier,
            &source.license,
            &source.policy,
            &mut git_cache,
        )?;
        if let Some(observations) = report.git_observations.take() {
            git_observations.accumulate(&observations);
        }
        let mut snapshot = BTreeMap::new();
        for document in report.documents {
            for chunk in chunk_document(&document, source.chunking)? {
                let input = embedding_text(&chunk);
                let vector_key = ContentDigest::of(input.as_bytes());
                contents
                    .entry(vector_key.clone())
                    .or_insert_with(|| HistoryContent {
                        vector_key: vector_key.clone(),
                        embedding_text: input,
                        identifiers: embedding_identifiers(&chunk)
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                    });
                for tag in &release.tags {
                    occurrences.push(HistoryOccurrence {
                        repository: source.repository_id.clone(),
                        tag: tag.clone(),
                        revision: release.revision.clone(),
                        path: chunk.path.clone(),
                        source_span: chunk.source_span,
                        chunk_id: chunk.chunk_id.clone(),
                        vector_key: vector_key.clone(),
                    });
                }
            }
            let path = document.path.clone();
            snapshot.insert(
                path,
                SnapshotDocument::from_document(document, &mut snapshot_content),
            );
        }
        if release.release_parents.is_empty() {
            changes.extend(root_changes(source, release, &snapshot));
        } else {
            let mut consumed_parents = Vec::new();
            for parent in &release.release_parents {
                let parent_release = releases
                    .iter()
                    .find(|candidate| &candidate.revision == parent)
                    .ok_or_else(|| {
                        HistoryError::Git(format!("tagged release parent {parent} was not indexed"))
                    })?;
                changes.extend(compare_snapshots(
                    source,
                    parent_release,
                    release,
                    &snapshots[parent],
                    &snapshot,
                ));
                let Some(remaining) = remaining_children.get_mut(parent) else {
                    return Err(HistoryError::Git(format!(
                        "release parent {parent} has no child count"
                    )));
                };
                *remaining -= 1;
                if *remaining == 0 {
                    consumed_parents.push(parent.clone());
                }
            }
            for parent in consumed_parents {
                snapshots.remove(&parent);
            }
        }
        if remaining_children
            .get(&release.revision)
            .is_some_and(|&children| children > 0)
        {
            snapshots.insert(release.revision.clone(), snapshot);
        }
    }

    let public_releases = releases.iter().map(public_release).collect::<Vec<_>>();
    occurrences.sort_by(|left, right| {
        left.revision
            .cmp(&right.revision)
            .then_with(|| left.tag.cmp(&right.tag))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    let observations = TaggedHistoryObservations {
        tag_count: releases.iter().map(|release| release.tags.len()).sum(),
        release_count: releases.len(),
        occurrence_count: occurrences.len(),
        unique_embedding_inputs: contents.len(),
        git: git_observations,
    };
    Ok(TaggedHistory {
        schema: 1,
        repository: source.repository_id.clone(),
        releases: public_releases,
        contents: contents.into_values().collect(),
        occurrences,
        changes,
        observations,
    })
}

fn tagged_releases(repo: &gix::Repository) -> Result<Vec<RawRelease>, HistoryError> {
    let refs = repo
        .references()
        .map_err(|error| HistoryError::Git(error.to_string()))?
        .tags()
        .map_err(|error| HistoryError::Git(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| HistoryError::Git(error.to_string()))?;
    let mut named_refs = refs
        .into_iter()
        .map(|reference| {
            let name = String::from_utf8_lossy(reference.name().shorten().as_ref()).into_owned();
            (reference, name)
        })
        .collect::<Vec<_>>();
    let references = repo
        .references()
        .map_err(|error| HistoryError::Git(error.to_string()))?;
    let refs = references
        .remote_branches()
        .map_err(|error| HistoryError::Git(error.to_string()))?;
    for reference in refs {
        let reference = reference.map_err(|error| HistoryError::Git(error.to_string()))?;
        let short = String::from_utf8_lossy(reference.name().shorten().as_ref()).into_owned();
        let branch = short
            .rsplit_once('/')
            .map_or(short.as_str(), |(_, name)| name);
        if branch.starts_with("release_") {
            named_refs.push((reference, branch.to_owned()));
        }
    }
    let mut grouped = BTreeMap::<String, (i64, Vec<String>, Vec<Revision>)>::new();
    for (mut reference, tag) in named_refs {
        let commit = reference
            .peel_to_commit()
            .map_err(|error| HistoryError::Git(error.to_string()))?;
        let revision = commit.id.to_string();
        let commit_time = commit
            .time()
            .map_err(|error| HistoryError::Git(error.to_string()))?
            .seconds;
        let parents = commit
            .parent_ids()
            .map(|parent| Revision::try_from(parent.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| HistoryError::Git(error.to_string()))?;
        let entry = grouped
            .entry(revision)
            .or_insert((commit_time, Vec::new(), parents));
        entry.1.push(tag);
    }
    grouped
        .into_iter()
        .map(|(revision, (commit_time, mut tags, commit_parents))| {
            tags.sort();
            Ok(RawRelease {
                revision: Revision::try_from(revision)
                    .map_err(|error| HistoryError::Git(error.to_string()))?,
                tags,
                commit_time,
                commit_parents,
                release_parents: Vec::new(),
            })
        })
        .collect()
}

fn nearest_tagged_ancestors(
    repo: &gix::Repository,
    revision: &str,
    tagged: &HashSet<String>,
) -> Result<Vec<Revision>, HistoryError> {
    let commit = repo
        .find_commit(
            gix::ObjectId::from_hex(revision.as_bytes())
                .map_err(|error| HistoryError::Git(error.to_string()))?,
        )
        .map_err(|error| HistoryError::Git(error.to_string()))?;
    let mut queue = commit
        .parent_ids()
        .map(|id| id.to_string())
        .collect::<VecDeque<_>>();
    let mut visited = HashSet::new();
    let mut found = BTreeSet::new();
    while let Some(candidate) = queue.pop_front() {
        if !visited.insert(candidate.clone()) {
            continue;
        }
        if tagged.contains(&candidate) {
            found.insert(candidate);
            continue;
        }
        let commit = repo
            .find_commit(
                gix::ObjectId::from_hex(candidate.as_bytes())
                    .map_err(|error| HistoryError::Git(error.to_string()))?,
            )
            .map_err(|error| HistoryError::Git(error.to_string()))?;
        queue.extend(commit.parent_ids().map(|id| id.to_string()));
    }
    found
        .into_iter()
        .map(|revision| {
            Revision::try_from(revision).map_err(|error| HistoryError::Git(error.to_string()))
        })
        .collect()
}

fn topological_releases(mut releases: Vec<RawRelease>) -> Vec<RawRelease> {
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::with_capacity(releases.len());
    while !releases.is_empty() {
        let mut ready = releases
            .iter()
            .enumerate()
            .filter(|(_, release)| {
                release
                    .release_parents
                    .iter()
                    .all(|parent| emitted.contains(parent))
            })
            .map(|(index, release)| (release.commit_time, release.revision.clone(), index))
            .collect::<Vec<_>>();
        ready.sort();
        let index = ready.first().map_or(0, |(_, _, index)| *index);
        let release = releases.remove(index);
        emitted.insert(release.revision.clone());
        ordered.push(release);
    }
    ordered
}

fn public_release(release: &RawRelease) -> HistoryRelease {
    let primary_tag = release
        .tags
        .iter()
        .min_by_key(|tag| (tag.len(), tag.as_str()))
        .cloned()
        .unwrap_or_default();
    HistoryRelease {
        revision: release.revision.clone(),
        tags: release.tags.clone(),
        primary_tag,
        commit_time: release.commit_time,
        commit_parents: release.commit_parents.clone(),
        release_parents: release.release_parents.clone(),
    }
}

fn root_changes(
    source: &TaggedHistorySource,
    release: &RawRelease,
    after: &BTreeMap<String, SnapshotDocument>,
) -> Vec<ChangeEvent> {
    after
        .values()
        .map(|document| {
            change_event(
                source,
                None,
                release,
                ChangeKind::Added,
                None,
                Some(document),
            )
        })
        .collect()
}

fn compare_snapshots(
    source: &TaggedHistorySource,
    parent: &RawRelease,
    release: &RawRelease,
    before: &BTreeMap<String, SnapshotDocument>,
    after: &BTreeMap<String, SnapshotDocument>,
) -> Vec<ChangeEvent> {
    let mut changes = Vec::new();
    let mut removed = before
        .iter()
        .filter(|(path, _)| !after.contains_key(*path))
        .collect::<Vec<_>>();
    let mut added = after
        .iter()
        .filter(|(path, _)| !before.contains_key(*path))
        .collect::<Vec<_>>();
    let mut consumed_removed = BTreeSet::new();
    let mut consumed_added = BTreeSet::new();
    removed.sort_by_key(|(path, _)| *path);
    added.sort_by_key(|(path, _)| *path);
    for (before_path, before_document) in &removed {
        if let Some((after_path, after_document)) = added.iter().find(|(after_path, document)| {
            !consumed_added.contains(*after_path)
                && document.content_digest == before_document.content_digest
        }) {
            consumed_removed.insert(*before_path);
            consumed_added.insert(*after_path);
            changes.push(change_event(
                source,
                Some(parent),
                release,
                ChangeKind::Moved,
                Some(before_document),
                Some(after_document),
            ));
        }
    }
    for (path, before_document) in before {
        if let Some(after_document) = after.get(path) {
            if before_document.content_digest != after_document.content_digest {
                changes.push(change_event(
                    source,
                    Some(parent),
                    release,
                    ChangeKind::Modified,
                    Some(before_document),
                    Some(after_document),
                ));
            }
        } else if !consumed_removed.contains(path) {
            changes.push(change_event(
                source,
                Some(parent),
                release,
                ChangeKind::Removed,
                Some(before_document),
                None,
            ));
        }
    }
    for (path, after_document) in after {
        if !before.contains_key(path) && !consumed_added.contains(path) {
            changes.push(change_event(
                source,
                Some(parent),
                release,
                ChangeKind::Added,
                None,
                Some(after_document),
            ));
        }
    }
    changes
}

fn change_event(
    source: &TaggedHistorySource,
    parent: Option<&RawRelease>,
    release: &RawRelease,
    kind: ChangeKind,
    before: Option<&SnapshotDocument>,
    after: Option<&SnapshotDocument>,
) -> ChangeEvent {
    let identifiers = before
        .into_iter()
        .chain(after)
        .flat_map(|document| document.identifiers.iter().cloned())
        .collect();
    ChangeEvent {
        repository: source.repository_id.clone(),
        kind,
        from_revision: parent.map(|release| release.revision.clone()),
        from_tags: parent.map_or_else(Vec::new, |release| release.tags.clone()),
        to_revision: release.revision.clone(),
        to_tags: release.tags.clone(),
        before_path: before.map(|document| document.path.clone()),
        after_path: after.map(|document| document.path.clone()),
        before_content: before.map(|document| document.content_digest.clone()),
        after_content: after.map(|document| document.content_digest.clone()),
        identifiers,
        evidence: change_evidence(kind, before, after),
    }
}

fn change_evidence(
    kind: ChangeKind,
    before: Option<&SnapshotDocument>,
    after: Option<&SnapshotDocument>,
) -> String {
    match kind {
        ChangeKind::Added => bounded_evidence(&[
            "+ ",
            after.map_or("", |document| document.path.as_str()),
            "\n",
            after.map_or("", |document| document.content.as_ref()),
        ]),
        ChangeKind::Removed => bounded_evidence(&[
            "- ",
            before.map_or("", |document| document.path.as_str()),
            "\n",
            before.map_or("", |document| document.content.as_ref()),
        ]),
        ChangeKind::Moved => bounded_evidence(&[
            before.map_or("", |document| document.path.as_str()),
            " -> ",
            after.map_or("", |document| document.path.as_str()),
        ]),
        ChangeKind::Modified => modified_evidence(
            before.map_or("", |document| document.content.as_ref()),
            after.map_or("", |document| document.content.as_ref()),
        ),
    }
}

fn bounded_evidence(parts: &[&str]) -> String {
    const MAX_BYTES: usize = 4 * 1024;
    let mut output = String::with_capacity(MAX_BYTES);
    for part in parts {
        if !push_bounded(&mut output, part, MAX_BYTES) {
            output.push_str("\n... [bounded excerpt]");
            return output;
        }
    }
    output
}

fn push_bounded(output: &mut String, part: &str, maximum: usize) -> bool {
    let remaining = maximum.saturating_sub(output.len());
    if part.len() <= remaining {
        output.push_str(part);
        return true;
    }
    let end = part
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= remaining)
        .last()
        .unwrap_or(0);
    output.push_str(&part[..end]);
    false
}

fn modified_evidence(before: &str, after: &str) -> String {
    let before_lines = before.lines().collect::<Vec<_>>();
    let after_lines = after.lines().collect::<Vec<_>>();
    let prefix = before_lines
        .iter()
        .zip(&after_lines)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = before_lines[prefix..]
        .iter()
        .rev()
        .zip(after_lines[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let before_end = before_lines.len().saturating_sub(suffix);
    let after_end = after_lines.len().saturating_sub(suffix);
    let mut output = String::with_capacity(4 * 1024);
    for (marker, lines) in [
        ("- ", &before_lines[prefix..before_end]),
        ("+ ", &after_lines[prefix..after_end]),
    ] {
        for line in lines {
            let separator = if output.is_empty() { "" } else { "\n" };
            if !push_bounded(&mut output, separator, 4 * 1024)
                || !push_bounded(&mut output, marker, 4 * 1024)
                || !push_bounded(&mut output, line, 4 * 1024)
            {
                output.push_str("\n... [bounded excerpt]");
                return output;
            }
        }
    }
    output
}

/// Exact namespace for persistent vector reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingContract {
    pub schema: u16,
    pub model_id: String,
    pub model_revision: String,
    pub model_digest: ContentDigest,
    pub tokenizer_digest: ContentDigest,
    pub profile: EmbeddingProfile,
    pub windowing: String,
    pub embedding_text_version: u16,
}

impl EmbeddingContract {
    /// Returns whether query vectors share this artifact's embedding space.
    #[must_use]
    pub fn supports_query(&self, query: &Self) -> bool {
        let same_profile = self.profile.pooling == query.profile.pooling
            && self.profile.query_prefix == query.profile.query_prefix
            && self.profile.document_prefix == query.profile.document_prefix
            && self.profile.dimension == query.profile.dimension
            && self.profile.normalize == query.profile.normalize;
        let same_model = self.model_digest == query.model_digest;
        let approved_jina_split = self.model_id == crate::hardware::JINA_CODE_MODEL_ID
            && self.model_revision == crate::hardware::JINA_CODE_MODEL_REVISION
            && digest_is(&self.model_digest, JINA_CODE_FP16_SHA256)
            && digest_is(&query.model_digest, JINA_CODE_INT8_SHA256);
        self.schema == query.schema
            && self.model_id == query.model_id
            && self.model_revision == query.model_revision
            && self.tokenizer_digest == query.tokenizer_digest
            && same_profile
            && (same_model || approved_jina_split)
    }
}

fn digest_is(digest: &ContentDigest, expected_hex: &str) -> bool {
    digest
        .as_str()
        .strip_prefix("sha256:")
        .is_some_and(|actual| actual == expected_hex)
}

impl EmbeddingContract {
    fn digest(&self) -> Result<ContentDigest, serde_json::Error> {
        Ok(ContentDigest::of(&serde_json::to_vec(self)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HistoryVectorBuildObservations {
    pub unique_inputs: usize,
    pub cache_hits: usize,
    pub provider_inputs: usize,
    pub avoided_provider_inputs: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryVectorIndex {
    pub schema: u16,
    pub contract: EmbeddingContract,
    pub dimension: usize,
    pub keys: Vec<ContentDigest>,
    pub values: Vec<f32>,
    pub occurrences: Vec<HistoryOccurrence>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistorySemanticHit {
    pub occurrence: HistoryOccurrence,
    pub similarity: f32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheRecord {
    key: ContentDigest,
    vector: Vec<f32>,
}

impl HistoryVectorIndex {
    /// Embeds only cache misses, appending each completed batch to a durable log.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when the contract cache is incompatible,
    /// persistence fails, or the provider returns invalid vectors.
    pub fn build_with_cache<P: EmbeddingProvider + ?Sized>(
        provider: &mut P,
        history: &TaggedHistory,
        contract: EmbeddingContract,
        cache_root: &Path,
    ) -> Result<(Self, HistoryVectorBuildObservations), HistoryError> {
        let namespace = contract.digest()?.as_str()["sha256:".len()..].to_owned();
        let cache_dir = cache_root.join(namespace);
        fs::create_dir_all(&cache_dir)?;
        ensure_cache_contract(&cache_dir, &contract)?;
        let log_path = cache_dir.join("vectors.jsonl");
        let mut cache = load_cache(&log_path, contract.profile.dimension)?;
        let mut observations = HistoryVectorBuildObservations {
            unique_inputs: history.contents.len(),
            ..HistoryVectorBuildObservations::default()
        };
        observations.cache_hits = history
            .contents
            .iter()
            .filter(|content| cache.contains_key(&content.vector_key))
            .count();
        observations.avoided_provider_inputs = observations.cache_hits;

        let missing = history
            .contents
            .iter()
            .filter(|content| !cache.contains_key(&content.vector_key))
            .collect::<Vec<_>>();
        let batch_size = provider.embedding_batch_size().get();
        for batch in missing.chunks(batch_size) {
            let texts = batch
                .iter()
                .map(|content| content.embedding_text.clone())
                .collect::<Vec<_>>();
            let mut vectors = provider.embed_documents(&texts)?;
            if vectors.len() != batch.len() {
                return Err(HistoryError::ProviderCount {
                    expected: batch.len(),
                    actual: vectors.len(),
                });
            }
            observations.provider_inputs = observations.provider_inputs.saturating_add(batch.len());
            for vector in &mut vectors {
                if vector.len() != contract.profile.dimension {
                    return Err(HistoryError::CacheDimension {
                        key: batch[0].vector_key.clone(),
                        expected: contract.profile.dimension,
                        actual: vector.len(),
                    });
                }
                match provider.output_normalization() {
                    OutputNormalization::Guaranteed => validate_normalized(vector)?,
                    OutputNormalization::Unknown => normalize(vector)?,
                }
            }
            append_cache_batch(&log_path, batch, &vectors)?;
            for (content, vector) in batch.iter().zip(vectors) {
                cache.insert(content.vector_key.clone(), vector);
            }
        }

        let keys = history
            .contents
            .iter()
            .map(|content| content.vector_key.clone())
            .collect::<Vec<_>>();
        let mut values = Vec::with_capacity(keys.len().saturating_mul(contract.profile.dimension));
        for key in &keys {
            values.extend_from_slice(&cache[key]);
        }
        let index = Self {
            schema: 1,
            dimension: contract.profile.dimension,
            contract,
            keys,
            values,
            occurrences: history.occurrences.clone(),
        };
        index.validate()?;
        Ok((index, observations))
    }

    #[must_use]
    pub fn unique_vector_count(&self) -> usize {
        self.keys.len()
    }

    #[must_use]
    pub fn occurrence_count(&self) -> usize {
        self.occurrences.len()
    }

    /// Scores unique vectors before expanding version occurrences.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when the query contract or dimension differs
    /// from the index.
    pub fn search(
        &self,
        query: &[f32],
        query_contract: &EmbeddingContract,
        tag: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistorySemanticHit>, HistoryError> {
        if !self.contract.supports_query(query_contract) {
            return Err(HistoryError::QueryContractMismatch);
        }
        if query.len() != self.dimension {
            return Err(HistoryError::CacheDimension {
                key: ContentDigest::of(b"query"),
                expected: self.dimension,
                actual: query.len(),
            });
        }
        let mut scored = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let start = index * self.dimension;
                let score = query
                    .iter()
                    .zip(&self.values[start..start + self.dimension])
                    .map(|(left, right)| left * right)
                    .sum::<f32>();
                (key, score)
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(left_key, left), (right_key, right)| {
            right.total_cmp(left).then_with(|| left_key.cmp(right_key))
        });
        let mut hits = Vec::new();
        for (key, similarity) in scored {
            for occurrence in self.occurrences.iter().filter(|occurrence| {
                occurrence.vector_key == *key && tag.is_none_or(|tag| occurrence.tag == tag)
            }) {
                hits.push(HistorySemanticHit {
                    occurrence: occurrence.clone(),
                    similarity,
                });
                if hits.len() == limit {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }

    /// Writes the vector index atomically.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when serialization, writing, syncing, or renaming fails.
    pub fn write_artifact(&self, path: &Path) -> Result<(), HistoryError> {
        self.validate()?;
        write_json_atomically(path, self)
    }

    /// Reads a vector index artifact.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when the file cannot be read or decoded.
    pub fn read_artifact(path: &Path) -> Result<Self, HistoryError> {
        let artifact: Self = serde_json::from_slice(&fs::read(path)?)?;
        artifact.validate()?;
        Ok(artifact)
    }

    fn validate(&self) -> Result<(), HistoryError> {
        if self.schema != 1
            || self.dimension == 0
            || self.dimension != self.contract.profile.dimension
        {
            return Err(HistoryError::InvalidArtifact(
                "schema or dimension does not match its contract".into(),
            ));
        }
        let expected_values = self
            .keys
            .len()
            .checked_mul(self.dimension)
            .ok_or_else(|| HistoryError::InvalidArtifact("vector size overflow".into()))?;
        if self.values.len() != expected_values
            || self.keys.windows(2).any(|keys| keys[0] >= keys[1])
        {
            return Err(HistoryError::InvalidArtifact(
                "vector rows and sorted unique keys disagree".into(),
            ));
        }
        let key_set = self.keys.iter().collect::<BTreeSet<_>>();
        if self
            .occurrences
            .iter()
            .any(|occurrence| !key_set.contains(&occurrence.vector_key))
        {
            return Err(HistoryError::InvalidArtifact(
                "an occurrence references a missing vector".into(),
            ));
        }
        for vector in self.values.chunks_exact(self.dimension) {
            validate_normalized(vector)?;
        }
        Ok(())
    }
}

fn ensure_cache_contract(
    cache_dir: &Path,
    contract: &EmbeddingContract,
) -> Result<(), HistoryError> {
    let path = cache_dir.join("contract.json");
    if path.exists() {
        let existing: EmbeddingContract = serde_json::from_slice(&fs::read(path)?)?;
        if existing != *contract {
            return Err(HistoryError::CacheContractMismatch);
        }
        return Ok(());
    }
    write_json_atomically(&path, contract)
}

fn load_cache(
    path: &Path,
    expected_dimension: usize,
) -> Result<BTreeMap<ContentDigest, Vec<f32>>, HistoryError> {
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut cache = BTreeMap::new();
    let mut valid_len = 0_usize;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let Ok(record) = serde_json::from_slice::<CacheRecord>(&line[..line.len() - 1]) else {
            break;
        };
        if record.vector.len() != expected_dimension {
            return Err(HistoryError::CacheDimension {
                key: record.key,
                expected: expected_dimension,
                actual: record.vector.len(),
            });
        }
        validate_normalized(&record.vector)?;
        valid_len += line.len();
        cache.insert(record.key, record.vector);
    }
    if valid_len != bytes.len() {
        file.set_len(u64::try_from(valid_len).unwrap_or(u64::MAX))?;
    }
    Ok(cache)
}

fn append_cache_batch(
    path: &Path,
    contents: &[&HistoryContent],
    vectors: &[Vec<f32>],
) -> Result<(), HistoryError> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for (content, vector) in contents.iter().zip(vectors) {
        serde_json::to_writer(
            &mut file,
            &CacheRecord {
                key: content.vector_key.clone(),
                vector: vector.clone(),
            },
        )?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    file.sync_data()?;
    Ok(())
}

fn normalize(vector: &mut [f32]) -> Result<(), HistoryError> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(HistoryError::InvalidVector);
    }
    for value in vector {
        *value /= norm;
    }
    Ok(())
}

fn validate_normalized(vector: &[f32]) -> Result<(), HistoryError> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if vector.iter().all(|value| value.is_finite()) && (norm - 1.0).abs() <= 1e-3 {
        Ok(())
    } else {
        Err(HistoryError::InvalidVector)
    }
}

fn write_json_atomically<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
) -> Result<(), HistoryError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temporary)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    serde_json::to_writer(&mut writer, value)?;
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    file.sync_data()?;
    fs::rename(temporary, path)?;
    if let Some(parent) = path.parent() {
        let directory = OpenOptions::new().read(true).open(parent)?;
        directory.sync_data()?;
    }
    Ok(())
}
