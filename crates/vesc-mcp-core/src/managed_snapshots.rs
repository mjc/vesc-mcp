//! Immutable multi-repository knowledge snapshots.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vesc_knowledge_index::corpus::git::{GitCorpusPolicy, GitCorpusSource};
use vesc_knowledge_index::corpus::{
    LicenseStatus, RepositoryId as CorpusRepositoryId, Revision, TrustTier as CorpusTrustTier,
};

use crate::managed_git::{ManagedGitError, ManagedGitStore};
pub use crate::managed_repositories::KnowledgeSnapshotId;
use crate::managed_repositories::{
    KnowledgeDataLayout, KnowledgeRepository, RepositoryId, RepositoryPolicy, RepositoryRegistry,
    TrustTier,
};

const SNAPSHOT_SCHEMA: u16 = 1;

// Keep the process inside one indexing working set. Different MCP requests can
// ask for different snapshots, so per-snapshot locks are not enough here.
static SNAPSHOT_BUILD_GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn snapshot_build_gate() -> Arc<tokio::sync::Semaphore> {
    Arc::clone(SNAPSHOT_BUILD_GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

/// Corpus profile represented by one immutable snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotProfile {
    #[default]
    SelectedTrees,
    CompleteHistory,
}

/// Optional semantic identity included when a snapshot contains vectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSemanticModel {
    pub model_id: String,
    pub model_revision: String,
}

/// One immutable repository selection in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRepository {
    pub repository: RepositoryId,
    pub commit: String,
    pub policy_digest: String,
}

/// Deterministic, path-free description of a prepared snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSnapshotManifest {
    pub schema: u16,
    pub id: KnowledgeSnapshotId,
    #[serde(default)]
    pub profile: SnapshotProfile,
    pub repositories: Vec<SnapshotRepository>,
    pub component_versions: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SnapshotSemanticModel>,
}

impl KnowledgeSnapshotManifest {
    /// Construct the canonical manifest and derive its identity.
    ///
    /// # Errors
    ///
    /// Returns an error if no repository is selected or serialization fails.
    pub fn new(
        repositories: Vec<SnapshotRepository>,
        semantic: Option<SnapshotSemanticModel>,
    ) -> Result<Self, SnapshotError> {
        Self::with_profile(repositories, semantic, SnapshotProfile::SelectedTrees)
    }

    fn with_profile(
        mut repositories: Vec<SnapshotRepository>,
        semantic: Option<SnapshotSemanticModel>,
        profile: SnapshotProfile,
    ) -> Result<Self, SnapshotError> {
        if repositories.is_empty() {
            return Err(SnapshotError::EmptySelection);
        }
        repositories.sort_by(|left, right| left.repository.cmp(&right.repository));
        if repositories
            .windows(2)
            .any(|pair| pair[0].repository == pair[1].repository)
        {
            return Err(SnapshotError::DuplicateRepository);
        }
        let component_versions = vesc_knowledge_index::artifact_component_versions();
        let identity = SnapshotIdentity {
            schema: SNAPSHOT_SCHEMA,
            profile,
            repositories: &repositories,
            component_versions: &component_versions,
            semantic: semantic.as_ref(),
        };
        let id = KnowledgeSnapshotId::new(hex_sha256(&serde_json::to_vec(&identity)?))
            .map_err(|error| SnapshotError::Build(error.to_string()))?;
        Ok(Self {
            schema: SNAPSHOT_SCHEMA,
            id,
            profile,
            repositories,
            component_versions,
            semantic,
        })
    }
}

#[derive(Serialize)]
struct SnapshotIdentity<'a> {
    schema: u16,
    profile: SnapshotProfile,
    repositories: &'a [SnapshotRepository],
    component_versions: &'a BTreeMap<String, String>,
    semantic: Option<&'a SnapshotSemanticModel>,
}

/// Whether preparation built a snapshot or reused a complete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotDisposition {
    Built,
    Reused,
    Deduplicated,
    Stale,
}

/// Bounded operational state suitable for agent-facing status responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SnapshotState {
    Ready,
    Building,
    Failed,
    Stale,
}

/// A complete immutable snapshot ready for search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSnapshot {
    pub manifest: KnowledgeSnapshotManifest,
    pub artifact_path: PathBuf,
    pub disposition: SnapshotDisposition,
}

/// Default and explicitly prewarmed snapshots prepared during startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSnapshots {
    pub default: PreparedSnapshot,
    pub prewarmed: Vec<PreparedSnapshot>,
}

/// Snapshot resolution or preparation failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    #[error("snapshot selection is empty")]
    EmptySelection,
    #[error("snapshot selection contains a duplicate repository")]
    DuplicateRepository,
    #[error("snapshot repository is not configured: {0}")]
    UnknownRepository(RepositoryId),
    #[error("managed repository resolution failed")]
    ManagedGit(#[from] ManagedGitError),
    #[error("snapshot storage failed")]
    Storage(#[from] std::io::Error),
    #[error("snapshot serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("snapshot artifact build failed: {0}")]
    Build(String),
    #[error("snapshot task failed")]
    Task(#[from] tokio::task::JoinError),
    #[error("snapshot manifest does not match its identity")]
    IdentityMismatch,
}

struct BuildSlot {
    generation: Mutex<u64>,
    state: Mutex<SnapshotState>,
}

impl Default for BuildSlot {
    fn default() -> Self {
        Self {
            generation: Mutex::new(0),
            state: Mutex::new(SnapshotState::Failed),
        }
    }
}

/// Manages immutable snapshot manifests, artifacts, and the default alias.
#[derive(Clone)]
pub struct KnowledgeSnapshotStore {
    layout: KnowledgeDataLayout,
    git: ManagedGitStore,
    slots: Arc<Mutex<HashMap<KnowledgeSnapshotId, Arc<BuildSlot>>>>,
}

impl KnowledgeSnapshotStore {
    #[must_use]
    pub fn new(layout: KnowledgeDataLayout) -> Self {
        Self {
            git: ManagedGitStore::new(layout.clone()),
            layout,
            slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Resolve configured defaults, prepare their immutable snapshot, and atomically activate it.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured ref cannot resolve or preparation fails.
    pub async fn prepare_default(
        &self,
        repositories: &RepositoryRegistry,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let prepared = match self
            .prepare_profile(
                repositories,
                &BTreeMap::new(),
                SnapshotProfile::CompleteHistory,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return match self.load_default(SnapshotDisposition::Stale) {
                    Ok(stale) => {
                        self.set_state(&stale.manifest.id, SnapshotState::Stale);
                        Ok(stale)
                    }
                    Err(_) => Err(error),
                };
            }
        };
        write_json_atomic(&self.default_alias_path(), &prepared.manifest)?;
        Ok(prepared)
    }

    /// Prepare the configured default and only the explicitly selected historical snapshots.
    ///
    /// Snapshot builds run in order so startup never holds multiple indexing
    /// working sets at once; the process-wide gate also covers independent
    /// stores created by concurrent MCP requests.
    ///
    /// # Errors
    ///
    /// Returns the first resolution, storage, or build failure.
    pub async fn prepare_configured(
        &self,
        repositories: &RepositoryRegistry,
        prewarm: &[BTreeMap<RepositoryId, String>],
    ) -> Result<PreparedSnapshots, SnapshotError> {
        let default = self.prepare_default(repositories).await?;
        let mut prewarmed = Vec::with_capacity(prewarm.len());
        for selection in prewarm {
            prewarmed.push(self.prepare(repositories, selection).await?);
        }
        Ok(PreparedSnapshots { default, prewarmed })
    }

    /// Prepare a snapshot, applying explicit selectors over configured defaults.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown repositories, selectors, storage failures,
    /// corrupt cached artifacts, or indexing failures.
    pub async fn prepare(
        &self,
        repositories: &RepositoryRegistry,
        selectors: &BTreeMap<RepositoryId, String>,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        self.prepare_profile(repositories, selectors, SnapshotProfile::SelectedTrees)
            .await
    }

    async fn prepare_profile(
        &self,
        repositories: &RepositoryRegistry,
        selectors: &BTreeMap<RepositoryId, String>,
        profile: SnapshotProfile,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        for id in selectors.keys() {
            if !repositories.iter().any(|repository| repository.id() == id) {
                return Err(SnapshotError::UnknownRepository(id.clone()));
            }
        }
        let mut selected = Vec::new();
        for repository in repositories.iter() {
            if repository.policy() == RepositoryPolicy::Disabled {
                continue;
            }
            let selector = selectors
                .get(repository.id())
                .map_or_else(|| repository.default_ref(), String::as_str);
            match self.git.resolve(repository.id(), selector) {
                Ok(resolved) => {
                    selected.push(SnapshotRepository {
                        repository: repository.id().clone(),
                        commit: resolved.commit,
                        policy_digest: repository_policy_digest(repository)?,
                    });
                }
                Err(_)
                    if repository.policy() == RepositoryPolicy::Optional
                        && !selectors.contains_key(repository.id()) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let manifest = KnowledgeSnapshotManifest::with_profile(selected, None, profile)?;
        self.prepare_resolved(repositories, manifest).await
    }

    /// Read the currently active default snapshot without filesystem paths.
    ///
    /// # Errors
    ///
    /// Returns an error when the alias is missing, corrupt, or has a mismatched identity.
    pub fn default_manifest(&self) -> Result<KnowledgeSnapshotManifest, SnapshotError> {
        read_and_validate_manifest(&self.default_alias_path())
    }

    #[must_use]
    pub fn artifact_path(&self, id: &KnowledgeSnapshotId) -> PathBuf {
        self.layout.artifact(id)
    }

    /// Return a path-free preparation state for a known or persisted snapshot.
    #[must_use]
    pub fn status(&self, id: &KnowledgeSnapshotId) -> SnapshotState {
        let slot = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned();
        if let Some(slot) = slot {
            return *slot
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        load_prepared(&self.layout, id, SnapshotDisposition::Reused)
            .map_or(SnapshotState::Failed, |_| SnapshotState::Ready)
    }

    fn default_alias_path(&self) -> PathBuf {
        self.layout.root().as_path().join("default-snapshot.json")
    }

    fn load_default(
        &self,
        disposition: SnapshotDisposition,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let manifest = self.default_manifest()?;
        let artifact_path = self.layout.artifact(&manifest.id);
        validate_snapshot_artifact(&artifact_path, manifest.profile)?;
        Ok(PreparedSnapshot {
            manifest,
            artifact_path,
            disposition,
        })
    }

    fn set_state(&self, id: &KnowledgeSnapshotId, state: SnapshotState) {
        let slot = {
            let mut slots = self.slots.lock().expect("snapshot slots mutex poisoned");
            Arc::clone(slots.entry(id.clone()).or_default())
        };
        *slot.state.lock().expect("snapshot state mutex poisoned") = state;
    }

    async fn prepare_resolved(
        &self,
        repositories: &RepositoryRegistry,
        manifest: KnowledgeSnapshotManifest,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let build_permit = snapshot_build_gate()
            .acquire_owned()
            .await
            .map_err(|_| SnapshotError::Build("snapshot build gate closed".into()))?;
        let slot = {
            let mut slots = self.slots.lock().expect("snapshot slots mutex poisoned");
            Arc::clone(slots.entry(manifest.id.clone()).or_default())
        };
        let observed = *slot
            .generation
            .lock()
            .expect("snapshot generation mutex poisoned");
        let layout = self.layout.clone();
        let repositories = repositories.iter().cloned().collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            let _build_permit = build_permit;
            *slot.state.lock().expect("snapshot state mutex poisoned") = SnapshotState::Building;
            let mut generation = slot
                .generation
                .lock()
                .expect("snapshot generation mutex poisoned");
            if *generation != observed {
                drop(generation);
                let result =
                    load_prepared(&layout, &manifest.id, SnapshotDisposition::Deduplicated);
                *slot.state.lock().expect("snapshot state mutex poisoned") = result
                    .as_ref()
                    .map_or(SnapshotState::Failed, |_| SnapshotState::Ready);
                return result;
            }
            let result = build_or_reuse(&layout, &repositories, &manifest);
            if result.is_ok() {
                *generation += 1;
            }
            drop(generation);
            *slot.state.lock().expect("snapshot state mutex poisoned") = result
                .as_ref()
                .map_or(SnapshotState::Failed, |_| SnapshotState::Ready);
            result
        })
        .await?
    }
}

fn build_or_reuse(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
) -> Result<PreparedSnapshot, SnapshotError> {
    let snapshots = layout.root().as_path().join("snapshots");
    fs::create_dir_all(&snapshots)?;
    fs::create_dir_all(layout.root().as_path().join("artifacts"))?;
    let lock_path = snapshots.join(format!("{}.lock", manifest.id.as_str()));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    let snapshot_path = layout.snapshot(&manifest.id);
    let artifact_path = layout.artifact(&manifest.id);
    if snapshot_path.is_file() {
        let cached = read_and_validate_manifest(&snapshot_path)?;
        if cached != *manifest {
            return Err(SnapshotError::IdentityMismatch);
        }
        validate_snapshot_artifact(&artifact_path, cached.profile)?;
        FileExt::unlock(&lock)?;
        return Ok(PreparedSnapshot {
            manifest: cached,
            artifact_path,
            disposition: SnapshotDisposition::Reused,
        });
    }

    let sources = manifest
        .repositories
        .iter()
        .map(|selected| {
            let repository = repositories
                .iter()
                .find(|repository| repository.id() == &selected.repository)
                .ok_or_else(|| SnapshotError::UnknownRepository(selected.repository.clone()))?;
            corpus_source(layout, repository, &selected.commit)
        })
        .collect::<Result<Vec<_>, _>>()?;
    match manifest.profile {
        SnapshotProfile::SelectedTrees => {
            vesc_knowledge_index::build_git_artifacts(&artifact_path, &sources)
                .map_err(|error| SnapshotError::Build(error.to_string()))?;
        }
        SnapshotProfile::CompleteHistory => {
            let previous = previous_history(layout, &manifest.id);
            vesc_knowledge_index::build_git_history_artifacts(&artifact_path, &sources, previous)
                .map_err(|error| SnapshotError::Build(error.to_string()))?;
        }
    }
    validate_snapshot_artifact(&artifact_path, manifest.profile)?;
    write_json_atomic(&snapshot_path, manifest)?;
    FileExt::unlock(&lock)?;
    Ok(PreparedSnapshot {
        manifest: manifest.clone(),
        artifact_path,
        disposition: SnapshotDisposition::Built,
    })
}

fn load_prepared(
    layout: &KnowledgeDataLayout,
    id: &KnowledgeSnapshotId,
    disposition: SnapshotDisposition,
) -> Result<PreparedSnapshot, SnapshotError> {
    let path = layout.snapshot(id);
    let manifest = read_and_validate_manifest(&path)?;
    let artifact_path = layout.artifact(id);
    validate_snapshot_artifact(&artifact_path, manifest.profile)?;
    Ok(PreparedSnapshot {
        manifest,
        artifact_path,
        disposition,
    })
}

fn corpus_source(
    layout: &KnowledgeDataLayout,
    repository: &KnowledgeRepository,
    commit: &str,
) -> Result<GitCorpusSource, SnapshotError> {
    let repository_id = CorpusRepositoryId::try_from(repository.id().as_str())
        .map_err(|error| SnapshotError::Build(error.to_string()))?;
    let revision =
        Revision::try_from(commit).map_err(|error| SnapshotError::Build(error.to_string()))?;
    let trust_tier = match repository.trust_tier() {
        TrustTier::Official => CorpusTrustTier::FirstParty,
        TrustTier::Community | TrustTier::Untrusted => CorpusTrustTier::CuratedUpstream,
    };
    let policy = GitCorpusPolicy {
        include_patterns: repository.include().to_vec(),
        exclude_patterns: repository.exclude().to_vec(),
        ..GitCorpusPolicy::default()
    };
    Ok(GitCorpusSource {
        repository_path: layout.repository(repository.id()),
        repository_id,
        revision,
        trust_tier,
        license: LicenseStatus::Redistributable {
            spdx: repository.license().to_owned(),
        },
        policy,
    })
}

fn previous_history(
    layout: &KnowledgeDataLayout,
    current: &KnowledgeSnapshotId,
) -> Option<vesc_knowledge_index::GitHistory> {
    let alias = layout.root().as_path().join("default-snapshot.json");
    let manifest = read_and_validate_manifest(&alias).ok()?;
    if manifest.profile != SnapshotProfile::CompleteHistory || &manifest.id == current {
        return None;
    }
    vesc_knowledge_index::GitHistory::read_artifact(
        &layout.artifact(&manifest.id).join("history.json"),
    )
    .ok()
}

fn validate_snapshot_artifact(path: &Path, profile: SnapshotProfile) -> Result<(), SnapshotError> {
    let manifest =
        vesc_knowledge_index::inspect_manifest(&vesc_knowledge_index::active_manifest_path(path))
            .map_err(|error| SnapshotError::Build(error.to_string()))?;
    let lexical = path
        .join("generations")
        .join(manifest.corpus.content_digest.to_string())
        .join("lexical.json");
    drop(
        vesc_knowledge_index::LexicalIndex::open_search_artifact(&lexical)
            .map_err(|error| SnapshotError::Build(error.to_string()))?,
    );
    if profile == SnapshotProfile::CompleteHistory {
        vesc_knowledge_index::GitHistory::read_artifact(&path.join("history.json"))
            .map_err(|error| SnapshotError::Build(error.to_string()))?;
    }
    Ok(())
}

fn read_and_validate_manifest(path: &Path) -> Result<KnowledgeSnapshotManifest, SnapshotError> {
    let manifest: KnowledgeSnapshotManifest = serde_json::from_slice(&fs::read(path)?)?;
    let expected = KnowledgeSnapshotManifest::with_profile(
        manifest.repositories.clone(),
        manifest.semantic.clone(),
        manifest.profile,
    )?;
    if manifest != expected {
        return Err(SnapshotError::IdentityMismatch);
    }
    Ok(manifest)
}

fn repository_policy_digest(repository: &KnowledgeRepository) -> Result<String, SnapshotError> {
    #[derive(Serialize)]
    struct PolicyIdentity<'a> {
        include: &'a [String],
        exclude: &'a [String],
        trust_tier: TrustTier,
        license: &'a str,
        max_file_bytes: u64,
        max_files: usize,
        max_total_bytes: u64,
    }

    Ok(hex_sha256(&serde_json::to_vec(&PolicyIdentity {
        include: repository.include(),
        exclude: repository.exclude(),
        trust_tier: repository.trust_tier(),
        license: repository.license(),
        max_file_bytes: repository.max_file_bytes(),
        max_files: repository.max_files(),
        max_total_bytes: repository.max_total_bytes(),
    })?))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), SnapshotError> {
    let parent = path.parent().expect("managed snapshot path has parent");
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use crate::config::McpConfig;
    use crate::managed_repositories::{DataRoot, DataRootInputs};
    use crate::tools::search_knowledge::{
        SearchMode, SearchResponseDetail, SearchVescKnowledgeFilters, SearchVescKnowledgeParams,
        search_vesc_knowledge_tool_with_config,
    };

    fn run_git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("UTF-8 git output")
            .trim()
            .to_owned()
    }

    fn fixture_remote(root: &Path) -> (PathBuf, PathBuf, String, String) {
        let work = root.join("work");
        let remote = root.join("remote.git");
        fs::create_dir(&work).expect("create work tree");
        run_git(&work, &["init", "-b", "main"]);
        fs::write(work.join("README.md"), "alphaunique first revision\n").expect("first file");
        run_git(&work, &["add", "README.md"]);
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "first",
            ],
        );
        let first = run_git(&work, &["rev-parse", "HEAD"]);
        run_git(&work, &["tag", "v1"]);
        fs::write(work.join("README.md"), "betaunique second revision\n").expect("second file");
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-am",
                "second",
            ],
        );
        let second = run_git(&work, &["rev-parse", "HEAD"]);
        run_git(
            &work,
            &[
                "clone",
                "--bare",
                ".",
                remote.to_str().expect("UTF-8 remote path"),
            ],
        );
        (work, remote, first, second)
    }

    fn fixture_registry(data_root: &Path, default_ref: &str) -> RepositoryRegistry {
        fixture_registry_with_include(data_root, default_ref, "**/*.md")
    }

    fn fixture_registry_with_include(
        data_root: &Path,
        default_ref: &str,
        include: &str,
    ) -> RepositoryRegistry {
        fixture_registry_with_policy(data_root, default_ref, include, "required")
    }

    fn fixture_registry_with_policy(
        data_root: &Path,
        default_ref: &str,
        include: &str,
        policy: &str,
    ) -> RepositoryRegistry {
        McpConfig::from_toml(
            &format!(
                r#"
[knowledge]
data_root = "{}"

[[knowledge.repositories]]
id = "fixture"
remote_url = "https://example.invalid/fixture.git"
default_ref = "{default_ref}"
policy = "{policy}"
include = ["{include}"]
exclude = []
trust_tier = "official"
license = "MIT"
attribution = "Test fixture"
max_file_bytes = 1048576
max_files = 100
max_total_bytes = 10485760
"#,
                data_root.display()
            ),
            &DataRootInputs::default(),
        )
        .expect("fixture configuration")
        .knowledge
        .repositories
    }

    #[tokio::test]
    async fn explicit_unknown_ref_is_not_ignored_for_an_optional_repository() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories =
            fixture_registry_with_policy(&data_root, "refs/heads/main", "**/*.md", "optional");
        let id = RepositoryId::new("fixture").expect("repository ID");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let selectors = BTreeMap::from([(id, String::from("refs/tags/missing"))]);

        let error = KnowledgeSnapshotStore::new(layout)
            .prepare(&repositories, &selectors)
            .await
            .expect_err("explicit missing ref must fail");

        assert!(matches!(error, SnapshotError::ManagedGit(_)));
    }

    fn artifact_matches(root: &Path, query: &str) -> bool {
        let manifest = vesc_knowledge_index::inspect_manifest(
            &vesc_knowledge_index::active_manifest_path(root),
        )
        .expect("artifact manifest");
        let lexical = vesc_knowledge_index::LexicalIndex::open_artifact(
            &root
                .join("generations")
                .join(manifest.corpus.content_digest.to_string())
                .join("lexical.json"),
        )
        .expect("lexical artifact");
        !lexical
            .search(query, &vesc_knowledge_index::LexicalFilters::default(), 1)
            .expect("search fixture")
            .is_empty()
    }

    fn assert_default_and_prewarm_profiles(prepared: &PreparedSnapshots) {
        assert_eq!(
            prepared.default.manifest.profile,
            SnapshotProfile::CompleteHistory
        );
        assert!(
            prepared
                .prewarmed
                .iter()
                .all(|snapshot| snapshot.manifest.profile == SnapshotProfile::SelectedTrees)
        );
        assert!(
            prepared
                .default
                .artifact_path
                .join("history.json")
                .is_file()
        );
        assert!(
            prepared
                .prewarmed
                .iter()
                .all(|snapshot| !snapshot.artifact_path.join("history.json").exists())
        );
    }

    fn selected(repository: RepositoryId, commit: String) -> SnapshotRepository {
        SnapshotRepository {
            repository,
            commit,
            policy_digest: String::from("fixture-policy-v1"),
        }
    }

    #[test]
    fn snapshot_identity_is_order_independent_and_commit_specific() {
        let one = RepositoryId::new("one").expect("valid id");
        let two = RepositoryId::new("two").expect("valid id");
        let left = KnowledgeSnapshotManifest::new(
            vec![
                selected(two.clone(), "b".repeat(40)),
                selected(one.clone(), "a".repeat(40)),
            ],
            None,
        )
        .expect("manifest");
        let same = KnowledgeSnapshotManifest::new(
            vec![
                selected(one.clone(), "a".repeat(40)),
                selected(two.clone(), "b".repeat(40)),
            ],
            None,
        )
        .expect("manifest");
        let moved = KnowledgeSnapshotManifest::new(
            vec![selected(one, "c".repeat(40)), selected(two, "b".repeat(40))],
            None,
        )
        .expect("manifest");
        let mut policy_changed_repositories = same.repositories.clone();
        policy_changed_repositories[0].policy_digest = String::from("fixture-policy-v2");
        let policy_changed = KnowledgeSnapshotManifest::new(policy_changed_repositories, None)
            .expect("policy-specific manifest");
        let complete_history = KnowledgeSnapshotManifest::with_profile(
            same.repositories.clone(),
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("history manifest");

        assert_eq!(left, same);
        assert_ne!(left.id, moved.id);
        assert_ne!(left.id, policy_changed.id);
        assert_ne!(left.id, complete_history.id);
        assert_eq!(left.id.as_str().len(), 64);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn configured_default_and_two_historical_snapshots_coexist_and_reuse_artifacts() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first, second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let store = KnowledgeSnapshotStore::new(layout.clone());
        let historical = BTreeMap::from([(id.clone(), String::from("refs/tags/v1"))]);
        let current = BTreeMap::from([(id, String::from("refs/heads/main"))]);

        let prepared = store
            .prepare_configured(&repositories, &[historical.clone(), current])
            .await
            .expect("configured snapshots");

        assert_eq!(prepared.default.manifest.repositories[0].commit, second);
        assert_eq!(prepared.prewarmed[0].manifest.repositories[0].commit, first);
        assert_eq!(
            prepared.prewarmed[1].manifest.repositories[0].commit,
            second
        );
        assert_default_and_prewarm_profiles(&prepared);
        assert_ne!(
            prepared.default.manifest.id,
            prepared.prewarmed[0].manifest.id
        );
        assert!(artifact_matches(
            &prepared.default.artifact_path,
            "betaunique"
        ));
        assert!(artifact_matches(
            &prepared.prewarmed[0].artifact_path,
            "alphaunique"
        ));
        assert!(artifact_matches(
            &prepared.prewarmed[1].artifact_path,
            "betaunique"
        ));
        assert!(!artifact_matches(
            &prepared.prewarmed[1].artifact_path,
            "alphaunique"
        ));
        assert_eq!(
            store.default_manifest().expect("default alias"),
            prepared.default.manifest
        );
        let response = search_vesc_knowledge_tool_with_config(
            &SearchVescKnowledgeParams {
                query: String::from("betaunique"),
                snapshot_id: None,
                category: None,
                limit: 1,
                mode: Some(SearchMode::Lexical),
                filters: SearchVescKnowledgeFilters::default(),
                max_response_bytes: None,
                max_context_bytes: None,
                detail: SearchResponseDetail::Full,
            },
            &crate::config::KnowledgeConfig {
                mode: crate::config::RetrievalMode::Lexical,
                data_root: Some(DataRoot::new(data_root.clone()).expect("absolute data root")),
                repositories: repositories.clone(),
                ..crate::config::KnowledgeConfig::default()
            },
        );
        let index = response.index.expect("managed snapshot metadata");
        assert_eq!(
            index.snapshot_id.as_deref(),
            Some(prepared.default.manifest.id.as_str())
        );
        assert_eq!(index.snapshot_profile.as_deref(), Some("complete_history"));
        assert_eq!(index.repositories.get("fixture"), Some(&second));
        assert_eq!(
            store.status(&prepared.default.manifest.id),
            SnapshotState::Ready
        );
        assert_eq!(
            store
                .prepare(&repositories, &historical)
                .await
                .expect("reused historical snapshot")
                .disposition,
            SnapshotDisposition::Reused
        );
        assert_eq!(
            fs::read_dir(layout.root().as_path().join("snapshots"))
                .expect("snapshot directory")
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json"))
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn snapshot_build_gate_allows_one_working_set() {
        let first = snapshot_build_gate()
            .acquire_owned()
            .await
            .expect("snapshot build gate");
        assert!(snapshot_build_gate().try_acquire_owned().is_err());
        drop(first);
    }

    #[tokio::test]
    async fn snapshot_build_waits_for_global_working_set() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let store = KnowledgeSnapshotStore::new(layout);
        let held = snapshot_build_gate()
            .acquire_owned()
            .await
            .expect("snapshot build gate");
        let build = tokio::spawn(async move {
            store
                .prepare(&repositories, &BTreeMap::new())
                .await
                .expect("snapshot build")
        });

        tokio::task::yield_now().await;
        assert!(!build.is_finished());
        drop(held);
        assert_eq!(
            build.await.expect("snapshot task").disposition,
            SnapshotDisposition::Built
        );
    }

    #[tokio::test]
    async fn concurrent_requests_build_one_snapshot() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let store = KnowledgeSnapshotStore::new(layout);
        let selectors = BTreeMap::new();

        let (left, right) = tokio::join!(
            store.prepare(&repositories, &selectors),
            store.prepare(&repositories, &selectors),
        );
        let left = left.expect("left snapshot");
        let right = right.expect("right snapshot");

        assert_eq!(left.manifest, right.manifest);
        assert_eq!(
            [left.disposition, right.disposition]
                .into_iter()
                .filter(|disposition| *disposition == SnapshotDisposition::Built)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cached_complete_history_default_restarts_without_remote_access() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let initial = KnowledgeSnapshotStore::new(layout.clone())
            .prepare_default(&repositories)
            .await
            .expect("initial default");
        fs::remove_dir_all(remote).expect("remove fixture remote");

        let restarted = KnowledgeSnapshotStore::new(layout)
            .prepare_default(&repositories)
            .await
            .expect("cached offline default");

        assert_eq!(restarted.manifest, initial.manifest);
        assert_eq!(restarted.disposition, SnapshotDisposition::Reused);
        assert!(artifact_matches(&restarted.artifact_path, "alphaunique"));
        assert!(artifact_matches(&restarted.artifact_path, "betaunique"));
    }

    #[tokio::test]
    async fn moved_default_branch_retains_the_previous_snapshot() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        let git = ManagedGitStore::new(layout.clone());
        git.sync_source(
            &id,
            remote.to_str().expect("UTF-8 remote path"),
            "refs/heads/main",
        )
        .await
        .expect("initial repository sync");
        let store = KnowledgeSnapshotStore::new(layout);
        let previous = store
            .prepare_default(&repositories)
            .await
            .expect("initial default snapshot");

        fs::write(work.join("README.md"), "gammaunique third revision\n").expect("third file");
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-am",
                "third",
            ],
        );
        run_git(
            &work,
            &["push", remote.to_str().expect("UTF-8 remote path"), "main"],
        );
        git.sync_source(
            &id,
            remote.to_str().expect("UTF-8 remote path"),
            "refs/heads/main",
        )
        .await
        .expect("moved repository sync");
        let current = store
            .prepare_default(&repositories)
            .await
            .expect("moved default snapshot");

        assert_ne!(previous.manifest.id, current.manifest.id);
        assert!(artifact_matches(&previous.artifact_path, "betaunique"));
        assert!(artifact_matches(&current.artifact_path, "alphaunique"));
        assert!(artifact_matches(&current.artifact_path, "betaunique"));
        assert!(artifact_matches(&current.artifact_path, "gammaunique"));
        let previous_history = vesc_knowledge_index::GitHistory::read_artifact(
            &previous.artifact_path.join("history.json"),
        )
        .expect("previous history");
        let current_history = vesc_knowledge_index::GitHistory::read_artifact(
            &current.artifact_path.join("history.json"),
        )
        .expect("current history");
        assert_eq!(previous_history.commits.len(), 2);
        assert_eq!(current_history.commits.len(), 3);
        assert_eq!(current_history.contents.len(), 3);
        assert_eq!(
            store.default_manifest().expect("default alias").id,
            current.manifest.id
        );
    }

    #[tokio::test]
    async fn failed_default_refresh_keeps_the_last_valid_snapshot_searchable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let store = KnowledgeSnapshotStore::new(layout);
        let first = store
            .prepare_default(&repositories)
            .await
            .expect("initial default");

        let stale = store
            .prepare_default(&fixture_registry(&data_root, "refs/heads/missing"))
            .await
            .expect("last default remains available");

        assert_eq!(stale.manifest.id, first.manifest.id);
        assert_eq!(stale.disposition, SnapshotDisposition::Stale);
        assert_eq!(store.status(&stale.manifest.id), SnapshotState::Stale);
        assert!(artifact_matches(&stale.artifact_path, "betaunique"));
    }

    #[tokio::test]
    async fn failed_default_build_keeps_the_last_valid_snapshot_searchable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let store = KnowledgeSnapshotStore::new(layout);
        let initial = store
            .prepare_default(&repositories)
            .await
            .expect("initial default");

        let stale = store
            .prepare_default(&fixture_registry_with_include(
                &data_root,
                "refs/heads/main",
                "[unsupported-glob]",
            ))
            .await
            .expect("last default survives failed build");

        assert_eq!(stale.manifest.id, initial.manifest.id);
        assert_eq!(stale.disposition, SnapshotDisposition::Stale);
        assert!(artifact_matches(&stale.artifact_path, "betaunique"));
    }
}
