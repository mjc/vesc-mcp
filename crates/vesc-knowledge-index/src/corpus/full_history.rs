//! Incremental, content-addressed ingestion of complete reachable Git history.

use std::collections::{BTreeMap, BTreeSet};

use super::chunking::{ChunkingConfig, chunk_document};
use super::git::{
    CachedGitBlob, Candidate, GitCorpusPolicy, GitCorpusSource, GitIngestionError,
    GitIngestionObservations, document_from_git_blob, identifiers, is_selected, load_git_blob,
    validate_policy,
};
use super::{Chunk, ContentDigest, RepositoryId, Revision, SourceKind};
use crate::semantic::embedding_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHistoryTip {
    pub repository: RepositoryId,
    pub revision: Revision,
}

/// Work performed by one refresh. These counters do not affect artifact identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitHistoryRefreshObservations {
    pub reachable_commits: usize,
    pub reused_commits: usize,
    pub ingested_commits: usize,
    pub ingested_blobs: usize,
    pub reused_contents: usize,
    pub git: GitIngestionObservations,
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

#[derive(Debug)]
struct PendingChange {
    path: String,
    id: gix::ObjectId,
}

enum HistoryContents<'a> {
    All(BTreeMap<ContentDigest, Chunk>),
    Delta {
        previous_contains:
            &'a mut dyn FnMut(&Chunk, &ContentDigest) -> Result<bool, GitHistoryError>,
        chunks: BTreeMap<ContentDigest, Chunk>,
    },
}

impl HistoryContents<'_> {
    fn insert(
        &mut self,
        key: ContentDigest,
        chunk: Chunk,
        reachable_revisions: &BTreeSet<gix::ObjectId>,
        observations: &mut GitHistoryRefreshObservations,
    ) -> Result<(), GitHistoryError> {
        match self {
            Self::All(contents) => {
                if let Some(existing) = contents.get_mut(&key) {
                    observations.reused_contents = observations.reused_contents.saturating_add(1);
                    let existing_is_reachable =
                        gix::ObjectId::from_hex(existing.revision.as_str().as_bytes())
                            .is_ok_and(|id| reachable_revisions.contains(&id));
                    if !existing_is_reachable {
                        *existing = chunk;
                    }
                } else {
                    contents.insert(key, chunk);
                }
            }
            Self::Delta {
                previous_contains,
                chunks,
            } => {
                if chunks.contains_key(&key) || previous_contains(&chunk, &key)? {
                    observations.reused_contents = observations.reused_contents.saturating_add(1);
                } else {
                    chunks.insert(key, chunk);
                }
            }
        }
        Ok(())
    }

    fn into_chunks(self) -> Vec<Chunk> {
        match self {
            Self::All(chunks) | Self::Delta { chunks, .. } => chunks.into_values().collect(),
        }
    }
}

/// Reuse cached Git chunks when every configured tip is a fast-forward.
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
    ingest_git_history_fast_forward_from_chunks(
        sources,
        previous_tips,
        cached_chunks.iter().cloned(),
    )
}

pub(crate) fn ingest_git_history_fast_forward_owned(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_chunks: Vec<Chunk>,
) -> Result<Option<(Vec<Chunk>, GitHistoryRefreshObservations)>, GitHistoryError> {
    ingest_git_history_fast_forward_from_chunks(sources, previous_tips, cached_chunks)
}

fn ingest_git_history_fast_forward_from_chunks(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    cached_chunks: impl IntoIterator<Item = Chunk>,
) -> Result<Option<(Vec<Chunk>, GitHistoryRefreshObservations)>, GitHistoryError> {
    let repositories = sources
        .iter()
        .map(|source| source.repository_id.clone())
        .collect::<BTreeSet<_>>();
    let contents = cached_chunks
        .into_iter()
        .filter(|chunk| {
            chunk.source_kind == SourceKind::GitBlob
                && repositories.contains(&chunk.repository)
                && previous_tips
                    .iter()
                    .any(|tip| tip.repository == chunk.repository)
        })
        .map(|chunk| {
            (
                history_content_key_for_chunk(&chunk)
                    .expect("filtered Git-history chunk has a content key"),
                chunk,
            )
        })
        .collect();
    ingest_git_history_fast_forward_with_contents(
        sources,
        previous_tips,
        HistoryContents::All(contents),
    )
}

pub(crate) fn ingest_git_history_fast_forward_delta(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    previous_contains: &mut dyn FnMut(&Chunk, &ContentDigest) -> Result<bool, GitHistoryError>,
) -> Result<Option<(Vec<Chunk>, GitHistoryRefreshObservations)>, GitHistoryError> {
    ingest_git_history_fast_forward_with_contents(
        sources,
        previous_tips,
        HistoryContents::Delta {
            previous_contains,
            chunks: BTreeMap::new(),
        },
    )
}

fn ingest_git_history_fast_forward_with_contents(
    sources: &[GitCorpusSource],
    previous_tips: &[GitHistoryTip],
    mut contents: HistoryContents<'_>,
) -> Result<Option<(Vec<Chunk>, GitHistoryRefreshObservations)>, GitHistoryError> {
    let tips = previous_tips
        .iter()
        .map(|tip| (tip.repository.clone(), tip.revision.clone()))
        .collect::<BTreeMap<_, _>>();
    let repositories = sources
        .iter()
        .map(|source| source.repository_id.clone())
        .collect::<BTreeSet<_>>();
    if repositories.len() != sources.len() {
        return Ok(None);
    }
    if tips
        .keys()
        .any(|repository| !repositories.contains(repository))
    {
        return Ok(None);
    }
    let mut observations = GitHistoryRefreshObservations::default();
    let mut ordered_sources = sources.iter().collect::<Vec<_>>();
    ordered_sources.sort_by(|left, right| {
        tips.contains_key(&right.repository_id)
            .cmp(&tips.contains_key(&left.repository_id))
            .then_with(|| left.repository_id.cmp(&right.repository_id))
    });
    let mut processed = Vec::<(&GitCorpusSource, BTreeSet<gix::ObjectId>)>::new();
    for source in ordered_sources {
        validate_policy(&source.policy)?;
        let repo = gix::open(&source.repository_path)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let current_id = gix::ObjectId::from_hex(source.revision.as_str().as_bytes())
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let current = reachable_commits(&repo, current_id)?;
        let reachable_revisions = current
            .iter()
            .map(|commit| commit.id)
            .collect::<BTreeSet<_>>();
        let mut previous = if let Some(previous_revision) = tips.get(&source.repository_id) {
            let previous_id = gix::ObjectId::from_hex(previous_revision.as_str().as_bytes())
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            if !reachable_revisions.contains(&previous_id) {
                return Ok(None);
            }
            reachable_commits(&repo, previous_id)?
                .into_iter()
                .map(|commit| commit.id)
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        previous.extend(
            processed
                .iter()
                .filter(|(known, _)| same_corpus_contract(source, known))
                .flat_map(|(_, revisions)| revisions)
                .filter(|revision| reachable_revisions.contains(*revision))
                .copied(),
        );
        observations.reachable_commits =
            observations.reachable_commits.saturating_add(current.len());
        observations.reused_commits = observations.reused_commits.saturating_add(previous.len());
        for commit in current
            .iter()
            .filter(|commit| !previous.contains(&commit.id))
        {
            ingest_commit_changes(
                &repo,
                source,
                commit,
                &reachable_revisions,
                &mut contents,
                &mut observations,
            )?;
            observations.ingested_commits = observations.ingested_commits.saturating_add(1);
            #[cfg(feature = "coz-profile")]
            coz::progress!("git_history_ingested_commit");
        }
        processed.push((source, reachable_revisions));
    }
    Ok(Some((contents.into_chunks(), observations)))
}

fn same_corpus_contract(left: &GitCorpusSource, right: &GitCorpusSource) -> bool {
    left.trust_tier == right.trust_tier
        && left.license == right.license
        && left.policy == right.policy
}

fn reachable_commits(
    repo: &gix::Repository,
    tip: gix::ObjectId,
) -> Result<Vec<ReachableCommit>, GitHistoryError> {
    let walk = repo
        .rev_walk([tip])
        .all()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut commits = walk
        .map(|info| {
            let info = info.map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let commit = info
                .object()
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let first_parent = commit.parent_ids().next().map(gix::Id::detach);
            #[cfg(feature = "coz-profile")]
            coz::progress!("git_history_walk_commit");
            Ok(ReachableCommit {
                id: info.id,
                first_parent,
            })
        })
        .collect::<Result<Vec<_>, GitHistoryError>>()?;
    commits.reverse();
    Ok(commits)
}

fn ingest_commit_changes(
    repo: &gix::Repository,
    source: &GitCorpusSource,
    commit: &ReachableCommit,
    reachable_revisions: &BTreeSet<gix::ObjectId>,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<(), GitHistoryError> {
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
    previous
        .changes()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?
        .options(|options| {
            options.track_path();
            options.track_rewrites(None);
        })
        .for_each_to_obtain_tree(&current, |change| {
            collect_change(change, &source.policy, &mut pending);
            Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    pending.sort_by(|left, right| pending_path(left).cmp(pending_path(right)));

    let revision = Revision::try_from(commit.id.to_string())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    for PendingChange { path, id } in pending {
        ingest_upsert(
            repo,
            source,
            &revision,
            reachable_revisions,
            &path,
            id,
            contents,
            observations,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ingest_upsert(
    repo: &gix::Repository,
    source: &GitCorpusSource,
    revision: &Revision,
    reachable_revisions: &BTreeSet<gix::ObjectId>,
    path: &str,
    id: gix::ObjectId,
    contents: &mut HistoryContents<'_>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<(), GitHistoryError> {
    let size = repo
        .find_header(id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?
        .size();
    let candidate = Candidate {
        path: path.to_string(),
        id,
        size,
    };
    // History search uses chunk-local identifiers below. Avoid building and
    // cloning a file-wide identifier set into every chunk only to overwrite it.
    let blob = load_git_blob(repo, &candidate, &mut observations.git, false)?;
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
    let mut chunks = chunk_document(&document, ChunkingConfig::default())
        .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
    for chunk in &mut chunks {
        chunk.identifiers = identifiers(path, &chunk.text);
    }
    for chunk in chunks {
        let embedding_key = ContentDigest::of(embedding_text(&chunk).as_bytes());
        let key = history_content_key(&source.repository_id, path, &embedding_key);
        contents.insert(key, chunk, reachable_revisions, observations)?;
    }
    #[cfg(feature = "coz-profile")]
    coz::progress!("git_history_ingested_blob");
    Ok(())
}

fn history_content_key(
    repository: &RepositoryId,
    path: &str,
    embedding_key: &ContentDigest,
) -> ContentDigest {
    let mut identity = String::with_capacity(
        repository.as_str().len() + path.len() + embedding_key.as_str().len() + 2,
    );
    identity.push_str(repository.as_str());
    identity.push('\0');
    identity.push_str(path);
    identity.push('\0');
    identity.push_str(embedding_key.as_str());
    ContentDigest::of(identity.as_bytes())
}

pub(crate) fn history_content_key_for_chunk(chunk: &Chunk) -> Option<ContentDigest> {
    (chunk.source_kind == SourceKind::GitBlob).then(|| {
        let embedding_key = ContentDigest::of(embedding_text(chunk).as_bytes());
        history_content_key(&chunk.repository, &chunk.path, &embedding_key)
    })
}

fn collect_change(
    change: gix::object::tree::diff::Change<'_, '_, '_>,
    policy: &GitCorpusPolicy,
    pending: &mut Vec<PendingChange>,
) {
    use gix::object::tree::diff::Change;
    match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } if entry_mode.is_blob() => {
            push_upsert(location.to_string(), id.detach(), policy, pending);
        }
        Change::Modification {
            location,
            entry_mode,
            id,
            ..
        } if entry_mode.is_blob() => {
            push_upsert(location.to_string(), id.detach(), policy, pending);
        }
        _ => {}
    }
}

fn push_upsert(
    path: String,
    id: gix::ObjectId,
    policy: &GitCorpusPolicy,
    pending: &mut Vec<PendingChange>,
) {
    if is_selected(&path, policy) {
        pending.push(PendingChange { path, id });
    }
}

fn pending_path(change: &PendingChange) -> &str {
    &change.path
}
