//! Incremental, content-addressed ingestion of complete reachable Git history.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::chunking::{ChunkingConfig, chunk_document};
use super::git::{
    CachedGitBlob, Candidate, GitCorpusPolicy, GitCorpusSource, GitIngestionError,
    GitIngestionObservations, document_from_git_blob, identifiers, is_selected, load_git_blob,
    validate_policy,
};
use super::{Chunk, ContentDigest, RepositoryId, Revision};
use crate::semantic::embedding_text;

const HISTORY_SCHEMA: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHistoryTip {
    pub repository: RepositoryId,
    pub revision: Revision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHistoryCommit {
    pub repository: RepositoryId,
    pub revision: Revision,
    pub parents: Vec<Revision>,
    pub commit_time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHistoryRef {
    pub repository: RepositoryId,
    pub name: String,
    pub revision: Revision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHistoryChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHistoryOccurrence {
    pub repository: RepositoryId,
    pub revision: Revision,
    pub path: String,
    pub kind: GitHistoryChangeKind,
    pub content_keys: Vec<ContentDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHistoryContent {
    pub key: ContentDigest,
    pub embedding_key: ContentDigest,
    pub chunk: Chunk,
}

/// One logical, deterministic history set spanning every configured repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHistory {
    pub schema: u16,
    pub tips: Vec<GitHistoryTip>,
    pub commits: Vec<GitHistoryCommit>,
    pub refs: Vec<GitHistoryRef>,
    pub contents: Vec<GitHistoryContent>,
    pub occurrences: Vec<GitHistoryOccurrence>,
}

impl GitHistory {
    /// Writes the deterministic history artifact.
    ///
    /// # Errors
    ///
    /// Returns [`GitHistoryError`] when serialization or writing fails.
    pub fn write_artifact(&self, path: &Path) -> Result<(), GitHistoryError> {
        self.validate()?;
        let mut writer = BufWriter::new(File::create(path)?);
        serde_json::to_writer(&mut writer, self)?;
        writer.flush()?;
        Ok(())
    }

    /// Reads and validates a history artifact.
    ///
    /// # Errors
    ///
    /// Returns [`GitHistoryError`] when reading, decoding, or schema validation fails.
    pub fn read_artifact(path: &Path) -> Result<Self, GitHistoryError> {
        let history: Self = serde_json::from_slice(&fs::read(path)?)?;
        history.validate()?;
        Ok(history)
    }

    /// Checks referential integrity and content-addressed identities.
    ///
    /// # Errors
    ///
    /// Returns [`GitHistoryError::Invalid`] for a corrupt or inconsistent artifact.
    pub fn validate(&self) -> Result<(), GitHistoryError> {
        if self.schema != HISTORY_SCHEMA {
            return Err(GitHistoryError::Schema(self.schema));
        }
        let commits = self
            .commits
            .iter()
            .map(|commit| (commit.repository.clone(), commit.revision.clone()))
            .collect::<BTreeSet<_>>();
        if commits.len() != self.commits.len() {
            return Err(GitHistoryError::Invalid("duplicate commit identity".into()));
        }
        if self.commits.iter().any(|commit| {
            commit
                .parents
                .iter()
                .any(|parent| !commits.contains(&(commit.repository.clone(), parent.clone())))
        }) {
            return Err(GitHistoryError::Invalid(
                "commit names an unreachable parent".into(),
            ));
        }
        if self
            .tips
            .iter()
            .any(|tip| !commits.contains(&(tip.repository.clone(), tip.revision.clone())))
        {
            return Err(GitHistoryError::Invalid(
                "tip does not name a reachable commit".into(),
            ));
        }
        if self.refs.iter().any(|reference| {
            !commits.contains(&(reference.repository.clone(), reference.revision.clone()))
        }) {
            return Err(GitHistoryError::Invalid(
                "ref does not name a reachable commit".into(),
            ));
        }
        let contents = self
            .contents
            .iter()
            .map(|content| (content.key.clone(), content))
            .collect::<BTreeMap<_, _>>();
        if contents.len() != self.contents.len() {
            return Err(GitHistoryError::Invalid(
                "duplicate content identity".into(),
            ));
        }
        let mut referenced = BTreeSet::new();
        for occurrence in &self.occurrences {
            if !commits.contains(&(occurrence.repository.clone(), occurrence.revision.clone())) {
                return Err(GitHistoryError::Invalid(
                    "occurrence does not name a reachable commit".into(),
                ));
            }
            for key in &occurrence.content_keys {
                let Some(content) = contents.get(key) else {
                    return Err(GitHistoryError::Invalid(
                        "occurrence names missing content".into(),
                    ));
                };
                if content.embedding_key
                    != ContentDigest::of(embedding_text(&content.chunk).as_bytes())
                    || content.key
                        != occurrence_content_key(
                            &occurrence.repository,
                            &occurrence.path,
                            &content.embedding_key,
                        )
                {
                    return Err(GitHistoryError::Invalid(
                        "content-addressed identity mismatch".into(),
                    ));
                }
                referenced.insert(key.clone());
            }
        }
        if referenced.len() != contents.len() {
            return Err(GitHistoryError::Invalid("unreferenced content".into()));
        }
        Ok(())
    }

    #[must_use]
    pub fn chunks(&self) -> Vec<Chunk> {
        self.contents
            .iter()
            .map(|content| content.chunk.clone())
            .collect()
    }
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
    #[error("unsupported Git history schema {0}")]
    Schema(u16),
    #[error("invalid Git history artifact: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug)]
struct ReachableCommit {
    revision: Revision,
    parents: Vec<Revision>,
    commit_time: i64,
}

#[derive(Debug)]
enum PendingChange {
    Upsert {
        path: String,
        id: gix::ObjectId,
        kind: GitHistoryChangeKind,
    },
    Delete {
        path: String,
    },
}

struct HistoryAccumulator {
    commits: BTreeMap<(RepositoryId, Revision), GitHistoryCommit>,
    contents: BTreeMap<ContentDigest, GitHistoryContent>,
    occurrences: Vec<GitHistoryOccurrence>,
    tips: Vec<GitHistoryTip>,
    refs: Vec<GitHistoryRef>,
    reachable_keys: BTreeSet<(RepositoryId, Revision)>,
    observations: GitHistoryRefreshObservations,
}

impl HistoryAccumulator {
    fn new(previous: Option<GitHistory>, source_count: usize) -> Self {
        let (commits, contents, occurrences) = previous
            .filter(|history| history.schema == HISTORY_SCHEMA)
            .map_or_else(
                || (BTreeMap::new(), BTreeMap::new(), Vec::new()),
                |history| {
                    (
                        history
                            .commits
                            .into_iter()
                            .map(|commit| {
                                ((commit.repository.clone(), commit.revision.clone()), commit)
                            })
                            .collect(),
                        history
                            .contents
                            .into_iter()
                            .map(|content| (content.key.clone(), content))
                            .collect(),
                        history.occurrences,
                    )
                },
            );
        Self {
            commits,
            contents,
            occurrences,
            tips: Vec::with_capacity(source_count),
            refs: Vec::new(),
            reachable_keys: BTreeSet::new(),
            observations: GitHistoryRefreshObservations::default(),
        }
    }

    fn ingest_source(&mut self, source: &GitCorpusSource) -> Result<(), GitHistoryError> {
        validate_policy(&source.policy)?;
        let repo = gix::open(&source.repository_path)
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let tip_id = gix::ObjectId::from_hex(source.revision.as_str().as_bytes())
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let reachable = reachable_commits(&repo, tip_id)?;
        self.observations.reachable_commits = self
            .observations
            .reachable_commits
            .saturating_add(reachable.len());
        let reachable_revisions = reachable
            .iter()
            .map(|commit| commit.revision.clone())
            .collect::<BTreeSet<_>>();
        self.reachable_keys.extend(
            reachable_revisions
                .iter()
                .cloned()
                .map(|revision| (source.repository_id.clone(), revision)),
        );
        self.tips.push(GitHistoryTip {
            repository: source.repository_id.clone(),
            revision: source.revision.clone(),
        });
        self.refs.extend(reachable_refs(
            &repo,
            &source.repository_id,
            &reachable_revisions,
        )?);

        for commit in reachable {
            let key = (source.repository_id.clone(), commit.revision.clone());
            if self.commits.contains_key(&key) {
                self.observations.reused_commits =
                    self.observations.reused_commits.saturating_add(1);
                continue;
            }
            ingest_commit_changes(
                &repo,
                source,
                &commit,
                &reachable_revisions,
                &mut self.contents,
                &mut self.occurrences,
                &mut self.observations,
            )?;
            self.commits.insert(
                key,
                GitHistoryCommit {
                    repository: source.repository_id.clone(),
                    revision: commit.revision,
                    parents: commit.parents,
                    commit_time: commit.commit_time,
                },
            );
            self.observations.ingested_commits =
                self.observations.ingested_commits.saturating_add(1);
        }
        Ok(())
    }

    fn finish(mut self) -> (GitHistory, GitHistoryRefreshObservations) {
        self.commits
            .retain(|key, _| self.reachable_keys.contains(key));
        self.occurrences.retain(|occurrence| {
            self.reachable_keys
                .contains(&(occurrence.repository.clone(), occurrence.revision.clone()))
        });
        self.occurrences.sort_by(|left, right| {
            left.repository
                .cmp(&right.repository)
                .then_with(|| left.revision.cmp(&right.revision))
                .then_with(|| left.path.cmp(&right.path))
        });
        self.occurrences.dedup();
        let referenced = self
            .occurrences
            .iter()
            .flat_map(|occurrence| occurrence.content_keys.iter().cloned())
            .collect::<BTreeSet<_>>();
        self.contents.retain(|key, _| referenced.contains(key));
        self.tips
            .sort_by(|left, right| left.repository.cmp(&right.repository));
        self.refs.sort_by(|left, right| {
            left.repository
                .cmp(&right.repository)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.revision.cmp(&right.revision))
        });
        self.refs.dedup();
        (
            GitHistory {
                schema: HISTORY_SCHEMA,
                tips: self.tips,
                commits: self.commits.into_values().collect(),
                refs: self.refs,
                contents: self.contents.into_values().collect(),
                occurrences: self.occurrences,
            },
            self.observations,
        )
    }
}

/// Refresh a deterministic combined history, processing only newly reachable commits.
///
/// The commit graph is walked to establish the authoritative reachable set, but
/// blobs and chunks from commits present in `previous` are reused verbatim.
///
/// # Errors
///
/// Returns [`GitHistoryError`] for invalid policies, missing Git objects,
/// tree-diff failures, invalid source content, or chunking failures.
pub fn ingest_git_history(
    sources: &[GitCorpusSource],
    previous: Option<&GitHistory>,
) -> Result<(GitHistory, GitHistoryRefreshObservations), GitHistoryError> {
    ingest_git_history_owned(sources, previous.cloned())
}

pub(crate) fn ingest_git_history_owned(
    sources: &[GitCorpusSource],
    previous: Option<GitHistory>,
) -> Result<(GitHistory, GitHistoryRefreshObservations), GitHistoryError> {
    let mut ordered_sources = sources.iter().collect::<Vec<_>>();
    ordered_sources.sort_by(|left, right| left.repository_id.cmp(&right.repository_id));
    if ordered_sources
        .windows(2)
        .any(|pair| pair[0].repository_id == pair[1].repository_id)
    {
        return Err(GitHistoryError::Invalid(
            "repository identities must be unique".into(),
        ));
    }
    let mut accumulator = HistoryAccumulator::new(previous, ordered_sources.len());
    for source in ordered_sources {
        accumulator.ingest_source(source)?;
    }
    Ok(accumulator.finish())
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
            let revision = Revision::try_from(info.id.to_string())
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let parents = commit
                .parent_ids()
                .map(|parent| Revision::try_from(parent.to_string()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let commit_time = commit
                .time()
                .map_err(|error| GitHistoryError::Git(error.to_string()))?
                .seconds;
            Ok(ReachableCommit {
                revision,
                parents,
                commit_time,
            })
        })
        .collect::<Result<Vec<_>, GitHistoryError>>()?;
    commits.reverse();
    Ok(commits)
}

fn reachable_refs(
    repo: &gix::Repository,
    repository: &RepositoryId,
    reachable: &BTreeSet<Revision>,
) -> Result<Vec<GitHistoryRef>, GitHistoryError> {
    let references = repo
        .references()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let refs = references
        .all()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let mut output = Vec::new();
    for reference in refs {
        let mut reference = reference.map_err(|error| GitHistoryError::Git(error.to_string()))?;
        let Ok(commit) = reference.peel_to_commit() else {
            continue;
        };
        let revision = Revision::try_from(commit.id.to_string())
            .map_err(|error| GitHistoryError::Git(error.to_string()))?;
        if reachable.contains(&revision) {
            output.push(GitHistoryRef {
                repository: repository.clone(),
                name: reference.name().as_bstr().to_string(),
                revision,
            });
        }
    }
    Ok(output)
}

fn ingest_commit_changes(
    repo: &gix::Repository,
    source: &GitCorpusSource,
    commit: &ReachableCommit,
    reachable_revisions: &BTreeSet<Revision>,
    contents: &mut BTreeMap<ContentDigest, GitHistoryContent>,
    occurrences: &mut Vec<GitHistoryOccurrence>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<(), GitHistoryError> {
    let commit_id = gix::ObjectId::from_hex(commit.revision.as_str().as_bytes())
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let current_commit = repo
        .find_commit(commit_id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let current = current_commit
        .tree()
        .map_err(|error| GitHistoryError::Git(error.to_string()))?;
    let previous = commit.parents.first().map_or_else(
        || Ok(repo.empty_tree()),
        |parent| {
            let id = gix::ObjectId::from_hex(parent.as_str().as_bytes())
                .map_err(|error| GitHistoryError::Git(error.to_string()))?;
            let parent = repo
                .find_commit(id)
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

    for change in pending {
        match change {
            PendingChange::Delete { path } => occurrences.push(GitHistoryOccurrence {
                repository: source.repository_id.clone(),
                revision: commit.revision.clone(),
                path,
                kind: GitHistoryChangeKind::Deleted,
                content_keys: Vec::new(),
            }),
            PendingChange::Upsert { path, id, kind } => {
                let Some(occurrence) = ingest_upsert(
                    repo,
                    source,
                    commit,
                    reachable_revisions,
                    path,
                    id,
                    kind,
                    contents,
                    observations,
                )?
                else {
                    continue;
                };
                occurrences.push(occurrence);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ingest_upsert(
    repo: &gix::Repository,
    source: &GitCorpusSource,
    commit: &ReachableCommit,
    reachable_revisions: &BTreeSet<Revision>,
    path: String,
    id: gix::ObjectId,
    kind: GitHistoryChangeKind,
    contents: &mut BTreeMap<ContentDigest, GitHistoryContent>,
    observations: &mut GitHistoryRefreshObservations,
) -> Result<Option<GitHistoryOccurrence>, GitHistoryError> {
    let size = repo
        .find_header(id)
        .map_err(|error| GitHistoryError::Git(error.to_string()))?
        .size();
    let candidate = Candidate {
        path: path.clone(),
        id,
        size,
    };
    let blob = load_git_blob(repo, &candidate, &mut observations.git)?;
    if matches!(blob, CachedGitBlob::Rejected { .. }) {
        return Ok(Some(GitHistoryOccurrence {
            repository: source.repository_id.clone(),
            revision: commit.revision.clone(),
            path,
            kind,
            content_keys: Vec::new(),
        }));
    }
    observations.ingested_blobs = observations.ingested_blobs.saturating_add(1);
    let document = document_from_git_blob(
        &path,
        &source.repository_id,
        &commit.revision,
        source.trust_tier,
        &source.license,
        blob,
    )?;
    let mut chunks = chunk_document(&document, ChunkingConfig::default())
        .map_err(|error| GitHistoryError::Chunking(error.to_string()))?;
    for chunk in &mut chunks {
        chunk.identifiers = identifiers(&path, &chunk.text);
    }
    let mut content_keys = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let embedding_key = ContentDigest::of(embedding_text(&chunk).as_bytes());
        let key = occurrence_content_key(&source.repository_id, &path, &embedding_key);
        if let Some(existing) = contents.get_mut(&key) {
            observations.reused_contents = observations.reused_contents.saturating_add(1);
            if !reachable_revisions.contains(&existing.chunk.revision) {
                existing.chunk = chunk;
            }
        } else {
            contents.insert(
                key.clone(),
                GitHistoryContent {
                    key: key.clone(),
                    embedding_key,
                    chunk,
                },
            );
        }
        content_keys.push(key);
    }
    Ok(Some(GitHistoryOccurrence {
        repository: source.repository_id.clone(),
        revision: commit.revision.clone(),
        path,
        kind,
        content_keys,
    }))
}

fn occurrence_content_key(
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
        } if entry_mode.is_blob() => push_upsert(
            location.to_string(),
            id.detach(),
            GitHistoryChangeKind::Added,
            policy,
            pending,
        ),
        Change::Modification {
            location,
            entry_mode,
            id,
            ..
        } if entry_mode.is_blob() => push_upsert(
            location.to_string(),
            id.detach(),
            GitHistoryChangeKind::Modified,
            policy,
            pending,
        ),
        Change::Deletion {
            location,
            entry_mode,
            ..
        } if entry_mode.is_blob() => {
            let path = location.to_string();
            if is_selected(&path, policy) {
                pending.push(PendingChange::Delete { path });
            }
        }
        _ => {}
    }
}

fn push_upsert(
    path: String,
    id: gix::ObjectId,
    kind: GitHistoryChangeKind,
    policy: &GitCorpusPolicy,
    pending: &mut Vec<PendingChange>,
) {
    if is_selected(&path, policy) {
        pending.push(PendingChange::Upsert { path, id, kind });
    }
}

fn pending_path(change: &PendingChange) -> &str {
    match change {
        PendingChange::Upsert { path, .. } | PendingChange::Delete { path } => path,
    }
}
