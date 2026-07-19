//! Bounded ingestion of immutable Git commit trees without a worktree.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use gix::bstr::ByteSlice;

use super::ingest::{IngestionReport, SourceInventory, SourceRejection, normalize_text_ref};
use super::{
    ContentDigest, CorpusError, LicenseStatus, NormalizedDocument, RepositoryId, Revision,
    SourceKind, SourceSpan, TrustTier,
};

const DEFAULT_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "h", "hh", "hpp", "json", "lisp", "md", "qml", "rs", "toml", "ts", "txt",
    "yaml", "yml",
];
const DEFAULT_FILENAMES: &[&str] = &["CMakeLists.txt", "Kconfig", "Makefile"];
const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "build",
    "dist",
    "target",
    "ChibiOS_3.0.5",
    "lispBM/lispBM/repl/windows",
    "lispBM/lispBM/test_reports",
    "lispBM/c_libs/stdperiph_stm32f4",
    "vesc_pkg_lib/stdperiph_stm32f4",
];

/// Version of the reviewed default code-corpus path and resource policy.
pub const GIT_CORPUS_POLICY_VERSION: &str = "reviewed-v1";

/// Reviewed path and media-type selection for one immutable repository snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCorpusPolicy {
    pub include_prefixes: Vec<String>,
    pub exclude_prefixes: Vec<String>,
    pub extensions: BTreeSet<String>,
    pub filenames: BTreeSet<String>,
}

impl Default for GitCorpusPolicy {
    fn default() -> Self {
        Self {
            include_prefixes: Vec::new(),
            exclude_prefixes: DEFAULT_EXCLUDES.iter().map(ToString::to_string).collect(),
            extensions: DEFAULT_EXTENSIONS.iter().map(ToString::to_string).collect(),
            filenames: DEFAULT_FILENAMES.iter().map(ToString::to_string).collect(),
        }
    }
}

/// One already-managed repository and immutable commit selected for a corpus build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCorpusSource {
    pub repository_path: PathBuf,
    pub repository_id: RepositoryId,
    pub revision: Revision,
    pub trust_tier: TrustTier,
    pub license: LicenseStatus,
    pub policy: GitCorpusPolicy,
}

/// Failures that prevent producing a trustworthy commit-tree corpus.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitIngestionError {
    #[error("open managed Git repository: {0}")]
    Open(String),
    #[error("invalid immutable commit id: {0}")]
    InvalidCommit(String),
    #[error("read immutable commit tree: {0}")]
    ReadTree(String),
    #[error("invalid Git corpus policy: {0}")]
    InvalidPolicy(String),
    #[error(transparent)]
    Contract(#[from] CorpusError),
}

#[derive(Debug)]
struct Candidate {
    path: String,
    id: gix::ObjectId,
    size: u64,
}

#[derive(Debug, Clone)]
enum CachedGitBlob {
    Text {
        content: String,
        digest: ContentDigest,
        media_type: String,
        identifiers: BTreeSet<String>,
        line_count: u32,
    },
    Rejected {
        code: &'static str,
        message: &'static str,
    },
}

#[derive(Debug, Default)]
pub(crate) struct GitIngestionCache {
    blobs: HashMap<(gix::ObjectId, String), CachedGitBlob>,
}

/// Aggregate Git-ingestion work, retained for profiling rather than artifact identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitIngestionObservations {
    pub tree_walk_us: u64,
    pub candidate_sort_us: u64,
    pub blob_load_us: u64,
    pub binary_scan_us: u64,
    pub utf8_normalization_us: u64,
    pub document_metadata_us: u64,
    pub candidate_count: u64,
    pub blob_bytes_loaded: u64,
    pub binary_rejection_count: u64,
    pub encoding_rejection_count: u64,
    #[serde(default)]
    pub blob_cache_hits: u64,
}

impl GitIngestionObservations {
    pub(crate) const fn accumulate(&mut self, other: &Self) {
        self.tree_walk_us = self.tree_walk_us.saturating_add(other.tree_walk_us);
        self.candidate_sort_us = self
            .candidate_sort_us
            .saturating_add(other.candidate_sort_us);
        self.blob_load_us = self.blob_load_us.saturating_add(other.blob_load_us);
        self.binary_scan_us = self.binary_scan_us.saturating_add(other.binary_scan_us);
        self.utf8_normalization_us = self
            .utf8_normalization_us
            .saturating_add(other.utf8_normalization_us);
        self.document_metadata_us = self
            .document_metadata_us
            .saturating_add(other.document_metadata_us);
        self.candidate_count = self.candidate_count.saturating_add(other.candidate_count);
        self.blob_bytes_loaded = self
            .blob_bytes_loaded
            .saturating_add(other.blob_bytes_loaded);
        self.binary_rejection_count = self
            .binary_rejection_count
            .saturating_add(other.binary_rejection_count);
        self.encoding_rejection_count = self
            .encoding_rejection_count
            .saturating_add(other.encoding_rejection_count);
        self.blob_cache_hits = self.blob_cache_hits.saturating_add(other.blob_cache_hits);
    }
}

/// Ingest approved text/code blobs reachable from one exact commit.
///
/// The repository may be bare. Branch and tag resolution belongs to the managed
/// repository layer; this boundary accepts only an immutable object ID.
///
/// # Errors
///
/// Returns [`GitIngestionError`] when the policy is unsafe, the repository or
/// exact commit cannot be read, a snapshot bound is exceeded, or normalized
/// corpus metadata violates its contract.
#[allow(clippy::too_many_lines)]
pub fn ingest_git_commit(
    repository_path: &Path,
    repository_id: &RepositoryId,
    revision: &Revision,
    trust_tier: TrustTier,
    license: &LicenseStatus,
    policy: &GitCorpusPolicy,
) -> Result<IngestionReport, GitIngestionError> {
    ingest_git_commit_inner(
        repository_path,
        repository_id,
        revision,
        trust_tier,
        license,
        policy,
        None,
    )
}

pub(crate) fn ingest_git_commit_cached(
    repository_path: &Path,
    repository_id: &RepositoryId,
    revision: &Revision,
    trust_tier: TrustTier,
    license: &LicenseStatus,
    policy: &GitCorpusPolicy,
    cache: &mut GitIngestionCache,
) -> Result<IngestionReport, GitIngestionError> {
    ingest_git_commit_inner(
        repository_path,
        repository_id,
        revision,
        trust_tier,
        license,
        policy,
        Some(cache),
    )
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn ingest_git_commit_inner(
    repository_path: &Path,
    repository_id: &RepositoryId,
    revision: &Revision,
    trust_tier: TrustTier,
    license: &LicenseStatus,
    policy: &GitCorpusPolicy,
    mut cache: Option<&mut GitIngestionCache>,
) -> Result<IngestionReport, GitIngestionError> {
    validate_policy(policy)?;
    let repo =
        gix::open(repository_path).map_err(|error| GitIngestionError::Open(error.to_string()))?;
    let commit_id = gix::ObjectId::from_hex(revision.as_str().as_bytes())
        .map_err(|error| GitIngestionError::InvalidCommit(error.to_string()))?;
    let commit = repo
        .find_commit(commit_id)
        .map_err(|error| GitIngestionError::InvalidCommit(error.to_string()))?;
    let tree = commit
        .tree()
        .map_err(|error| GitIngestionError::ReadTree(error.to_string()))?;
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    let mut visited_files = 0_usize;
    let tree_walk_started = Instant::now();
    collect_tree(
        &tree,
        "",
        policy,
        &mut visited_files,
        &mut candidates,
        &mut rejected,
    )?;
    let mut observations = GitIngestionObservations {
        tree_walk_us: elapsed_us(tree_walk_started),
        candidate_count: u64::try_from(candidates.len()).unwrap_or(u64::MAX),
        ..GitIngestionObservations::default()
    };
    let candidate_sort_started = Instant::now();
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    observations.candidate_sort_us = elapsed_us(candidate_sort_started);

    let mut report = IngestionReport {
        documents: Vec::with_capacity(candidates.len()),
        rejected,
        sources: Vec::with_capacity(candidates.len()),
        visited_files,
        #[cfg(feature = "git-corpus")]
        git_observations: None,
    };
    for candidate in candidates {
        let cache_key = (candidate.id, candidate.path.clone());
        let cached = cache
            .as_deref()
            .and_then(|cache| cache.blobs.get(&cache_key))
            .cloned();
        let blob = if let Some(cached) = cached {
            observations.blob_cache_hits = observations.blob_cache_hits.saturating_add(1);
            cached
        } else {
            let loaded = load_git_blob(&repo, &candidate, &mut observations)?;
            if let Some(cache) = cache.as_deref_mut() {
                cache.blobs.insert(cache_key, loaded.clone());
            }
            loaded
        };
        let CachedGitBlob::Text {
            content,
            digest,
            media_type,
            identifiers,
            line_count,
        } = blob
        else {
            let CachedGitBlob::Rejected { code, message } = blob else {
                unreachable!()
            };
            report
                .rejected
                .push(source_rejection(&candidate.path, code, message));
            continue;
        };
        let metadata_started = Instant::now();
        let mut document = NormalizedDocument::new(
            candidate.path.clone(),
            SourceKind::GitBlob,
            repository_id.clone(),
            revision.clone(),
            candidate.path.clone(),
            media_type,
            content,
        )?;
        document.trust_tier = trust_tier;
        document.license = license.clone();
        document.source_span = SourceSpan::new(
            1,
            line_count,
            Some(0),
            u64::try_from(document.content.len()).ok(),
        )
        .ok();
        document.identifiers = identifiers;
        document.canonical_uri =
            Some(format!("vesc://knowledge/document/{}", document.document_id).try_into()?);
        report.sources.push(SourceInventory {
            relative_path: candidate.path.clone().into(),
            title: candidate.path,
            repository: repository_id.clone(),
            revision: revision.clone(),
            media_type: document.media_type.clone(),
            source_kind: SourceKind::GitBlob,
            trust_tier,
            license: license.clone(),
            required: false,
            byte_count: Some(candidate.size),
            content_digest: Some(digest),
            document_count: 1,
            rejection: None,
        });
        report.documents.push(document);
        observations.document_metadata_us = observations
            .document_metadata_us
            .saturating_add(elapsed_us(metadata_started));
    }
    report.git_observations = Some(observations);
    Ok(report)
}

fn load_git_blob(
    repo: &gix::Repository,
    candidate: &Candidate,
    observations: &mut GitIngestionObservations,
) -> Result<CachedGitBlob, GitIngestionError> {
    let blob_load_started = Instant::now();
    let object = repo
        .find_object(candidate.id)
        .map_err(|error| GitIngestionError::ReadTree(error.to_string()))?;
    observations.blob_load_us = observations
        .blob_load_us
        .saturating_add(elapsed_us(blob_load_started));
    observations.blob_bytes_loaded = observations
        .blob_bytes_loaded
        .saturating_add(u64::try_from(object.data.len()).unwrap_or(u64::MAX));
    let binary_scan_started = Instant::now();
    let is_binary = object.data.contains(&0);
    observations.binary_scan_us = observations
        .binary_scan_us
        .saturating_add(elapsed_us(binary_scan_started));
    if is_binary {
        observations.binary_rejection_count = observations.binary_rejection_count.saturating_add(1);
        return Ok(CachedGitBlob::Rejected {
            code: "binary",
            message: "Git blob contains binary data",
        });
    }
    let utf8_started = Instant::now();
    let content = normalize_text_ref(&object.data);
    observations.utf8_normalization_us = observations
        .utf8_normalization_us
        .saturating_add(elapsed_us(utf8_started));
    let Ok(content) = content else {
        observations.encoding_rejection_count =
            observations.encoding_rejection_count.saturating_add(1);
        return Ok(CachedGitBlob::Rejected {
            code: "encoding",
            message: "Git blob is not UTF-8 text",
        });
    };
    Ok(CachedGitBlob::Text {
        digest: ContentDigest::of(content.as_bytes()),
        media_type: media_type(&candidate.path).to_owned(),
        identifiers: identifiers(&candidate.path, &content),
        line_count: u32::try_from(content.lines().count().max(1)).unwrap_or(u32::MAX),
        content,
    })
}

fn validate_policy(policy: &GitCorpusPolicy) -> Result<(), GitIngestionError> {
    for prefix in policy
        .include_prefixes
        .iter()
        .chain(&policy.exclude_prefixes)
    {
        let path = Path::new(prefix);
        if prefix.is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(GitIngestionError::InvalidPolicy(format!(
                "path prefix must be a relative normalized Git path: {prefix}"
            )));
        }
    }
    Ok(())
}

fn collect_tree(
    tree: &gix::Tree<'_>,
    prefix: &str,
    policy: &GitCorpusPolicy,
    visited_files: &mut usize,
    candidates: &mut Vec<Candidate>,
    rejected: &mut Vec<SourceRejection>,
) -> Result<(), GitIngestionError> {
    for entry in tree.iter() {
        let entry = entry.map_err(|error| GitIngestionError::ReadTree(error.to_string()))?;
        let filename = entry
            .filename()
            .to_str()
            .map_err(|_| GitIngestionError::ReadTree("tree contains a non-UTF-8 path".into()))?;
        let path = if prefix.is_empty() {
            filename.to_owned()
        } else {
            format!("{prefix}/{filename}")
        };
        match entry.kind() {
            gix::object::tree::EntryKind::Tree => {
                if !is_excluded(&path, policy) {
                    let subtree = entry
                        .object()
                        .map_err(|error| GitIngestionError::ReadTree(error.to_string()))?
                        .into_tree();
                    collect_tree(&subtree, &path, policy, visited_files, candidates, rejected)?;
                }
            }
            gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable => {
                *visited_files = visited_files.saturating_add(1);
                if !is_selected(&path, policy) {
                    rejected.push(source_rejection(
                        &path,
                        "unsupported",
                        "path or media type is outside the configured corpus policy",
                    ));
                    continue;
                }
                let size = entry
                    .id()
                    .header()
                    .map_err(|error| GitIngestionError::ReadTree(error.to_string()))?
                    .size();
                candidates.push(Candidate {
                    path,
                    id: entry.object_id(),
                    size,
                });
            }
            gix::object::tree::EntryKind::Link | gix::object::tree::EntryKind::Commit => {
                *visited_files = visited_files.saturating_add(1);
                rejected.push(source_rejection(
                    &path,
                    "unsupported",
                    "symlinks and Gitlinks are metadata and are not followed",
                ));
            }
        }
    }
    Ok(())
}

fn is_selected(path: &str, policy: &GitCorpusPolicy) -> bool {
    !is_excluded(path, policy)
        && (policy.include_prefixes.is_empty()
            || policy
                .include_prefixes
                .iter()
                .any(|prefix| path_is_under(path, prefix)))
        && Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                policy.filenames.contains(name)
                    || Path::new(name)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            policy.extensions.contains(&extension.to_ascii_lowercase())
                        })
            })
}

fn is_excluded(path: &str, policy: &GitCorpusPolicy) -> bool {
    policy
        .exclude_prefixes
        .iter()
        .any(|prefix| path_is_under(path, prefix))
}

fn path_is_under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_matches('/');
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn media_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("c" | "h") => "text/x-c",
        Some("cc" | "cpp" | "hh" | "hpp") => "text/x-c++",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("qml") => "text/x-qml",
        Some("rs") => "text/x-rust",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        _ => "text/plain",
    }
}

fn identifiers(path: &str, content: &str) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    values.insert(path.to_owned());
    if let Some(stem) = Path::new(path).file_stem().and_then(|stem| stem.to_str()) {
        values.insert(stem.to_owned());
    }
    for token in
        content.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
    {
        if token.len() >= 3
            && token.len() <= 128
            && token
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
        {
            values.insert(token.to_owned());
            if values.len() == 4096 {
                break;
            }
        }
    }
    values
}

fn source_rejection(path: &str, code: &str, message: &str) -> SourceRejection {
    SourceRejection {
        source: path.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        required: false,
    }
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}
