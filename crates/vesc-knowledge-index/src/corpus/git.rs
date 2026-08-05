//! Bounded ingestion of immutable Git commit trees without a worktree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use compact_str::CompactString;
use gix::bstr::ByteSlice;

use super::ingest::{IngestionReport, SourceInventory, SourceRejection, normalize_text_ref};
use super::{
    ContentDigest, CorpusError, LicenseStatus, NormalizedDocument, RepositoryId, Revision,
    SourceKind, SourceSpan, TrustTier,
};

const DEFAULT_EXTENSIONS: &[&str] = &[
    "c", "c++", "cc", "cp", "cpp", "cppm", "cxx", "h", "h++", "hh", "hp", "hpp", "hxx", "i", "icc",
    "ii", "inc", "inl", "ipp", "ixx", "mpp", "tcc", "tpp", "txx", "json", "lisp", "md", "qml",
    "rs", "toml", "ts", "txt", "yaml", "yml",
];
const MANDATORY_CODE_EXTENSIONS: &[&str] = &[
    "c", "c++", "cc", "cp", "cpp", "cppm", "cxx", "h", "h++", "hh", "hp", "hpp", "hxx", "i", "icc",
    "ii", "inc", "inl", "ipp", "ixx", "mpp", "tcc", "tpp", "txx",
];
const DEFAULT_FILENAMES: &[&str] = &["CMakeLists.txt", "Kconfig", "Makefile"];
pub(crate) const MAX_REJECTION_SAMPLES: usize = 64;
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
pub const GIT_CORPUS_POLICY_VERSION: &str = "reviewed-v4";

/// Working-set limits for one repository ingestion pass.
///
/// A cold history build applies independent file-count and total-byte budgets
/// to code/doc blobs and commit messages, so messages cannot crowd out source.
/// A fast-forward build applies them only to the new delta. `max_file_bytes`
/// is checked again before every later Git-object hydration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitCorpusLimits {
    file_bytes: u64,
    files: usize,
    total_bytes: u64,
}

impl GitCorpusLimits {
    /// Construct a nonzero, internally consistent limit set.
    ///
    /// # Errors
    ///
    /// Returns [`GitIngestionError::InvalidPolicy`] when any limit is zero or
    /// the per-file limit exceeds the total-byte limit.
    pub fn new(
        max_file_bytes: u64,
        max_files: usize,
        max_total_bytes: u64,
    ) -> Result<Self, GitIngestionError> {
        if max_file_bytes == 0
            || max_files == 0
            || max_total_bytes == 0
            || max_file_bytes > max_total_bytes
        {
            return Err(GitIngestionError::InvalidPolicy(
                "Git corpus limits must be nonzero and max_file_bytes must not exceed max_total_bytes"
                    .into(),
            ));
        }
        Ok(Self {
            file_bytes: max_file_bytes,
            files: max_files,
            total_bytes: max_total_bytes,
        })
    }

    #[must_use]
    pub const fn max_file_bytes(self) -> u64 {
        self.file_bytes
    }

    #[must_use]
    pub const fn max_files(self) -> usize {
        self.files
    }

    #[must_use]
    pub const fn max_total_bytes(self) -> u64 {
        self.total_bytes
    }
}

impl Default for GitCorpusLimits {
    fn default() -> Self {
        Self {
            file_bytes: u64::MAX,
            files: usize::MAX,
            total_bytes: u64::MAX,
        }
    }
}

/// Reviewed path and media-type selection for one immutable repository snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCorpusPolicy {
    pub include_prefixes: Vec<String>,
    pub exclude_prefixes: Vec<String>,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub extensions: BTreeSet<String>,
    pub filenames: BTreeSet<String>,
    pub limits: GitCorpusLimits,
}

impl Default for GitCorpusPolicy {
    fn default() -> Self {
        Self {
            include_prefixes: Vec::new(),
            exclude_prefixes: DEFAULT_EXCLUDES.iter().map(ToString::to_string).collect(),
            include_patterns: Vec::new(),
            exclude_patterns: Vec::new(),
            extensions: DEFAULT_EXTENSIONS.iter().map(ToString::to_string).collect(),
            filenames: DEFAULT_FILENAMES.iter().map(ToString::to_string).collect(),
            limits: GitCorpusLimits::default(),
        }
    }
}

/// One already-managed repository and immutable commit selected for a corpus build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCorpusSource {
    pub repository_path: PathBuf,
    pub repository_id: RepositoryId,
    pub revision: Revision,
    pub history_tips: Vec<Revision>,
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
pub(super) struct Candidate {
    pub(super) path: String,
    pub(super) id: gix::ObjectId,
    pub(super) size: u64,
}

#[derive(Debug)]
pub(super) enum CachedGitBlob {
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

pub(super) struct GitCorpusBudget {
    limits: GitCorpusLimits,
    files: usize,
    bytes: u64,
}

struct TreeCollection {
    budget: GitCorpusBudget,
    visited_files: usize,
    candidates: Vec<Candidate>,
    rejected: Vec<SourceRejection>,
    rejection_count: u64,
}

impl GitCorpusBudget {
    pub(super) const fn new(limits: GitCorpusLimits) -> Self {
        Self {
            limits,
            files: 0,
            bytes: 0,
        }
    }

    pub(super) const fn reserve(&mut self, size: u64) -> Result<(), (&'static str, &'static str)> {
        if size > self.limits.file_bytes {
            return Err((
                "oversized",
                "Git blob exceeds the configured per-file byte limit",
            ));
        }
        if self.files >= self.limits.files {
            return Err(("file_limit", "Git corpus exceeds the configured file limit"));
        }
        let Some(bytes) = self.bytes.checked_add(size) else {
            return Err((
                "total_bytes",
                "Git corpus exceeds the configured total byte limit",
            ));
        };
        if bytes > self.limits.total_bytes {
            return Err((
                "total_bytes",
                "Git corpus exceeds the configured total byte limit",
            ));
        }
        self.files += 1;
        self.bytes = bytes;
        Ok(())
    }
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
    pub rejection_count: u64,
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
        self.rejection_count = self.rejection_count.saturating_add(other.rejection_count);
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
    let mut collection = TreeCollection {
        budget: GitCorpusBudget::new(policy.limits),
        visited_files: 0,
        candidates: Vec::new(),
        rejected: Vec::new(),
        rejection_count: 0,
    };
    let tree_walk_started = Instant::now();
    collect_tree(&tree, "", policy, &mut collection)?;
    let TreeCollection {
        mut candidates,
        rejected,
        rejection_count,
        visited_files,
        ..
    } = collection;
    let mut observations = GitIngestionObservations {
        tree_walk_us: elapsed_us(tree_walk_started),
        candidate_count: u64::try_from(candidates.len()).unwrap_or(u64::MAX),
        rejection_count,
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
        git_observations: None,
    };
    for candidate in candidates {
        let blob = load_git_blob(&repo, &candidate, policy.limits, &mut observations, true)?;
        let digest = if let CachedGitBlob::Text { digest, .. } = &blob {
            digest.clone()
        } else {
            let CachedGitBlob::Rejected { code, message } = blob else {
                unreachable!()
            };
            record_rejection(
                &mut report.rejected,
                &mut observations.rejection_count,
                &candidate.path,
                code,
                message,
            );
            continue;
        };
        let metadata_started = Instant::now();
        let document = document_from_git_blob(
            &candidate.path,
            repository_id,
            revision,
            trust_tier,
            license,
            blob,
        )?;
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

pub(super) fn load_git_blob(
    repo: &gix::Repository,
    candidate: &Candidate,
    limits: GitCorpusLimits,
    observations: &mut GitIngestionObservations,
    extract_identifiers: bool,
) -> Result<CachedGitBlob, GitIngestionError> {
    if candidate.size > limits.file_bytes {
        return Ok(CachedGitBlob::Rejected {
            code: "oversized",
            message: "Git blob exceeds the configured per-file byte limit",
        });
    }
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
        identifiers: if extract_identifiers {
            identifiers(&candidate.path, &content)
        } else {
            BTreeSet::new()
        },
        line_count: u32::try_from(content.lines().count().max(1)).unwrap_or(u32::MAX),
        content,
    })
}

pub(super) fn document_from_git_blob(
    path: &str,
    repository_id: &RepositoryId,
    revision: &Revision,
    trust_tier: TrustTier,
    license: &LicenseStatus,
    blob: CachedGitBlob,
) -> Result<NormalizedDocument, GitIngestionError> {
    let CachedGitBlob::Text {
        content,
        media_type,
        identifiers,
        line_count,
        ..
    } = blob
    else {
        unreachable!("rejected blobs are filtered before document construction")
    };
    let mut document = NormalizedDocument::new(
        path.to_owned(),
        SourceKind::GitBlob,
        repository_id.clone(),
        revision.clone(),
        path.to_owned(),
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
    Ok(document)
}

pub(crate) fn commit_message_size(commit: &gix::Commit<'_>, max_bytes: u64) -> Option<u64> {
    let message = commit.message_raw_sloppy().to_str_lossy();
    let message = message.trim();
    let size = u64::try_from(message.len().saturating_add(1)).ok()?;
    if message.is_empty() || size > max_bytes {
        return None;
    }
    Some(size)
}

pub(crate) fn commit_message_content(commit: &gix::Commit<'_>, max_bytes: u64) -> Option<String> {
    commit_message_size(commit, max_bytes)?;
    let message = commit.message_raw_sloppy().to_str_lossy();
    let message = message.trim();
    let mut content = String::with_capacity(message.len().saturating_add(1));
    content.push_str(message);
    content.push('\n');
    Some(content)
}

pub(crate) fn document_from_git_commit(
    commit: &gix::Commit<'_>,
    repository_id: &RepositoryId,
    trust_tier: TrustTier,
    license: &LicenseStatus,
    max_bytes: u64,
) -> Result<Option<NormalizedDocument>, GitIngestionError> {
    let raw_message = commit.message_raw_sloppy().to_str_lossy();
    let title = raw_message
        .lines()
        .find(|line| !line.trim().is_empty())
        .map_or("Git commit", str::trim);
    let Some(content) = commit_message_content(commit, max_bytes) else {
        return Ok(None);
    };
    let revision = Revision::try_from(commit.id.to_string())?;
    let path = format!("commits/{}", commit.id);
    let mut document = NormalizedDocument::new(
        title,
        SourceKind::GitCommit,
        repository_id.clone(),
        revision,
        path,
        "text/x-git-commit",
        content,
    )?;
    document.trust_tier = trust_tier;
    document.license = license.clone();
    document.source_span = SourceSpan::new(
        1,
        u32::try_from(document.content.lines().count().max(1)).unwrap_or(u32::MAX),
        Some(0),
        u64::try_from(document.content.len()).ok(),
    )
    .ok();
    document.canonical_uri =
        Some(format!("vesc://knowledge/document/{}", document.document_id).try_into()?);
    Ok(Some(document))
}

pub(super) fn validate_policy(policy: &GitCorpusPolicy) -> Result<(), GitIngestionError> {
    for prefix in policy
        .include_prefixes
        .iter()
        .chain(&policy.exclude_prefixes)
        .chain(&policy.include_patterns)
        .chain(&policy.exclude_patterns)
    {
        let path = Path::new(prefix);
        if prefix.is_empty()
            || path.is_absolute()
            || prefix.contains(['[', ']', '\\'])
            || has_ambiguous_double_star(prefix)
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(GitIngestionError::InvalidPolicy(format!(
                "path selector must be a relative normalized Git path using only *, **, and ?: {prefix}"
            )));
        }
    }
    Ok(())
}

fn has_ambiguous_double_star(pattern: &str) -> bool {
    let pattern = pattern.as_bytes();
    let mut index = 0;
    while index + 1 < pattern.len() {
        if pattern[index] != b'*' || pattern[index + 1] != b'*' {
            index += 1;
            continue;
        }
        let start = index;
        while pattern.get(index) == Some(&b'*') {
            index += 1;
        }
        if start != 0 && pattern[start - 1] != b'/'
            || index != pattern.len() && pattern[index] != b'/'
        {
            return true;
        }
    }
    false
}

fn collect_tree(
    tree: &gix::Tree<'_>,
    prefix: &str,
    policy: &GitCorpusPolicy,
    collection: &mut TreeCollection,
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
                // Excluded directories can still contain mandatory C/C++.
                // Apply exclusions to blobs, where the mandatory floor is known.
                let subtree = entry
                    .object()
                    .map_err(|error| GitIngestionError::ReadTree(error.to_string()))?
                    .into_tree();
                collect_tree(&subtree, &path, policy, collection)?;
            }
            gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable => {
                collection.visited_files = collection.visited_files.saturating_add(1);
                if !is_selected(&path, policy) {
                    record_rejection(
                        &mut collection.rejected,
                        &mut collection.rejection_count,
                        &path,
                        "unsupported",
                        "path or media type is outside the configured corpus policy",
                    );
                    continue;
                }
                let size = entry
                    .id()
                    .header()
                    .map_err(|error| GitIngestionError::ReadTree(error.to_string()))?
                    .size();
                if let Err((code, message)) = collection.budget.reserve(size) {
                    record_rejection(
                        &mut collection.rejected,
                        &mut collection.rejection_count,
                        &path,
                        code,
                        message,
                    );
                    continue;
                }
                collection.candidates.push(Candidate {
                    path,
                    id: entry.object_id(),
                    size,
                });
            }
            gix::object::tree::EntryKind::Link | gix::object::tree::EntryKind::Commit => {
                collection.visited_files = collection.visited_files.saturating_add(1);
                record_rejection(
                    &mut collection.rejected,
                    &mut collection.rejection_count,
                    &path,
                    "unsupported",
                    "symlinks and Gitlinks are metadata and are not followed",
                );
            }
        }
    }
    Ok(())
}

pub(super) fn is_selected(path: &str, policy: &GitCorpusPolicy) -> bool {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let mandatory_code = extension
        .as_deref()
        .is_some_and(|extension| MANDATORY_CODE_EXTENSIONS.contains(&extension));
    if mandatory_code {
        return true;
    }
    if is_excluded(path, policy) {
        return false;
    }
    if !policy.include_prefixes.is_empty() || !policy.include_patterns.is_empty() {
        return policy
            .include_prefixes
            .iter()
            .any(|prefix| path_is_under(path, prefix))
            || policy
                .include_patterns
                .iter()
                .any(|pattern| glob_matches(pattern, path));
    }
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            policy.filenames.contains(name)
                || Path::new(name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase)
                    .is_some_and(|extension| policy.extensions.contains(&extension))
        })
}

fn is_excluded(path: &str, policy: &GitCorpusPolicy) -> bool {
    policy
        .exclude_prefixes
        .iter()
        .any(|prefix| path_is_under(path, prefix))
        || policy
            .exclude_patterns
            .iter()
            .any(|pattern| glob_matches(pattern, path))
}

fn path_is_under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_matches('/');
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.as_bytes().as_bstr();
    let mode = gix::glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL;
    let path_match = gix::glob::wildmatch(pattern, path.as_bytes().as_bstr(), mode);
    path_match
        || (!pattern.contains(&b'/')
            && Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| gix::glob::wildmatch(pattern, name.as_bytes().as_bstr(), mode)))
}

fn media_type(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("c" | "h" | "i" | "inc") => "text/x-c",
        Some(
            "c++" | "cc" | "cp" | "cpp" | "cppm" | "cxx" | "h++" | "hh" | "hp" | "hpp" | "hxx"
            | "icc" | "ii" | "inl" | "ipp" | "ixx" | "mpp" | "tcc" | "tpp" | "txx",
        ) => "text/x-c++",
        Some("json") => "application/json",
        Some("md") => "text/markdown",
        Some("qml") => "text/x-qml",
        Some("rs") => "text/x-rust",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        _ => "text/plain",
    }
}

pub(super) fn identifiers(path: &str, content: &str) -> BTreeSet<String> {
    identifier_values(path, content)
        .into_iter()
        .map(CompactString::into_string)
        .collect()
}

pub(crate) fn identifier_values(path: &str, content: &str) -> Vec<CompactString> {
    let mut buffer = [""; MAX_IDENTIFIERS];
    identifier_refs(path, content, &mut buffer)
        .iter()
        .map(|value| CompactString::new(value))
        .collect()
}

pub(crate) const MAX_IDENTIFIERS: usize = 32;

pub(super) fn identifier_refs<'input, 'buffer>(
    path: &'input str,
    content: &'input str,
    buffer: &'buffer mut [&'input str; MAX_IDENTIFIERS],
) -> &'buffer [&'input str] {
    let mut length = 1;
    buffer[0] = path;
    if let Some(stem) = Path::new(path).file_stem().and_then(|stem| stem.to_str())
        && !buffer[..length].contains(&stem)
    {
        buffer[length] = stem;
        length += 1;
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
            && !buffer[..length].contains(&token)
        {
            buffer[length] = token;
            length += 1;
            if length == MAX_IDENTIFIERS {
                break;
            }
        }
    }
    buffer[..length].sort_unstable();
    &buffer[..length]
}

fn source_rejection(path: &str, code: &str, message: &str) -> SourceRejection {
    SourceRejection {
        source: path.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        required: false,
    }
}

fn record_rejection(
    samples: &mut Vec<SourceRejection>,
    count: &mut u64,
    path: &str,
    code: &str,
    message: &str,
) {
    *count = count.saturating_add(1);
    if samples.len() < MAX_REJECTION_SAMPLES {
        samples.push(source_rejection(path, code, message));
    }
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        GitCorpusPolicy, GitIngestionError, glob_matches, identifier_refs, identifier_values,
        is_selected, validate_policy,
    };

    #[test]
    fn managed_repository_allowlist_cannot_omit_c_or_cpp_sources() {
        let policy = GitCorpusPolicy {
            include_patterns: vec!["**/*.md".into()],
            extensions: std::iter::once("md".into()).collect(),
            ..GitCorpusPolicy::default()
        };

        for path in [
            "src/control.c",
            "src/control.c++",
            "src/control.cc",
            "src/control.cp",
            "src/control.cpp",
            "src/control.cppm",
            "src/control.cxx",
            "include/control.h",
            "include/control.h++",
            "include/control.hh",
            "include/control.hp",
            "include/control.hpp",
            "include/control.hxx",
            "src/control.i",
            "include/control.icc",
            "src/control.ii",
            "include/control.inc",
            "include/control.inl",
            "include/control.ipp",
            "include/control.ixx",
            "src/control.mpp",
            "include/control.tcc",
            "include/control.tpp",
            "include/control.txx",
        ] {
            assert!(is_selected(path, &policy), "omitted {path}");
        }
    }

    #[test]
    fn managed_repository_explicit_patterns_admit_requested_text_types() {
        let policy = GitCorpusPolicy {
            include_patterns: vec!["**/*.lbm".into()],
            ..GitCorpusPolicy::default()
        };

        assert!(is_selected("package/main.lbm", &policy));
        assert!(!is_selected("package/image.png", &policy));
    }

    #[test]
    fn managed_repository_exclusions_cannot_omit_the_mandatory_code_floor() {
        let policy = GitCorpusPolicy {
            exclude_patterns: vec!["generated/**".into()],
            ..GitCorpusPolicy::default()
        };

        assert!(is_selected("generated/control.cpp", &policy));
        assert!(!is_selected("generated/notes.md", &policy));
    }

    #[test]
    fn managed_repository_globs_reject_ambiguous_non_git_forms() {
        for pattern in [r"src\*.rs", "src/prefix**/*.rs", "src/**suffix.rs"] {
            let mut policy = GitCorpusPolicy::default();
            policy.include_patterns.push(pattern.to_owned());
            assert!(
                matches!(
                    validate_policy(&policy),
                    Err(GitIngestionError::InvalidPolicy(_))
                ),
                "accepted ambiguous pattern {pattern:?}"
            );
        }
    }

    #[test]
    fn managed_repository_globs_match_paths_without_crossing_single_stars() {
        for (pattern, path, expected) in [
            ("", "", true),
            ("", "a", false),
            ("*", "", true),
            ("*", "name", true),
            ("?", "a", true),
            ("?", "/", false),
            ("**", "src/nested/lib.rs", true),
            ("**/*.md", "README.md", true),
            ("**/*.md", "docs/guide.md", true),
            ("a/**/b", "a/b", true),
            ("a/**/b", "a/nested/deeper/b", true),
            ("src/**/*.rs", "src/lib.rs", true),
            ("src/**/*.rs", "src/nested/mod.rs", true),
            ("src/?.rs", "src/a.rs", true),
            ("src/?.rs", "src/ab.rs", false),
            ("src/*.rs", "src/nested/mod.rs", false),
            ("*.pro", "nested/vesc_tool.pro", true),
            ("*.pro", "nested/vesc_tool.pri", false),
            ("**/*.md", "docs/guide.rs", false),
        ] {
            assert_eq!(
                glob_matches(pattern, path),
                expected,
                "pattern {pattern:?}, path {path:?}"
            );
        }
    }

    #[test]
    fn git_chunk_identifiers_are_bounded() {
        let content = (0..100)
            .map(|index| format!("identifier_{index}"))
            .collect::<Vec<_>>()
            .join(" ");

        let identifiers = identifier_values("src/motor_control.c", &content);

        assert!(identifiers.len() <= 32);
        assert!(identifiers.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            identifiers
                .iter()
                .any(|value| value == "src/motor_control.c")
        );
        assert!(identifiers.iter().any(|value| value == "motor_control"));
    }

    #[test]
    fn short_git_chunk_identifiers_are_inline_strings() {
        let identifiers = identifier_values("src/motor.c", "motor_speed motor_current");
        let serialized = serde_json::to_value(&identifiers).expect("identifiers serialize");

        assert!(
            identifiers
                .iter()
                .filter(|value| value.len() <= 24)
                .all(|value| !value.is_heap_allocated())
        );
        assert_eq!(
            serialized,
            serde_json::json!(["motor", "motor_current", "motor_speed", "src/motor.c"])
        );
        assert_eq!(
            serde_json::from_value::<Vec<compact_str::CompactString>>(serialized)
                .expect("existing JSON identifiers deserialize"),
            identifiers
        );
    }

    #[test]
    fn identifier_refs_borrow_from_path_and_content() {
        let path = String::from("src/motor.c");
        let content = String::from("motor_speed motor_current");
        let mut buffer = [""; 32];
        let identifiers = identifier_refs(&path, &content, &mut buffer);

        assert_eq!(
            identifiers,
            ["motor", "motor_current", "motor_speed", "src/motor.c"]
        );
        for identifier in identifiers {
            let start = identifier.as_ptr() as usize;
            let in_path =
                (path.as_ptr() as usize..path.as_ptr() as usize + path.len()).contains(&start);
            let in_content = (content.as_ptr() as usize..content.as_ptr() as usize + content.len())
                .contains(&start);
            assert!(in_path || in_content);
        }
    }
}
