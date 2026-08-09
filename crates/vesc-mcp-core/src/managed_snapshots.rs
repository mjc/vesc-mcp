//! Immutable multi-repository knowledge snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vesc_knowledge_index::corpus::git::{GitCorpusLimits, GitCorpusPolicy, GitCorpusSource};
use vesc_knowledge_index::corpus::{
    LicenseStatus, RepositoryId as CorpusRepositoryId, Revision, TrustTier as CorpusTrustTier,
};

use crate::config::{KnowledgeConfig, RetrievalMode, SemanticIngestionProvider};
use crate::managed_git::{ManagedGitError, ManagedGitStore};
pub use crate::managed_repositories::KnowledgeSnapshotId;
use crate::managed_repositories::{
    KnowledgeDataLayout, KnowledgeRepository, RepositoryId, RepositoryPolicy, RepositoryRegistry,
    TrustTier,
};

mod publication;

#[cfg(test)]
use publication::DistributedPublicationBoundary;

const LEGACY_SNAPSHOT_SCHEMA: u16 = 2;
const SNAPSHOT_SCHEMA: u16 = 3;

// Avoid queueing duplicate build tasks inside one process. The filesystem lock
// below extends the same one-working-set limit across preparation children.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotBuildPolicy {
    AllowInference,
    CachedVectorsOnly,
}

/// Optional semantic identity included when a snapshot contains vectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSemanticModel {
    pub model_id: String,
    pub model_revision: String,
    pub max_length: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingestion: Option<SnapshotSemanticIngestion>,
}

/// Reproducible bulk-ingestion contract included in snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSemanticIngestion {
    pub model_sha256: String,
    pub provider: SemanticIngestionProvider,
    pub device_id: i32,
    pub max_length: usize,
    pub batch_size: usize,
    pub window_aggregation: vesc_knowledge_index::WindowAggregation,
}

#[derive(Clone)]
struct SnapshotSemanticConfig {
    model_dir: PathBuf,
    model: SnapshotSemanticModel,
}

/// One immutable repository selection in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRepository {
    pub repository: RepositoryId,
    pub commit: String,
    #[serde(default)]
    pub history_tips: Vec<String>,
    pub policy_digest: String,
}

/// One configured repository contract, including sources unavailable at build time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotRepositoryConfiguration {
    pub repository: RepositoryId,
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
    pub configured_repositories: Vec<SnapshotRepositoryConfiguration>,
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
        repositories: Vec<SnapshotRepository>,
        semantic: Option<SnapshotSemanticModel>,
        profile: SnapshotProfile,
    ) -> Result<Self, SnapshotError> {
        let configured_repositories = repositories
            .iter()
            .map(|selected| SnapshotRepositoryConfiguration {
                repository: selected.repository.clone(),
                policy_digest: selected.policy_digest.clone(),
            })
            .collect();
        Self::with_profile_and_configuration(
            repositories,
            configured_repositories,
            semantic,
            profile,
        )
    }

    fn with_profile_and_configuration(
        mut repositories: Vec<SnapshotRepository>,
        mut configured_repositories: Vec<SnapshotRepositoryConfiguration>,
        semantic: Option<SnapshotSemanticModel>,
        profile: SnapshotProfile,
    ) -> Result<Self, SnapshotError> {
        if repositories.is_empty() {
            return Err(SnapshotError::EmptySelection);
        }
        for repository in &mut repositories {
            repository.history_tips.sort_unstable();
            repository.history_tips.dedup();
            if repository.history_tips.is_empty()
                || !repository.history_tips.contains(&repository.commit)
                || profile == SnapshotProfile::SelectedTrees
                    && repository.history_tips.as_slice() != [repository.commit.as_str()]
            {
                return Err(SnapshotError::InvalidHistoryTips);
            }
        }
        repositories.sort_by(|left, right| left.repository.cmp(&right.repository));
        configured_repositories.sort();
        if repositories
            .windows(2)
            .any(|pair| pair[0].repository == pair[1].repository)
            || configured_repositories
                .windows(2)
                .any(|pair| pair[0].repository == pair[1].repository)
            || repositories.iter().any(|selected| {
                !configured_repositories.iter().any(|configured| {
                    configured.repository == selected.repository
                        && configured.policy_digest == selected.policy_digest
                })
            })
        {
            return Err(SnapshotError::DuplicateRepository);
        }
        let component_versions = vesc_knowledge_index::artifact_component_versions();
        let identity = SnapshotIdentity {
            schema: SNAPSHOT_SCHEMA,
            profile,
            repositories: &repositories,
            configured_repositories: &configured_repositories,
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
            configured_repositories,
            component_versions,
            semantic,
        })
    }

    fn has_valid_identity(&self) -> bool {
        if self.repositories.is_empty()
            || self.configured_repositories.is_empty()
            || self
                .repositories
                .windows(2)
                .any(|pair| pair[0].repository >= pair[1].repository)
            || self
                .configured_repositories
                .windows(2)
                .any(|pair| pair[0].repository >= pair[1].repository)
            || self.repositories.iter().any(|selected| {
                !self.configured_repositories.iter().any(|configured| {
                    configured.repository == selected.repository
                        && configured.policy_digest == selected.policy_digest
                })
            })
        {
            return false;
        }
        if self.schema == LEGACY_SNAPSHOT_SCHEMA
            && self
                .repositories
                .iter()
                .all(|repository| repository.history_tips.is_empty())
        {
            let repositories = self
                .repositories
                .iter()
                .map(|repository| LegacySnapshotRepository {
                    repository: &repository.repository,
                    commit: &repository.commit,
                    policy_digest: &repository.policy_digest,
                })
                .collect::<Vec<_>>();
            let identity = LegacySnapshotIdentity {
                schema: self.schema,
                profile: self.profile,
                repositories: &repositories,
                configured_repositories: &self.configured_repositories,
                component_versions: &self.component_versions,
                semantic: self.semantic.as_ref(),
            };
            return serde_json::to_vec(&identity)
                .ok()
                .and_then(|identity| KnowledgeSnapshotId::new(hex_sha256(&identity)).ok())
                .is_some_and(|id| id == self.id);
        }
        if self.schema != SNAPSHOT_SCHEMA
            || self
                .repositories
                .iter()
                .any(|repository| !history_tips_are_canonical(self.profile, repository))
        {
            return false;
        }
        let identity = SnapshotIdentity {
            schema: self.schema,
            profile: self.profile,
            repositories: &self.repositories,
            configured_repositories: &self.configured_repositories,
            component_versions: &self.component_versions,
            semantic: self.semantic.as_ref(),
        };
        serde_json::to_vec(&identity)
            .ok()
            .and_then(|identity| KnowledgeSnapshotId::new(hex_sha256(&identity)).ok())
            .is_some_and(|id| id == self.id)
    }

    /// Return whether this snapshot uses the component formats written by this build.
    #[must_use]
    pub fn uses_current_components(&self) -> bool {
        self.component_versions == vesc_knowledge_index::artifact_component_versions()
    }
}

fn history_tips_are_canonical(profile: SnapshotProfile, repository: &SnapshotRepository) -> bool {
    let canonical = !repository.history_tips.is_empty()
        && repository
            .history_tips
            .iter()
            .any(|tip| tip == &repository.commit)
        && repository
            .history_tips
            .windows(2)
            .all(|pair| pair[0] < pair[1]);
    canonical
        && match profile {
            SnapshotProfile::SelectedTrees => {
                repository.history_tips.as_slice() == [repository.commit.as_str()]
            }
            SnapshotProfile::CompleteHistory => true,
        }
}

#[derive(Serialize)]
struct SnapshotIdentity<'a> {
    schema: u16,
    profile: SnapshotProfile,
    repositories: &'a [SnapshotRepository],
    configured_repositories: &'a [SnapshotRepositoryConfiguration],
    component_versions: &'a BTreeMap<String, String>,
    semantic: Option<&'a SnapshotSemanticModel>,
}

#[derive(Serialize)]
struct LegacySnapshotRepository<'a> {
    repository: &'a RepositoryId,
    commit: &'a str,
    policy_digest: &'a str,
}

#[derive(Serialize)]
struct LegacySnapshotIdentity<'a> {
    schema: u16,
    profile: SnapshotProfile,
    repositories: &'a [LegacySnapshotRepository<'a>],
    configured_repositories: &'a [SnapshotRepositoryConfiguration],
    component_versions: &'a BTreeMap<String, String>,
    semantic: Option<&'a SnapshotSemanticModel>,
}

fn semantic_serving_contract_matches(
    snapshot: Option<&SnapshotSemanticModel>,
    configured: Option<&SnapshotSemanticModel>,
) -> bool {
    match (snapshot, configured) {
        (None, None) => true,
        (Some(snapshot), Some(configured)) => {
            snapshot.model_id == configured.model_id
                && snapshot.model_revision == configured.model_revision
                && snapshot.max_length == configured.max_length
                && configured
                    .ingestion
                    .as_ref()
                    .is_none_or(|ingestion| snapshot.ingestion.as_ref() == Some(ingestion))
        }
        _ => false,
    }
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

/// Live phase of a snapshot build, excluding repository synchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotBuildPhase {
    PlanningHistory,
    BuildingLexicalIndex,
    BuildingSemanticIndex,
    Publishing,
}

type SnapshotProgressReporter = dyn Fn(SnapshotBuildPhase) + Send + Sync;

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
    #[error("snapshot history tips are empty or do not contain the selected commit")]
    InvalidHistoryTips,
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
    #[error("snapshot is not cached on this host: {0}")]
    NotCached(KnowledgeSnapshotId),
}

impl SnapshotError {
    const fn source_is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::ManagedGit(ManagedGitError::Storage(_) | ManagedGitError::Git(_))
        )
    }
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
    semantic: Option<SnapshotSemanticConfig>,
    progress: Option<Arc<SnapshotProgressReporter>>,
}

impl KnowledgeSnapshotStore {
    #[must_use]
    pub fn new(layout: KnowledgeDataLayout) -> Self {
        Self {
            git: ManagedGitStore::new(layout.clone()),
            layout,
            slots: Arc::new(Mutex::new(HashMap::new())),
            semantic: None,
            progress: None,
        }
    }

    /// Report live phase transitions for snapshot builds.
    #[must_use]
    pub fn with_progress_reporter(
        mut self,
        reporter: impl Fn(SnapshotBuildPhase) + Send + Sync + 'static,
    ) -> Self {
        self.progress = Some(Arc::new(reporter));
        self
    }

    /// Configure semantic vectors for newly built snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error when only part of the semantic model contract is configured.
    pub fn with_semantic_config(mut self, config: &KnowledgeConfig) -> Result<Self, SnapshotError> {
        if config.mode == RetrievalMode::Lexical {
            self.semantic = None;
            return Ok(self);
        }
        self.semantic = match (
            config.semantic_model_dir.clone(),
            config.semantic_model_id.clone(),
            config.semantic_model_revision.clone(),
        ) {
            (None, None, None) if config.semantic_max_length.is_none() => None,
            (Some(model_dir), Some(model_id), Some(model_revision)) => {
                let profile = vesc_knowledge_index::EmbeddingProfile::for_model_id(&model_id)
                    .ok_or_else(|| {
                        SnapshotError::Build(format!(
                            "no embedding profile is registered for {model_id}"
                        ))
                    })?;
                let max_length = config.semantic_max_length.unwrap_or(profile.max_length);
                if max_length == 0 || max_length > profile.max_length {
                    return Err(SnapshotError::Build(format!(
                        "semantic max length must be between 1 and {} for {model_id}",
                        profile.max_length
                    )));
                }
                let (model_dir, ingestion) = config.semantic_ingestion.as_ref().map_or_else(
                    || Ok((model_dir, None)),
                    |ingestion| {
                        if ingestion.max_length == 0
                            || ingestion.max_length > profile.max_length
                            || ingestion.batch_size == 0
                        {
                            return Err(SnapshotError::Build(format!(
                                "semantic ingestion max length must be between 1 and {} and batch size must be nonzero for {model_id}",
                                profile.max_length
                            )));
                        }
                        let actual = vesc_knowledge_index::hardware::sha256_file(
                            &ingestion.model_dir.join("model.onnx"),
                        )
                        .map_err(|error| {
                            SnapshotError::Build(format!(
                                "read semantic ingestion model: {error}"
                            ))
                        })?;
                        if !actual.eq_ignore_ascii_case(&ingestion.model_sha256) {
                            return Err(SnapshotError::Build(
                                "semantic ingestion model SHA-256 does not match configuration"
                                    .into(),
                            ));
                        }
                        Ok((
                            ingestion.model_dir.clone(),
                            Some(SnapshotSemanticIngestion {
                                model_sha256: ingestion.model_sha256.to_ascii_lowercase(),
                                provider: ingestion.provider,
                                device_id: ingestion.device_id,
                                max_length: ingestion.max_length,
                                batch_size: ingestion.batch_size,
                                window_aggregation: ingestion.window_aggregation,
                            }),
                        ))
                    },
                )?;
                Some(SnapshotSemanticConfig {
                    model_dir,
                    model: SnapshotSemanticModel {
                        model_id,
                        model_revision,
                        max_length,
                        ingestion,
                    },
                })
            }
            _ => {
                return Err(SnapshotError::Build(
                    "semantic model directory, identity, and revision must be configured together"
                        .into(),
                ));
            }
        };
        Ok(self)
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
        if let Ok(prepared) =
            self.load_compatible_default(repositories, SnapshotDisposition::Reused, false)
        {
            self.set_state(&prepared.manifest.id, SnapshotState::Ready);
            return Ok(prepared);
        }
        let prepared = match self
            .prepare_profile(
                repositories,
                &BTreeMap::new(),
                SnapshotProfile::CompleteHistory,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(error) if error.source_is_unavailable() => {
                tracing::warn!(
                    %error,
                    "managed snapshot preparation failed; serving the last compatible snapshot"
                );
                return match self.load_compatible_default(
                    repositories,
                    SnapshotDisposition::Stale,
                    true,
                ) {
                    Ok(stale) => {
                        self.set_state(&stale.manifest.id, SnapshotState::Stale);
                        Ok(stale)
                    }
                    Err(_) => Err(error),
                };
            }
            Err(error) => return Err(error),
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
        let manifest = self.resolve_manifest(repositories, selectors, profile)?;
        self.prepare_resolved(repositories, manifest, SnapshotBuildPolicy::AllowInference)
            .await
    }

    /// Derive a selected snapshot without allowing semantic inference.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::NotCached`] when the cached vector corpus does
    /// not contain every selected chunk.
    pub async fn derive_cached(
        &self,
        repositories: &RepositoryRegistry,
        selectors: &BTreeMap<RepositoryId, String>,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let manifest =
            self.resolve_manifest(repositories, selectors, SnapshotProfile::SelectedTrees)?;
        let configured = repositories.iter().cloned().collect::<Vec<_>>();
        if let Some(prepared) = load_reusable_snapshot(
            &self.layout,
            &configured,
            &manifest,
            SnapshotDisposition::Reused,
        )? {
            return Ok(prepared);
        }
        if self.semantic.is_none() {
            return Err(SnapshotError::NotCached(manifest.id));
        }
        self.prepare_resolved(
            repositories,
            manifest,
            SnapshotBuildPolicy::CachedVectorsOnly,
        )
        .await
    }

    /// Resolve a selected snapshot and reuse it without building missing data.
    ///
    /// # Errors
    ///
    /// Returns an error when the selection is invalid or its complete artifact is not cached.
    pub fn reuse(
        &self,
        repositories: &RepositoryRegistry,
        selectors: &BTreeMap<RepositoryId, String>,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let manifest =
            self.resolve_manifest(repositories, selectors, SnapshotProfile::SelectedTrees)?;
        let repositories = repositories.iter().cloned().collect::<Vec<_>>();
        load_reusable_snapshot(
            &self.layout,
            &repositories,
            &manifest,
            SnapshotDisposition::Reused,
        )?
        .ok_or_else(|| SnapshotError::NotCached(manifest.id))
    }

    fn resolve_manifest(
        &self,
        repositories: &RepositoryRegistry,
        selectors: &BTreeMap<RepositoryId, String>,
        profile: SnapshotProfile,
    ) -> Result<KnowledgeSnapshotManifest, SnapshotError> {
        for id in selectors.keys() {
            if !repositories.iter().any(|repository| repository.id() == id) {
                return Err(SnapshotError::UnknownRepository(id.clone()));
            }
        }
        let configured_repositories = repository_configuration(repositories)?;
        let mut selected = Vec::new();
        for repository in repositories.enabled() {
            let selector = selectors
                .get(repository.id())
                .map_or_else(|| repository.default_ref(), String::as_str);
            match self
                .git
                .resolve_configured(repository, selector)
                .and_then(|resolved| {
                    let history_tips = match profile {
                        SnapshotProfile::SelectedTrees => vec![resolved.commit.clone()],
                        SnapshotProfile::CompleteHistory => {
                            self.git.history_tips_configured(repository)?
                        }
                    };
                    Ok((resolved, history_tips))
                }) {
                Ok((resolved, history_tips)) => {
                    selected.push(SnapshotRepository {
                        repository: repository.id().clone(),
                        commit: resolved.commit,
                        history_tips,
                        policy_digest: repository_policy_digest(repository)?,
                    });
                }
                Err(ManagedGitError::RemoteUrlChanged { .. })
                    if repository.policy() == RepositoryPolicy::Optional => {}
                Err(_)
                    if repository.policy() == RepositoryPolicy::Optional
                        && !selectors.contains_key(repository.id()) => {}
                Err(error) => return Err(error.into()),
            }
        }
        KnowledgeSnapshotManifest::with_profile_and_configuration(
            selected,
            configured_repositories,
            self.semantic
                .as_ref()
                .map(|semantic| semantic.model.clone()),
            profile,
        )
    }

    /// Read the currently active default snapshot without filesystem paths.
    ///
    /// # Errors
    ///
    /// Returns an error when the alias is missing, corrupt, or has a mismatched identity.
    pub fn default_manifest(&self) -> Result<KnowledgeSnapshotManifest, SnapshotError> {
        let manifest: KnowledgeSnapshotManifest =
            serde_json::from_slice(&crate::read_default_snapshot(self.layout.root().as_path())?)?;
        if !manifest.has_valid_identity() {
            return Err(SnapshotError::IdentityMismatch);
        }
        Ok(manifest)
    }

    /// Return whether the default snapshot still matches configured sources and refs.
    ///
    /// This compares component, semantic, repository, policy, and resolved
    /// default-ref identities without fetching remotes or rebuilding the artifact.
    ///
    /// # Errors
    ///
    /// Returns an error when the default alias is missing or invalid.
    pub fn default_configuration_is_current(
        &self,
        repositories: &RepositoryRegistry,
    ) -> Result<bool, SnapshotError> {
        let manifest = self.default_manifest()?;
        self.default_is_compatible(&manifest, repositories, true)
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
        crate::default_snapshot_path(self.layout.root().as_path())
    }

    fn load_default(
        &self,
        disposition: SnapshotDisposition,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let manifest = self.default_manifest()?;
        let artifact_path = self.layout.artifact(&manifest.id);
        validate_snapshot_artifact(&artifact_path, &manifest)?;
        Ok(PreparedSnapshot {
            manifest,
            artifact_path,
            disposition,
        })
    }

    fn load_compatible_default(
        &self,
        repositories: &RepositoryRegistry,
        disposition: SnapshotDisposition,
        allow_unavailable: bool,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let prepared = self.load_default(disposition)?;
        if self.default_is_compatible(&prepared.manifest, repositories, allow_unavailable)? {
            Ok(prepared)
        } else {
            Err(SnapshotError::IdentityMismatch)
        }
    }

    fn default_is_compatible(
        &self,
        manifest: &KnowledgeSnapshotManifest,
        repositories: &RepositoryRegistry,
        allow_unavailable: bool,
    ) -> Result<bool, SnapshotError> {
        if !self.snapshot_contract_matches(manifest, repositories)? {
            return Ok(false);
        }

        for repository in repositories.enabled() {
            let selected = manifest
                .repositories
                .iter()
                .find(|selected| selected.repository == *repository.id());
            match self
                .git
                .resolve_configured(repository, repository.default_ref())
                .and_then(|resolved| Ok((resolved, self.git.history_tips_configured(repository)?)))
            {
                Ok((resolved, history_tips))
                    if selected.is_some_and(|selected| {
                        manifest.profile == SnapshotProfile::CompleteHistory
                            && selected.commit == resolved.commit
                            && selected.history_tips == history_tips
                            || manifest.profile == SnapshotProfile::SelectedTrees
                                && selected.commit == resolved.commit
                                && selected.history_tips == history_tips
                    }) => {}
                Err(ManagedGitError::Storage(_) | ManagedGitError::Git(_))
                    if allow_unavailable
                        && (selected.is_some()
                            || repository.policy() == RepositoryPolicy::Optional) => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn snapshot_contract_matches(
        &self,
        manifest: &KnowledgeSnapshotManifest,
        repositories: &RepositoryRegistry,
    ) -> Result<bool, SnapshotError> {
        if manifest.profile != SnapshotProfile::CompleteHistory
            || !manifest.uses_current_components()
            || !semantic_serving_contract_matches(
                manifest.semantic.as_ref(),
                self.semantic.as_ref().map(|value| &value.model),
            )
        {
            return Ok(false);
        }

        let configured_repositories = repository_configuration(repositories)?;
        if manifest.configured_repositories != configured_repositories {
            return Ok(false);
        }
        for repository in repositories
            .enabled()
            .filter(|repository| repository.policy() == RepositoryPolicy::Required)
        {
            if !manifest
                .repositories
                .iter()
                .any(|selected| selected.repository == *repository.id())
            {
                return Ok(false);
            }
        }
        Ok(true)
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
        policy: SnapshotBuildPolicy,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let repositories = repositories.iter().cloned().collect::<Vec<_>>();
        if let Some(prepared) = load_reusable_snapshot(
            &self.layout,
            &repositories,
            &manifest,
            SnapshotDisposition::Reused,
        )? {
            return Ok(prepared);
        }
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
        let semantic = self.semantic.clone();
        let progress = self.progress.clone();
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
            let result = build_or_reuse(
                &layout,
                &repositories,
                &manifest,
                semantic.as_ref(),
                policy,
                progress.as_deref(),
            );
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

fn snapshot_corpus_sources(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
) -> Result<Vec<GitCorpusSource>, SnapshotError> {
    manifest
        .repositories
        .iter()
        .map(|selected| {
            let repository = repositories
                .iter()
                .find(|repository| repository.id() == &selected.repository)
                .ok_or_else(|| SnapshotError::UnknownRepository(selected.repository.clone()))?;
            corpus_source(layout, repository, &selected.commit, &selected.history_tips)
        })
        .collect()
}

fn build_or_reuse(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
    semantic: Option<&SnapshotSemanticConfig>,
    policy: SnapshotBuildPolicy,
    progress: Option<&SnapshotProgressReporter>,
) -> Result<PreparedSnapshot, SnapshotError> {
    if let Some(prepared) =
        load_reusable_snapshot(layout, repositories, manifest, SnapshotDisposition::Reused)?
    {
        cleanup_completed_lexical_stage(&prepared.artifact_path);
        cleanup_completed_vector_checkpoint(layout, &prepared.manifest.id);
        cleanup_abandoned_artifact_staging_if_idle(layout);
        return Ok(prepared);
    }
    let _build_lock = acquire_snapshot_build_lock(layout)?;
    let snapshots = layout.root().as_path().join("snapshots");
    fs::create_dir_all(&snapshots)?;
    fs::create_dir_all(layout.root().as_path().join("artifacts"))?;
    remove_abandoned_artifact_staging(layout)?;
    prune_obsolete_incomplete_snapshots(layout, &manifest.id)?;
    let lock_path = snapshots.join(format!("{}.lock", manifest.id.as_str()));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    lock.lock_exclusive()?;

    let snapshot_path = layout.snapshot(&manifest.id);
    let artifact_path = layout.artifact(&manifest.id);
    if let Some(prepared) = load_reusable_snapshot(
        layout,
        repositories,
        manifest,
        SnapshotDisposition::Deduplicated,
    )? {
        cleanup_completed_lexical_stage(&prepared.artifact_path);
        cleanup_completed_vector_checkpoint(layout, &prepared.manifest.id);
        FileExt::unlock(&lock)?;
        return Ok(prepared);
    }

    let sources = snapshot_corpus_sources(layout, repositories, manifest)?;
    let build = match build_snapshot_artifact(
        layout,
        repositories,
        manifest,
        &artifact_path,
        &sources,
        SnapshotArtifactBuildOptions {
            semantic,
            policy,
            progress,
        },
    ) {
        Ok(build) => build,
        Err(error) => {
            FileExt::unlock(&lock)?;
            drop(lock);
            if policy == SnapshotBuildPolicy::CachedVectorsOnly {
                cleanup_failed_cached_build(&artifact_path, &lock_path);
            }
            return Err(error);
        }
    };
    if semantic.is_some() && !build.has_vectors {
        return Err(SnapshotError::Build(
            "semantic snapshot vector artifact is unavailable".into(),
        ));
    }
    validate_snapshot_artifact(&artifact_path, manifest)?;
    pin_snapshot_commits(layout, manifest)?;
    write_json_atomic(&snapshot_path, manifest)?;
    cleanup_completed_lexical_stage(&artifact_path);
    if let Some(path) = build.vector_checkpoint_path {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, "failed to remove completed vector checkpoint"),
        }
    }
    FileExt::unlock(&lock)?;
    Ok(PreparedSnapshot {
        manifest: manifest.clone(),
        artifact_path,
        disposition: SnapshotDisposition::Built,
    })
}

fn cleanup_failed_cached_build(artifact_path: &Path, lock_path: &Path) {
    if let Err(error) = fs::remove_dir(artifact_path)
        && error.kind() != std::io::ErrorKind::NotFound
        && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
    {
        tracing::warn!(%error, "failed to remove cached-only artifact staging directory");
    }
    if let Err(error) = remove_if_present(lock_path) {
        tracing::warn!(%error, "failed to remove cached-only snapshot lock");
    }
}

fn cleanup_completed_lexical_stage(artifact_path: &Path) {
    if let Err(error) = vesc_knowledge_index::remove_git_history_lexical_stage(artifact_path) {
        tracing::warn!(%error, "failed to remove completed lexical stage");
    }
}

fn cleanup_completed_vector_checkpoint(layout: &KnowledgeDataLayout, id: &KnowledgeSnapshotId) {
    if let Err(error) = remove_if_present(&vector_checkpoint_path(layout, id)) {
        tracing::warn!(%error, "failed to remove completed vector checkpoint");
    }
}

fn load_reusable_snapshot(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
    disposition: SnapshotDisposition,
) -> Result<Option<PreparedSnapshot>, SnapshotError> {
    let snapshot_path = layout.snapshot(&manifest.id);
    if snapshot_path.is_file() {
        let cached = read_and_validate_manifest(&snapshot_path)?;
        if cached != *manifest {
            return Err(SnapshotError::IdentityMismatch);
        }
        if let Some(prepared) = load_validated_snapshot(layout, repositories, cached, disposition)?
        {
            return Ok(Some(prepared));
        }
    }

    load_serving_compatible_snapshot(layout, repositories, manifest, disposition)
}

fn load_serving_compatible_snapshot(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    requested: &KnowledgeSnapshotManifest,
    disposition: SnapshotDisposition,
) -> Result<Option<PreparedSnapshot>, SnapshotError> {
    if requested.profile != SnapshotProfile::SelectedTrees
        || requested
            .semantic
            .as_ref()
            .is_none_or(|semantic| semantic.ingestion.is_some())
    {
        return Ok(None);
    }
    let snapshots = layout.root().as_path().join("snapshots");
    let mut paths = match fs::read_dir(snapshots) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    paths.sort_unstable();
    for path in paths {
        let Ok(candidate) = read_and_validate_manifest(&path) else {
            continue;
        };
        if candidate.id == requested.id
            || candidate.schema != requested.schema
            || candidate.profile != requested.profile
            || candidate.repositories != requested.repositories
            || candidate.configured_repositories != requested.configured_repositories
            || candidate.component_versions != requested.component_versions
            || !semantic_serving_contract_matches(
                candidate.semantic.as_ref(),
                requested.semantic.as_ref(),
            )
        {
            continue;
        }
        let artifact_path = layout.artifact(&candidate.id);
        if validate_serving_compatible_artifact(layout, repositories, &artifact_path, &candidate)
            .is_ok()
        {
            pin_snapshot_commits(layout, &candidate)?;
            return Ok(Some(PreparedSnapshot {
                manifest: candidate,
                artifact_path,
                disposition,
            }));
        }
    }
    Ok(None)
}

fn load_validated_snapshot(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: KnowledgeSnapshotManifest,
    disposition: SnapshotDisposition,
) -> Result<Option<PreparedSnapshot>, SnapshotError> {
    let artifact_path = layout.artifact(&manifest.id);
    let validation = if manifest.profile == SnapshotProfile::SelectedTrees {
        validate_serving_compatible_artifact(layout, repositories, &artifact_path, &manifest)
    } else {
        validate_snapshot_artifact(&artifact_path, &manifest)
    };
    match validation {
        Ok(()) => {
            pin_snapshot_commits(layout, &manifest)?;
            Ok(Some(PreparedSnapshot {
                manifest,
                artifact_path,
                disposition,
            }))
        }
        Err(error) => {
            tracing::warn!(%error, "ignoring incomplete managed snapshot artifact");
            Ok(None)
        }
    }
}

fn pin_snapshot_commits(
    layout: &KnowledgeDataLayout,
    manifest: &KnowledgeSnapshotManifest,
) -> Result<(), SnapshotError> {
    for selected in &manifest.repositories {
        let repository = gix::open(layout.repository(&selected.repository)).map_err(|error| {
            SnapshotError::Build(format!("open managed Git repository: {error}"))
        })?;
        let tips = if selected.history_tips.is_empty() {
            std::slice::from_ref(&selected.commit)
        } else {
            selected.history_tips.as_slice()
        };
        for tip in tips {
            let commit = gix::ObjectId::from_hex(tip.as_bytes())
                .map_err(|error| SnapshotError::Build(format!("parse snapshot commit: {error}")))?;
            repository
                .find_commit(commit)
                .map_err(|error| SnapshotError::Build(format!("read snapshot commit: {error}")))?;
            let reference = format!("refs/vesc-mcp/snapshots/{}/{}", manifest.id.as_str(), tip);
            repository
                .reference(
                    reference.as_str(),
                    commit,
                    gix::refs::transaction::PreviousValue::Any,
                    "vesc-mcp snapshot pin",
                )
                .map_err(|error| SnapshotError::Build(format!("pin snapshot commit: {error}")))?;
        }
    }
    Ok(())
}

fn snapshot_build_lock_path(layout: &KnowledgeDataLayout) -> PathBuf {
    layout
        .root()
        .as_path()
        .join("snapshots")
        .join(".build.lock")
}

fn acquire_snapshot_build_lock(
    layout: &KnowledgeDataLayout,
) -> Result<std::fs::File, SnapshotError> {
    let lock = open_snapshot_build_lock(layout)?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn open_snapshot_build_lock(layout: &KnowledgeDataLayout) -> Result<std::fs::File, SnapshotError> {
    let path = snapshot_build_lock_path(layout);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    Ok(lock)
}

fn cleanup_abandoned_artifact_staging_if_idle(layout: &KnowledgeDataLayout) {
    let Ok(lock) = open_snapshot_build_lock(layout) else {
        return;
    };
    if lock.try_lock_exclusive().is_err() {
        return;
    }
    if let Err(error) = remove_abandoned_artifact_staging(layout) {
        tracing::warn!(%error, "failed to remove abandoned artifact staging");
    }
    if let Err(error) = FileExt::unlock(&lock) {
        tracing::warn!(%error, "failed to unlock snapshot build cleanup");
    }
}

fn remove_abandoned_artifact_staging(layout: &KnowledgeDataLayout) -> Result<(), SnapshotError> {
    let artifacts = layout.root().as_path().join("artifacts");
    let entries = match fs::read_dir(&artifacts) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for artifact in entries {
        let artifact = artifact?;
        if !artifact.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(artifact.path())? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry.file_name().to_string_lossy().starts_with(".tmp-")
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keep discovery and deletion in one cleanup transaction.
fn prune_obsolete_incomplete_snapshots(
    layout: &KnowledgeDataLayout,
    current: &KnowledgeSnapshotId,
) -> Result<(), SnapshotError> {
    const PIN_PREFIX: &str = "refs/vesc-mcp/snapshots/";

    let mut candidates = BTreeSet::new();
    let artifacts = layout.root().as_path().join("artifacts");
    match fs::read_dir(&artifacts) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && let Some(id) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| KnowledgeSnapshotId::new(name).ok())
                {
                    candidates.insert(id);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let checkpoints = layout.root().as_path().join("vector-checkpoints");
    match fs::read_dir(&checkpoints) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_file()
                    && entry.path().extension().is_some_and(|ext| ext == "bin")
                    && let Some(id) = entry
                        .path()
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .and_then(|name| KnowledgeSnapshotId::new(name).ok())
                {
                    candidates.insert(id);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut repositories = Vec::new();
    let repositories_root = layout.root().as_path().join("repositories");
    match fs::read_dir(repositories_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let repository = gix::open(entry.path()).map_err(|error| {
                    SnapshotError::Build(format!(
                        "open managed Git repository for cleanup: {error}"
                    ))
                })?;
                let references = repository.references().map_err(|error| {
                    SnapshotError::Build(format!(
                        "read managed Git references for cleanup: {error}"
                    ))
                })?;
                for reference in references.prefixed(PIN_PREFIX).map_err(|error| {
                    SnapshotError::Build(format!("list snapshot pins for cleanup: {error}"))
                })? {
                    let reference = reference.map_err(|error| {
                        SnapshotError::Build(format!("read snapshot pin for cleanup: {error}"))
                    })?;
                    let name = reference.name().as_bstr().to_string();
                    if let Some(id) = name
                        .strip_prefix(PIN_PREFIX)
                        .and_then(|name| name.split('/').next())
                        .and_then(|name| KnowledgeSnapshotId::new(name).ok())
                    {
                        candidates.insert(id);
                    }
                }
                repositories.push(repository);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    for id in candidates.iter().filter(|id| layout.snapshot(id).is_file()) {
        remove_if_present(&vector_checkpoint_path(layout, id))?;
    }
    candidates.retain(|id| id != current && !layout.snapshot(id).is_file());
    for id in candidates {
        for repository in &repositories {
            let prefix = format!("{PIN_PREFIX}{}/", id.as_str());
            let references = repository.references().map_err(|error| {
                SnapshotError::Build(format!("read snapshot pins for cleanup: {error}"))
            })?;
            let mut names = references
                .prefixed(prefix.as_str())
                .map_err(|error| {
                    SnapshotError::Build(format!("list obsolete snapshot pins: {error}"))
                })?
                .map(|reference| {
                    reference
                        .map(|reference| reference.name().as_bstr().to_string())
                        .map_err(|error| {
                            SnapshotError::Build(format!("read obsolete snapshot pin: {error}"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let legacy = format!("{PIN_PREFIX}{}", id.as_str());
            if repository
                .try_find_reference(legacy.as_str())
                .map_err(|error| {
                    SnapshotError::Build(format!("find legacy snapshot pin: {error}"))
                })?
                .is_some()
            {
                names.push(legacy);
            }
            for name in names {
                repository
                    .find_reference(name.as_str())
                    .map_err(|error| {
                        SnapshotError::Build(format!("find obsolete snapshot pin: {error}"))
                    })?
                    .delete()
                    .map_err(|error| {
                        SnapshotError::Build(format!("delete obsolete snapshot pin: {error}"))
                    })?;
            }
        }
        remove_if_present(&layout.artifact(&id))?;
        remove_if_present(&vector_checkpoint_path(layout, &id))?;
        remove_if_present(
            &layout
                .root()
                .as_path()
                .join("snapshots")
                .join(format!("{}.lock", id.as_str())),
        )?;
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), SnapshotError> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn report_progress(progress: Option<&SnapshotProgressReporter>, phase: SnapshotBuildPhase) {
    if let Some(progress) = progress {
        progress(phase);
    }
}

#[derive(Clone, Copy)]
struct SnapshotArtifactBuildOptions<'a> {
    semantic: Option<&'a SnapshotSemanticConfig>,
    policy: SnapshotBuildPolicy,
    progress: Option<&'a SnapshotProgressReporter>,
}

fn build_snapshot_artifact(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
    artifact_path: &Path,
    sources: &[GitCorpusSource],
    options: SnapshotArtifactBuildOptions<'_>,
) -> Result<SnapshotArtifactBuild, SnapshotError> {
    report_progress(options.progress, SnapshotBuildPhase::PlanningHistory);
    let vector_checkpoint_path = options
        .semantic
        .filter(|_| options.policy == SnapshotBuildPolicy::AllowInference)
        .map(|_| vector_checkpoint_path(layout, &manifest.id));
    let summary = match manifest.profile {
        SnapshotProfile::SelectedTrees => build_selected_tree_artifact(
            layout,
            repositories,
            manifest,
            artifact_path,
            sources,
            options,
            vector_checkpoint_path.as_deref(),
        )?,
        SnapshotProfile::CompleteHistory => build_complete_history_artifact(
            layout,
            repositories,
            manifest,
            artifact_path,
            sources,
            options,
            vector_checkpoint_path.as_deref(),
        )?,
    };
    Ok(SnapshotArtifactBuild {
        vector_checkpoint_path,
        has_vectors: summary.manifest.vector_checksum.is_some(),
    })
}

fn build_selected_tree_artifact(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
    artifact_path: &Path,
    sources: &[GitCorpusSource],
    options: SnapshotArtifactBuildOptions<'_>,
    vector_checkpoint_path: Option<&Path>,
) -> Result<vesc_knowledge_index::BuildSummary, SnapshotError> {
    let previous_vectors = options
        .semantic
        .and_then(|_| load_previous_vector_artifact(layout, repositories, manifest));
    if options.policy == SnapshotBuildPolicy::CachedVectorsOnly
        && (options.semantic.is_none() || previous_vectors.is_none())
    {
        return Err(SnapshotError::NotCached(manifest.id.clone()));
    }
    let mut provider =
        options
            .semantic
            .map(|semantic| match options.policy {
                SnapshotBuildPolicy::AllowInference => semantic_provider(semantic),
                SnapshotBuildPolicy::CachedVectorsOnly => Ok(Box::new(CachedVectorProvider)
                    as Box<dyn vesc_knowledge_index::EmbeddingProvider>),
            })
            .transpose()?;
    let semantic_build = provider
        .as_mut()
        .zip(options.semantic)
        .map(|(provider, semantic)| {
            (
                provider.as_mut() as &mut dyn vesc_knowledge_index::EmbeddingProvider,
                semantic.model.model_id.as_str(),
                semantic.model.model_revision.as_str(),
            )
        });
    report_progress(options.progress, SnapshotBuildPhase::BuildingLexicalIndex);
    let summary = vesc_knowledge_index::build_git_artifacts_with_provider(
        artifact_path,
        sources,
        semantic_build,
        previous_vectors.as_ref(),
        vector_checkpoint_path,
    )
    .map_err(|error| {
        if options.policy == SnapshotBuildPolicy::CachedVectorsOnly
            && matches!(
                error,
                vesc_knowledge_index::LifecycleError::Vector(
                    vesc_knowledge_index::EmbeddingError::InferenceDisabled
                )
            )
        {
            SnapshotError::NotCached(manifest.id.clone())
        } else {
            SnapshotError::Build(error.to_string())
        }
    })?;
    report_progress(options.progress, SnapshotBuildPhase::Publishing);
    Ok(summary)
}

fn build_complete_history_artifact(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
    artifact_path: &Path,
    sources: &[GitCorpusSource],
    options: SnapshotArtifactBuildOptions<'_>,
    vector_checkpoint_path: Option<&Path>,
) -> Result<vesc_knowledge_index::BuildSummary, SnapshotError> {
    if options.policy == SnapshotBuildPolicy::CachedVectorsOnly {
        return Err(SnapshotError::NotCached(manifest.id.clone()));
    }
    let mut provider = options.semantic.map(semantic_provider).transpose()?;
    let previous = load_previous_snapshot(layout, repositories, manifest);
    let semantic_build = provider
        .as_mut()
        .zip(options.semantic)
        .map(|(provider, semantic)| {
            (
                provider.as_mut() as &mut dyn vesc_knowledge_index::EmbeddingProvider,
                semantic.model.model_id.as_str(),
                semantic.model.model_revision.as_str(),
            )
        });
    let mut lifecycle_progress = |phase| {
        let phase = match phase {
            vesc_knowledge_index::BuildPhase::Lexical => {
                Some(SnapshotBuildPhase::BuildingLexicalIndex)
            }
            vesc_knowledge_index::BuildPhase::Inference => {
                Some(SnapshotBuildPhase::BuildingSemanticIndex)
            }
            vesc_knowledge_index::BuildPhase::Activation => Some(SnapshotBuildPhase::Publishing),
            _ => None,
        };
        if let Some(phase) = phase {
            report_progress(options.progress, phase);
        }
    };
    let summary = vesc_knowledge_index::build_git_history_artifacts_from_previous(
        artifact_path,
        sources,
        previous.map(
            |previous| vesc_knowledge_index::PreviousGitHistoryArtifact {
                tips: previous.tips,
                lexical_path: previous.lexical_path,
                corpus_digest: previous.artifact.corpus_digest,
                vector_checksum: previous.vector_checksum,
                vector_path: previous.vector_path,
                lexical_format_compatible: previous.lexical_format_compatible,
            },
        ),
        semantic_build,
        vector_checkpoint_path,
        &mut lifecycle_progress,
    )
    .map_err(|error| SnapshotError::Build(error.to_string()))?;
    tracing::info!(
        reused_snapshot = summary.reused_snapshot,
        reused_commits = summary.refresh.reused_commits,
        ingested_commits = summary.refresh.ingested_commits,
        reused_blobs = summary.refresh.reused_blobs,
        reused_contents = summary.refresh.reused_contents,
        candidate_chunks = summary.refresh.candidate_chunks,
        materialized_chunks = summary.refresh.materialized_chunks,
        candidate_identifier_count_histogram = ?summary.refresh.candidate_identifier_count_histogram,
        materialized_identifier_count_histogram = ?summary.refresh.materialized_identifier_count_histogram,
        "prepared managed Git history snapshot"
    );
    if let Some(vectors) = summary.artifacts.observations.vector_build {
        tracing::info!(
            reused_vectors = vectors.reused_vectors,
            embedded_vectors = vectors.embedded_vectors,
            "prepared managed semantic snapshot"
        );
    }
    Ok(summary.artifacts)
}

struct SnapshotArtifactBuild {
    vector_checkpoint_path: Option<PathBuf>,
    has_vectors: bool,
}

struct CachedVectorProvider;

impl vesc_knowledge_index::EmbeddingProvider for CachedVectorProvider {
    fn document_inference_enabled(&self) -> bool {
        false
    }

    fn embed_documents(
        &mut self,
        _texts: &[String],
    ) -> Result<Vec<Vec<f32>>, vesc_knowledge_index::EmbeddingError> {
        Err(vesc_knowledge_index::EmbeddingError::InferenceDisabled)
    }

    fn embed_query(
        &mut self,
        _text: &str,
    ) -> Result<Vec<f32>, vesc_knowledge_index::EmbeddingError> {
        Err(vesc_knowledge_index::EmbeddingError::InferenceDisabled)
    }
}

fn vector_checkpoint_path(layout: &KnowledgeDataLayout, id: &KnowledgeSnapshotId) -> PathBuf {
    layout
        .root()
        .as_path()
        .join("vector-checkpoints")
        .join(format!("{}.bin", id.as_str()))
}

struct PreviousSnapshotArtifacts {
    tips: Vec<vesc_knowledge_index::GitHistoryTip>,
    lexical_path: PathBuf,
    artifact: vesc_knowledge_index::PreviousArtifactSummary,
    vector_path: Option<PathBuf>,
    vector_checksum: Option<vesc_knowledge_index::ContentDigest>,
    lexical_format_compatible: bool,
}

fn load_previous_vector_artifact(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    current: &KnowledgeSnapshotManifest,
) -> Option<vesc_knowledge_index::PreviousVectorArtifact> {
    let legacy_policy_digests = legacy_policy_digests(repositories)?;
    let snapshots = fs::read_dir(layout.root().as_path().join("snapshots")).ok()?;
    let default = crate::read_default_snapshot(layout.root().as_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<KnowledgeSnapshotManifest>(&bytes).ok())
        .filter(KnowledgeSnapshotManifest::has_valid_identity)
        .map(|manifest| manifest.id);
    let mut candidates = snapshots
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| read_and_validate_manifest(&entry.path()).ok())
        .filter(|previous| {
            previous_vector_snapshot_is_compatible_with_legacy(
                previous,
                current,
                &legacy_policy_digests,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| {
        let left_is_default = default.as_ref().is_some_and(|id| id == &left.id);
        let right_is_default = default.as_ref().is_some_and(|id| id == &right.id);
        right_is_default
            .cmp(&left_is_default)
            .then_with(|| {
                previous_snapshot_score(right, current).cmp(&previous_snapshot_score(left, current))
            })
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.into_iter().find_map(|previous| {
        load_previous_vector_snapshot_candidate(layout, current, &previous, &legacy_policy_digests)
    })
}

#[cfg(test)]
fn previous_vector_snapshot_is_compatible(
    previous: &KnowledgeSnapshotManifest,
    current: &KnowledgeSnapshotManifest,
) -> bool {
    previous_vector_snapshot_is_compatible_with_legacy(previous, current, &HashMap::new())
}

fn previous_vector_snapshot_is_compatible_with_legacy(
    previous: &KnowledgeSnapshotManifest,
    current: &KnowledgeSnapshotManifest,
    legacy_policy_digests: &HashMap<RepositoryId, String>,
) -> bool {
    previous.profile == SnapshotProfile::CompleteHistory
        && current.profile == SnapshotProfile::SelectedTrees
        && component_versions_are_incrementally_compatible(
            &previous.component_versions,
            &current.component_versions,
        )
        && previous.component_versions.get("lexical-format")
            == current.component_versions.get("lexical-format")
        && semantic_serving_contract_matches(previous.semantic.as_ref(), current.semantic.as_ref())
        && current.repositories.iter().all(|selected| {
            previous.repositories.iter().any(|candidate| {
                previous_repository_policy_is_compatible(
                    previous.schema,
                    candidate,
                    selected,
                    legacy_policy_digests,
                )
            })
        })
}

fn load_previous_vector_snapshot_candidate(
    layout: &KnowledgeDataLayout,
    current: &KnowledgeSnapshotManifest,
    previous: &KnowledgeSnapshotManifest,
    legacy_policy_digests: &HashMap<RepositoryId, String>,
) -> Option<vesc_knowledge_index::PreviousVectorArtifact> {
    if !previous.has_valid_identity()
        || !previous_vector_snapshot_is_compatible_with_legacy(
            previous,
            current,
            legacy_policy_digests,
        )
    {
        return None;
    }
    let artifact_root = layout.artifact(&previous.id);
    let artifact = vesc_knowledge_index::inspect_previous_artifact(
        &vesc_knowledge_index::active_manifest_path(&artifact_root),
    )
    .ok()?;
    if artifact.component_versions != previous.component_versions {
        return None;
    }
    let lexical_path = artifact_root
        .join("generations")
        .join(artifact.generation.to_string())
        .join("lexical.json");
    if !matches!(
        vesc_knowledge_index::LexicalIndex::corpus_inventory(&lexical_path),
        Ok((_documents, _chunks, digest)) if digest == artifact.corpus_digest
    ) {
        return None;
    }
    let semantic = current.semantic.as_ref()?;
    let checksum = artifact.vector_checksum?;
    let path = lexical_path.with_file_name("vectors.bin");
    vesc_knowledge_index::PreviousVectorArtifact::open(
        lexical_path,
        artifact.corpus_digest,
        checksum,
        path,
        &semantic.model_id,
        &semantic.model_revision,
        None,
    )
    .ok()
}

fn load_previous_snapshot(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    current: &KnowledgeSnapshotManifest,
) -> Option<PreviousSnapshotArtifacts> {
    let legacy_policy_digests = legacy_policy_digests(repositories)?;
    let snapshots = fs::read_dir(layout.root().as_path().join("snapshots")).ok()?;
    let candidates = snapshots
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|entry| read_and_validate_manifest(&entry.path()).ok())
        .filter(|previous| {
            previous_snapshot_is_incrementally_compatible_with_legacy(
                previous,
                current,
                &legacy_policy_digests,
            )
        })
        .collect::<Vec<_>>();
    let distances = reachable_previous_commit_distances(layout, current, &candidates);
    let mut candidates = candidates
        .into_iter()
        .filter_map(|previous| {
            previous_snapshot_distance(&previous, &distances).map(|distance| (previous, distance))
        })
        .collect::<Vec<_>>();
    let default = crate::read_default_snapshot(layout.root().as_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<KnowledgeSnapshotManifest>(&bytes).ok())
        .filter(KnowledgeSnapshotManifest::has_valid_identity)
        .map(|manifest| manifest.id);
    sort_previous_snapshot_candidates(&mut candidates, current, default.as_ref());
    candidates.into_iter().find_map(|(previous, _)| {
        load_previous_snapshot_candidate_with_legacy(
            layout,
            current,
            &previous,
            &legacy_policy_digests,
        )
    })
}

fn legacy_policy_digests(
    repositories: &[KnowledgeRepository],
) -> Option<HashMap<RepositoryId, String>> {
    repositories
        .iter()
        .map(|repository| {
            Some((
                repository.id().clone(),
                legacy_repository_policy_digest(repository).ok()?,
            ))
        })
        .collect()
}

fn reachable_previous_commit_distances(
    layout: &KnowledgeDataLayout,
    current: &KnowledgeSnapshotManifest,
    candidates: &[KnowledgeSnapshotManifest],
) -> HashMap<RepositoryId, HashMap<gix::ObjectId, usize>> {
    let mut wanted = BTreeMap::<RepositoryId, BTreeSet<gix::ObjectId>>::new();
    for previous in candidates {
        for selected in &previous.repositories {
            for tip in snapshot_repository_tips(selected) {
                if let Ok(commit) = gix::ObjectId::from_hex(tip.as_bytes()) {
                    wanted
                        .entry(selected.repository.clone())
                        .or_default()
                        .insert(commit);
                }
            }
        }
    }

    let mut distances = HashMap::new();
    for selected in &current.repositories {
        let Some(mut wanted) = wanted.remove(&selected.repository) else {
            continue;
        };
        let Ok(repository) = gix::open(layout.repository(&selected.repository)) else {
            continue;
        };
        let tips = snapshot_repository_tips(selected)
            .iter()
            .filter_map(|tip| gix::ObjectId::from_hex(tip.as_bytes()).ok())
            .collect::<Vec<_>>();
        let Ok(walk) = repository.rev_walk(tips).all() else {
            continue;
        };
        let mut found = HashMap::with_capacity(wanted.len());
        for (distance, info) in walk.enumerate() {
            let Ok(info) = info else {
                found.clear();
                break;
            };
            if wanted.remove(&info.id) {
                found.insert(info.id, distance);
                if wanted.is_empty() {
                    break;
                }
            }
        }
        distances.insert(selected.repository.clone(), found);
    }
    distances
}

fn previous_snapshot_distance(
    previous: &KnowledgeSnapshotManifest,
    distances: &HashMap<RepositoryId, HashMap<gix::ObjectId, usize>>,
) -> Option<usize> {
    previous
        .repositories
        .iter()
        .try_fold(0_usize, |total, selected| {
            let repository_distances = distances.get(&selected.repository)?;
            snapshot_repository_tips(selected)
                .iter()
                .map(|tip| {
                    let commit = gix::ObjectId::from_hex(tip.as_bytes()).ok()?;
                    repository_distances.get(&commit).copied()
                })
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .max()
                .map(|distance| total.saturating_add(distance))
        })
}

fn snapshot_repository_tips(repository: &SnapshotRepository) -> &[String] {
    if repository.history_tips.is_empty() {
        std::slice::from_ref(&repository.commit)
    } else {
        &repository.history_tips
    }
}

fn sort_previous_snapshot_candidates(
    candidates: &mut [(KnowledgeSnapshotManifest, usize)],
    current: &KnowledgeSnapshotManifest,
    default: Option<&KnowledgeSnapshotId>,
) {
    candidates.sort_unstable_by(|(left, left_distance), (right, right_distance)| {
        let left_is_default = default.is_some_and(|id| id == &left.id);
        let right_is_default = default.is_some_and(|id| id == &right.id);
        right_is_default
            .cmp(&left_is_default)
            .then_with(|| {
                previous_snapshot_score(right, current).cmp(&previous_snapshot_score(left, current))
            })
            .then_with(|| left_distance.cmp(right_distance))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn previous_snapshot_score(
    previous: &KnowledgeSnapshotManifest,
    current: &KnowledgeSnapshotManifest,
) -> (usize, usize, bool) {
    (
        previous
            .repositories
            .iter()
            .map(|repository| {
                let Some(candidate) = current
                    .repositories
                    .iter()
                    .find(|candidate| candidate.repository == repository.repository)
                else {
                    return 0;
                };
                let current_tips = snapshot_repository_tips(candidate);
                snapshot_repository_tips(repository)
                    .iter()
                    .filter(|tip| current_tips.contains(tip))
                    .count()
            })
            .sum(),
        previous
            .repositories
            .iter()
            .map(|repository| snapshot_repository_tips(repository).len())
            .sum(),
        previous.component_versions.get("lexical-format")
            == current.component_versions.get("lexical-format"),
    )
}

#[cfg(test)]
fn load_previous_snapshot_candidate(
    layout: &KnowledgeDataLayout,
    current: &KnowledgeSnapshotManifest,
    previous: &KnowledgeSnapshotManifest,
) -> Option<PreviousSnapshotArtifacts> {
    load_previous_snapshot_candidate_with_legacy(layout, current, previous, &HashMap::new())
}

fn load_previous_snapshot_candidate_with_legacy(
    layout: &KnowledgeDataLayout,
    current: &KnowledgeSnapshotManifest,
    previous: &KnowledgeSnapshotManifest,
    legacy_policy_digests: &HashMap<RepositoryId, String>,
) -> Option<PreviousSnapshotArtifacts> {
    if !previous.has_valid_identity() {
        return None;
    }
    if !previous_snapshot_is_incrementally_compatible_with_legacy(
        previous,
        current,
        legacy_policy_digests,
    ) {
        return None;
    }

    let artifact_root = layout.artifact(&previous.id);
    let artifact = vesc_knowledge_index::inspect_previous_artifact(
        &vesc_knowledge_index::active_manifest_path(&artifact_root),
    )
    .ok()?;
    if artifact.component_versions != previous.component_versions {
        return None;
    }
    let lexical = artifact_root
        .join("generations")
        .join(artifact.generation.to_string())
        .join("lexical.json");
    let lexical_format_compatible = previous.component_versions.get("lexical-format")
        == current.component_versions.get("lexical-format");
    if lexical_format_compatible
        && !matches!(
            vesc_knowledge_index::LexicalIndex::corpus_inventory(&lexical),
            Ok((_documents, _chunks, digest)) if digest == artifact.corpus_digest
        )
    {
        return None;
    }
    let (vector_path, vector_checksum) = match (&previous.semantic, &current.semantic) {
        (Some(previous), Some(current)) if previous == current => {
            let path = lexical.with_file_name("vectors.bin");
            artifact
                .vector_checksum
                .as_ref()
                .filter(|checksum| {
                    vesc_knowledge_index::VectorArtifact::validate_reusable_artifact(
                        &path,
                        checksum,
                        &artifact.corpus_digest,
                        &current.model_id,
                        &current.model_revision,
                        None,
                    )
                    .is_ok()
                })
                .map_or((None, None), |checksum| {
                    (Some(path), Some(checksum.clone()))
                })
        }
        _ => (None, None),
    };
    let tips = previous
        .repositories
        .iter()
        .filter(|repository| {
            current.repositories.iter().any(|candidate| {
                previous_repository_policy_is_compatible(
                    previous.schema,
                    repository,
                    candidate,
                    legacy_policy_digests,
                )
            })
        })
        .flat_map(|repository| {
            let tips = if repository.history_tips.is_empty() {
                std::slice::from_ref(&repository.commit)
            } else {
                repository.history_tips.as_slice()
            };
            tips.iter().map(move |revision| {
                Some(vesc_knowledge_index::GitHistoryTip {
                    repository: CorpusRepositoryId::try_from(repository.repository.as_str())
                        .ok()?,
                    revision: Revision::try_from(revision.clone()).ok()?,
                })
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(PreviousSnapshotArtifacts {
        tips,
        lexical_path: lexical,
        artifact,
        vector_path,
        vector_checksum,
        lexical_format_compatible,
    })
}

#[cfg(test)]
fn previous_snapshot_is_incrementally_compatible(
    previous: &KnowledgeSnapshotManifest,
    current: &KnowledgeSnapshotManifest,
) -> bool {
    previous_snapshot_is_incrementally_compatible_with_legacy(previous, current, &HashMap::new())
}

fn previous_snapshot_is_incrementally_compatible_with_legacy(
    previous: &KnowledgeSnapshotManifest,
    current: &KnowledgeSnapshotManifest,
    legacy_policy_digests: &HashMap<RepositoryId, String>,
) -> bool {
    previous.profile == SnapshotProfile::CompleteHistory
        && component_versions_are_incrementally_compatible(
            &previous.component_versions,
            &current.component_versions,
        )
        && previous.repositories.iter().all(|repository| {
            current.repositories.iter().any(|candidate| {
                previous_repository_policy_is_compatible(
                    previous.schema,
                    repository,
                    candidate,
                    legacy_policy_digests,
                )
            })
        })
}

fn previous_repository_policy_is_compatible(
    previous_schema: u16,
    previous: &SnapshotRepository,
    current: &SnapshotRepository,
    legacy_policy_digests: &HashMap<RepositoryId, String>,
) -> bool {
    previous.repository == current.repository
        && (previous.policy_digest == current.policy_digest
            || previous_schema == LEGACY_SNAPSHOT_SCHEMA
                && legacy_policy_digests.get(&previous.repository) == Some(&previous.policy_digest))
}

fn component_versions_are_incrementally_compatible(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> bool {
    vesc_knowledge_index::git_history_corpus_versions_are_compatible(previous, current)
}

fn load_prepared(
    layout: &KnowledgeDataLayout,
    id: &KnowledgeSnapshotId,
    disposition: SnapshotDisposition,
) -> Result<PreparedSnapshot, SnapshotError> {
    let path = layout.snapshot(id);
    let manifest = read_and_validate_manifest(&path)?;
    let artifact_path = layout.artifact(id);
    validate_snapshot_artifact(&artifact_path, &manifest)?;
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
    history_tips: &[String],
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
        limits: GitCorpusLimits::new(
            repository.max_file_bytes(),
            repository.max_files(),
            repository.max_total_bytes(),
        )
        .map_err(|error| SnapshotError::Build(error.to_string()))?,
        ..GitCorpusPolicy::default()
    };
    Ok(GitCorpusSource {
        repository_path: layout.repository(repository.id()),
        repository_id,
        revision,
        history_tips: history_tips
            .iter()
            .map(|tip| {
                Revision::try_from(tip.clone())
                    .map_err(|error| SnapshotError::Build(error.to_string()))
            })
            .collect::<Result<_, _>>()?,
        trust_tier,
        license: LicenseStatus::Redistributable {
            spdx: repository.license().to_owned(),
        },
        policy,
    })
}

fn validate_snapshot_artifact(
    path: &Path,
    snapshot: &KnowledgeSnapshotManifest,
) -> Result<(), SnapshotError> {
    validated_snapshot_artifact(path, snapshot).map(|_| ())
}

fn validated_snapshot_artifact(
    path: &Path,
    snapshot: &KnowledgeSnapshotManifest,
) -> Result<vesc_knowledge_index::PreviousArtifactSummary, SnapshotError> {
    let vector_before = crate::preparation_status::ValidatedVectorArtifact::current_identity(path);
    let artifact = vesc_knowledge_index::validate_active_generation(path)
        .map_err(|error| SnapshotError::Build(error.to_string()))?;
    if snapshot.semantic.is_some() && artifact.vector_checksum.is_none() {
        return Err(SnapshotError::Build(
            "semantic snapshot vector artifact is unavailable".into(),
        ));
    }
    let vector_after = crate::preparation_status::ValidatedVectorArtifact::current_identity(path);
    if vector_before != vector_after {
        return Err(SnapshotError::Build(
            "semantic snapshot vector artifact changed during validation".into(),
        ));
    }
    if let Some(identity) = vector_after {
        crate::preparation_status::record_validated_vector(path, identity);
    }
    Ok(artifact)
}

fn validate_serving_compatible_artifact(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    path: &Path,
    snapshot: &KnowledgeSnapshotManifest,
) -> Result<(), SnapshotError> {
    let artifact = validated_snapshot_artifact(path, snapshot)?;
    if artifact.component_versions != snapshot.component_versions {
        return Err(SnapshotError::Build(
            "snapshot artifact component versions do not match its manifest".into(),
        ));
    }
    let generation = path
        .join("generations")
        .join(artifact.generation.to_string());
    let sources = snapshot_corpus_sources(layout, repositories, snapshot)?;
    vesc_knowledge_index::LexicalIndex::validate_git_source_contracts(
        &generation.join("lexical.json"),
        &sources,
    )
    .map_err(|error| SnapshotError::Build(error.to_string()))?;
    if let Some(semantic) = &snapshot.semantic {
        let vectors = vesc_knowledge_index::FileBackedVectorArtifact::open_search_artifact(
            &generation.join("vectors.bin"),
        )
        .map_err(|error| SnapshotError::Build(error.to_string()))?;
        if vectors.model_id != semantic.model_id
            || vectors.model_revision != semantic.model_revision
        {
            return Err(SnapshotError::Build(
                "snapshot vector model does not match its manifest".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "semantic-fastembed")]
struct DeferredSemanticProvider {
    model_dir: PathBuf,
    profile: vesc_knowledge_index::EmbeddingProfile,
    batch_size: vesc_knowledge_index::EmbeddingBatchSize,
    execution_provider: vesc_knowledge_index::SemanticExecutionProvider,
    length_bucketed: bool,
    window_aggregation: Option<vesc_knowledge_index::WindowAggregation>,
    provider: Option<vesc_knowledge_index::FastEmbedProvider>,
}

#[cfg(feature = "semantic-fastembed")]
impl DeferredSemanticProvider {
    fn new(semantic: &SnapshotSemanticConfig) -> Result<Self, SnapshotError> {
        let mut profile =
            vesc_knowledge_index::EmbeddingProfile::for_model_id(&semantic.model.model_id)
                .ok_or_else(|| {
                    SnapshotError::Build(format!(
                        "no embedding profile is registered for {}",
                        semantic.model.model_id
                    ))
                })?;
        let ingestion = semantic.model.ingestion.as_ref();
        profile.max_length =
            ingestion.map_or(semantic.model.max_length, |config| config.max_length);
        let batch_size = vesc_knowledge_index::EmbeddingBatchSize::new(ingestion.map_or(
            vesc_knowledge_index::DEFAULT_SEMANTIC_BATCH_SIZE,
            |config| config.batch_size,
        ))
        .map_err(|error| SnapshotError::Build(error.to_string()))?;
        let execution_provider = ingestion.map_or(
            vesc_knowledge_index::SemanticExecutionProvider::Auto,
            |config| match config.provider {
                SemanticIngestionProvider::Cpu => {
                    vesc_knowledge_index::SemanticExecutionProvider::Cpu
                }
                SemanticIngestionProvider::Migraphx => {
                    vesc_knowledge_index::SemanticExecutionProvider::Migraphx {
                        device_id: config.device_id,
                    }
                }
            },
        );
        Ok(Self {
            model_dir: semantic.model_dir.clone(),
            profile,
            batch_size,
            execution_provider,
            length_bucketed: ingestion.is_some(),
            window_aggregation: ingestion.map(|config| config.window_aggregation),
            provider: None,
        })
    }

    fn provider(
        &mut self,
    ) -> Result<&mut vesc_knowledge_index::FastEmbedProvider, vesc_knowledge_index::EmbeddingError>
    {
        if self.provider.is_none() {
            let mut provider = vesc_knowledge_index::FastEmbedProvider::
                from_model_dir_with_profile_and_threads_and_provider(
                    &self.model_dir,
                    Some(self.batch_size.get()),
                    self.profile.clone(),
                    Some(vesc_knowledge_index::default_semantic_intra_threads()),
                    self.execution_provider,
                )
                .map_err(|error| {
                    vesc_knowledge_index::EmbeddingError::Provider(format!(
                        "semantic provider unavailable: {error}"
                    ))
                })?;
            provider.set_length_bucketed(self.length_bucketed);
            provider.set_lossless_windowing(true);
            if let Some(aggregation) = self.window_aggregation {
                provider.set_window_aggregation(aggregation);
            }
            self.provider = Some(provider);
        }
        Ok(self.provider.as_mut().expect("provider initialized above"))
    }
}

#[cfg(feature = "semantic-fastembed")]
impl vesc_knowledge_index::EmbeddingProvider for DeferredSemanticProvider {
    fn embedding_dimension(&self) -> Option<usize> {
        Some(self.profile.dimension)
    }

    fn embedding_batch_size(&self) -> vesc_knowledge_index::EmbeddingBatchSize {
        self.batch_size
    }

    fn output_normalization(&self) -> vesc_knowledge_index::OutputNormalization {
        if self.profile.normalize {
            vesc_knowledge_index::OutputNormalization::Guaranteed
        } else {
            vesc_knowledge_index::OutputNormalization::Unknown
        }
    }

    fn inference_order(
        &mut self,
        chunks: &[&vesc_knowledge_index::Chunk],
    ) -> Result<Option<Vec<usize>>, vesc_knowledge_index::EmbeddingError> {
        if chunks.is_empty() {
            return Ok(None);
        }
        self.provider()?.inference_order(chunks)
    }

    fn embed_documents(
        &mut self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, vesc_knowledge_index::EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.provider()?.embed_documents(texts)
    }

    fn embed_query(
        &mut self,
        text: &str,
    ) -> Result<Vec<f32>, vesc_knowledge_index::EmbeddingError> {
        self.provider()?.embed_query(text)
    }
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_provider(
    semantic: &SnapshotSemanticConfig,
) -> Result<Box<dyn vesc_knowledge_index::EmbeddingProvider>, SnapshotError> {
    DeferredSemanticProvider::new(semantic)
        .map(|provider| Box::new(provider) as Box<dyn vesc_knowledge_index::EmbeddingProvider>)
}

#[cfg(not(feature = "semantic-fastembed"))]
fn semantic_provider(
    semantic: &SnapshotSemanticConfig,
) -> Result<Box<dyn vesc_knowledge_index::EmbeddingProvider>, SnapshotError> {
    let _ = &semantic.model_dir;
    Err(SnapshotError::Build(
        "semantic-fastembed feature is disabled".into(),
    ))
}

fn read_and_validate_manifest(path: &Path) -> Result<KnowledgeSnapshotManifest, SnapshotError> {
    let manifest: KnowledgeSnapshotManifest = serde_json::from_slice(&fs::read(path)?)?;
    if !manifest.has_valid_identity() {
        return Err(SnapshotError::IdentityMismatch);
    }
    Ok(manifest)
}

fn repository_policy_digest(repository: &KnowledgeRepository) -> Result<String, SnapshotError> {
    #[derive(Serialize)]
    struct PolicyIdentity<'a> {
        policy: RepositoryPolicy,
        include: BTreeSet<&'a str>,
        exclude: BTreeSet<&'a str>,
        trust_tier: TrustTier,
        license: &'a str,
        max_file_bytes: u64,
        max_files: usize,
        max_total_bytes: u64,
    }

    Ok(hex_sha256(&serde_json::to_vec(&PolicyIdentity {
        policy: repository.policy(),
        include: repository.include().iter().map(String::as_str).collect(),
        exclude: repository.exclude().iter().map(String::as_str).collect(),
        trust_tier: repository.trust_tier(),
        license: repository.license(),
        max_file_bytes: repository.max_file_bytes(),
        max_files: repository.max_files(),
        max_total_bytes: repository.max_total_bytes(),
    })?))
}

fn legacy_repository_policy_digest(
    repository: &KnowledgeRepository,
) -> Result<String, SnapshotError> {
    #[derive(Serialize)]
    struct PolicyIdentity<'a> {
        remote_url: &'a str,
        default_ref: &'a str,
        policy: RepositoryPolicy,
        include: BTreeSet<&'a str>,
        exclude: BTreeSet<&'a str>,
        trust_tier: TrustTier,
        license: &'a str,
        max_file_bytes: u64,
        max_files: usize,
        max_total_bytes: u64,
    }

    Ok(hex_sha256(&serde_json::to_vec(&PolicyIdentity {
        remote_url: repository.remote_url(),
        default_ref: repository.default_ref(),
        policy: repository.policy(),
        include: repository.include().iter().map(String::as_str).collect(),
        exclude: repository.exclude().iter().map(String::as_str).collect(),
        trust_tier: repository.trust_tier(),
        license: repository.license(),
        max_file_bytes: repository.max_file_bytes(),
        max_files: repository.max_files(),
        max_total_bytes: repository.max_total_bytes(),
    })?))
}

fn repository_configuration(
    repositories: &RepositoryRegistry,
) -> Result<Vec<SnapshotRepositoryConfiguration>, SnapshotError> {
    let mut configured = repositories
        .enabled()
        .map(|repository| {
            Ok(SnapshotRepositoryConfiguration {
                repository: repository.id().clone(),
                policy_digest: repository_policy_digest(repository)?,
            })
        })
        .collect::<Result<Vec<_>, SnapshotError>>()?;
    configured.sort();
    Ok(configured)
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

    #[test]
    fn snapshot_build_lock_is_shared_by_all_snapshots() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );

        assert_eq!(
            snapshot_build_lock_path(&layout),
            temp.path().join("data/snapshots/.build.lock")
        );
    }

    #[test]
    fn abandoned_artifact_staging_is_removed_without_touching_generations() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let artifact = layout.root().as_path().join("artifacts/snapshot");
        let abandoned = artifact.join(".tmp-abandoned");
        let generation = artifact.join("generations/current");
        fs::create_dir_all(&abandoned).expect("abandoned staging");
        fs::write(abandoned.join("partial.bin"), b"partial").expect("partial artifact");
        fs::create_dir_all(&generation).expect("published generation");
        fs::write(generation.join("manifest.json"), b"published").expect("published artifact");

        remove_abandoned_artifact_staging(&layout).expect("cleanup staging");

        assert!(!abandoned.exists());
        assert!(generation.join("manifest.json").is_file());
    }

    #[test]
    #[allow(clippy::too_many_lines)] // The cleanup proof covers files and every nested Git pin.
    fn obsolete_incomplete_snapshots_and_pins_are_pruned() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first, second) = fixture_remote(temp.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let repository_id = RepositoryId::new("fixture").expect("repository id");
        fs::create_dir_all(layout.root().as_path().join("repositories"))
            .expect("repositories directory");
        run_git(
            temp.path(),
            &[
                "clone",
                "--bare",
                remote.to_str().expect("UTF-8 remote"),
                layout
                    .repository(&repository_id)
                    .to_str()
                    .expect("UTF-8 managed repository"),
            ],
        );
        let current = KnowledgeSnapshotId::new("current").expect("current id");
        let completed = KnowledgeSnapshotId::new("completed").expect("completed id");
        let obsolete = KnowledgeSnapshotId::new("obsolete").expect("obsolete id");
        for id in [&current, &completed, &obsolete] {
            fs::create_dir_all(layout.artifact(id).join("lexical-stage"))
                .expect("resumable artifact");
            let checkpoint = layout
                .root()
                .as_path()
                .join("vector-checkpoints")
                .join(format!("{}.bin", id.as_str()));
            fs::create_dir_all(checkpoint.parent().expect("checkpoint parent"))
                .expect("checkpoint directory");
            fs::write(checkpoint, b"checkpoint").expect("checkpoint");
        }
        fs::create_dir_all(
            layout
                .snapshot(&completed)
                .parent()
                .expect("snapshot directory"),
        )
        .expect("snapshot directory");
        fs::write(layout.snapshot(&completed), b"completed").expect("completed marker");
        let repository = gix::open(layout.repository(&repository_id)).expect("managed repository");
        let commit = gix::ObjectId::from_hex(second.as_bytes()).expect("fixture commit");
        for id in [&current, &completed, &obsolete] {
            repository
                .reference(
                    format!("refs/vesc-mcp/snapshots/{}/{}", id.as_str(), second),
                    commit,
                    gix::refs::transaction::PreviousValue::Any,
                    "test snapshot pin",
                )
                .expect("snapshot pin");
        }
        repository
            .reference(
                format!("refs/vesc-mcp/snapshots/{}/{}", obsolete.as_str(), first),
                gix::ObjectId::from_hex(first.as_bytes()).expect("first fixture commit"),
                gix::refs::transaction::PreviousValue::Any,
                "second obsolete snapshot pin",
            )
            .expect("second obsolete snapshot pin");

        prune_obsolete_incomplete_snapshots(&layout, &current).expect("prune incomplete snapshots");

        assert!(layout.artifact(&current).is_dir());
        assert!(layout.artifact(&completed).is_dir());
        assert!(!layout.artifact(&obsolete).exists());
        assert!(
            layout
                .root()
                .as_path()
                .join("vector-checkpoints/current.bin")
                .is_file()
        );
        assert!(
            !layout
                .root()
                .as_path()
                .join("vector-checkpoints/completed.bin")
                .exists()
        );
        assert!(
            !layout
                .root()
                .as_path()
                .join("vector-checkpoints/obsolete.bin")
                .exists()
        );
        assert!(
            repository
                .try_find_reference(format!("refs/vesc-mcp/snapshots/current/{second}").as_str(),)
                .expect("find current pin")
                .is_some()
        );
        assert!(
            repository
                .try_find_reference(format!("refs/vesc-mcp/snapshots/completed/{second}").as_str(),)
                .expect("find completed pin")
                .is_some()
        );
        assert!(
            repository
                .try_find_reference(format!("refs/vesc-mcp/snapshots/obsolete/{second}").as_str(),)
                .expect("find obsolete pin")
                .is_none()
        );
        assert!(
            repository
                .try_find_reference(format!("refs/vesc-mcp/snapshots/obsolete/{first}").as_str(),)
                .expect("find second obsolete pin")
                .is_none()
        );
    }

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
        let remote_url = data_root
            .parent()
            .expect("fixture data root has parent")
            .join("remote.git");
        fixture_registry_with_source(
            data_root,
            default_ref,
            include,
            policy,
            remote_url.to_str().expect("UTF-8 fixture remote"),
        )
    }

    fn fixture_registry_with_source(
        data_root: &Path,
        default_ref: &str,
        include: &str,
        policy: &str,
        remote_url: &str,
    ) -> RepositoryRegistry {
        McpConfig::from_toml(
            &format!(
                r#"
[knowledge]
data_root = "{}"

[[knowledge.repositories]]
id = "fixture"
remote_url = "{remote_url}"
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

    #[test]
    fn corpus_source_preserves_configured_working_set_limits() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let repository = repositories.iter().next().expect("fixture repository");

        let source = corpus_source(&layout, repository, &"a".repeat(40), &["a".repeat(40)])
            .expect("configured corpus source");

        assert_eq!(
            source.policy.limits.max_file_bytes(),
            repository.max_file_bytes()
        );
        assert_eq!(source.policy.limits.max_files(), repository.max_files());
        assert_eq!(
            source.policy.limits.max_total_bytes(),
            repository.max_total_bytes()
        );
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

    #[tokio::test]
    async fn complete_history_manifest_records_every_unique_managed_ref_tip() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, first, second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository ID");
        let git = ManagedGitStore::new(layout.clone());
        git.sync_source(
            &id,
            remote.to_str().expect("UTF-8 remote path"),
            "refs/heads/main",
        )
        .await
        .expect("initial managed repository sync");
        let store = KnowledgeSnapshotStore::new(layout);
        let initial_history = store
            .resolve_manifest(
                &repositories,
                &BTreeMap::new(),
                SnapshotProfile::CompleteHistory,
            )
            .expect("initial complete-history manifest");
        let initial_selected = store
            .resolve_manifest(
                &repositories,
                &BTreeMap::new(),
                SnapshotProfile::SelectedTrees,
            )
            .expect("initial selected-tree manifest");

        run_git(&work, &["switch", "-c", "release/7", &first]);
        fs::write(work.join("release.md"), "release only\n").expect("release file");
        run_git(&work, &["add", "release.md"]);
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "release",
            ],
        );
        let release = run_git(&work, &["rev-parse", "HEAD"]);
        run_git(
            &work,
            &[
                "push",
                remote.to_str().expect("UTF-8 remote path"),
                "release/7",
            ],
        );
        git.sync_source(
            &id,
            remote.to_str().expect("UTF-8 remote path"),
            "refs/heads/main",
        )
        .await
        .expect("managed repository refresh");

        let manifest = store
            .resolve_manifest(
                &repositories,
                &BTreeMap::new(),
                SnapshotProfile::CompleteHistory,
            )
            .expect("complete-history manifest");
        let selected_manifest = store
            .resolve_manifest(
                &repositories,
                &BTreeMap::new(),
                SnapshotProfile::SelectedTrees,
            )
            .expect("selected-tree manifest");
        let selected = manifest.repositories.first().expect("selected repository");
        let mut expected = vec![first, second.clone(), release];
        expected.sort();
        expected.dedup();

        assert_eq!(selected.history_tips, expected);
        assert_eq!(selected.commit, second);
        assert_ne!(manifest.id, initial_history.id);
        assert_eq!(selected_manifest.id, initial_selected.id);
    }

    #[tokio::test]
    async fn changed_cached_origin_is_excluded_if_optional_and_fails_if_required() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let id = RepositoryId::new("fixture").expect("repository ID");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");
        let replacement = "https://example.invalid/replacement.git";
        let optional = fixture_registry_with_source(
            &data_root,
            "refs/heads/main",
            "**/*.md",
            "optional",
            replacement,
        );
        let required = fixture_registry_with_source(
            &data_root,
            "refs/heads/main",
            "**/*.md",
            "required",
            replacement,
        );
        let store = KnowledgeSnapshotStore::new(layout);

        let optional_error = store
            .prepare_default(&optional)
            .await
            .expect_err("excluding the only optional repository leaves no selection");
        let error = store
            .prepare_default(&required)
            .await
            .expect_err("required changed origin fails");

        assert!(matches!(optional_error, SnapshotError::EmptySelection));
        assert!(matches!(
            error,
            SnapshotError::ManagedGit(ManagedGitError::RemoteUrlChanged { .. })
        ));
    }

    #[tokio::test]
    async fn prepared_snapshot_pins_commits_across_git_gc() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first, second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let id = RepositoryId::new("fixture").expect("repository ID");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");

        let phases = Arc::new(Mutex::new(Vec::new()));
        let reported_phases = Arc::clone(&phases);
        let prepared = KnowledgeSnapshotStore::new(layout.clone())
            .with_progress_reporter(move |phase| {
                reported_phases.lock().expect("progress mutex").push(phase);
            })
            .prepare_default(&repositories)
            .await
            .expect("prepare snapshot");
        assert_eq!(
            *phases.lock().expect("progress mutex"),
            [
                SnapshotBuildPhase::PlanningHistory,
                SnapshotBuildPhase::BuildingLexicalIndex,
                SnapshotBuildPhase::Publishing,
            ]
        );
        assert!(
            !prepared.artifact_path.join("lexical-stage").exists(),
            "published snapshot must not retain private lexical staging"
        );
        let managed = layout.repository(&id);
        let pin = |tip: &str| {
            format!(
                "refs/vesc-mcp/snapshots/{}/{}",
                prepared.manifest.id.as_str(),
                tip
            )
        };
        assert_eq!(run_git(&managed, &["rev-parse", &pin(&first)]), first);
        assert_eq!(run_git(&managed, &["rev-parse", &pin(&second)]), second);

        run_git(&managed, &["update-ref", "-d", "refs/remotes/origin/main"]);
        run_git(&managed, &["update-ref", "-d", "refs/tags/v1"]);
        run_git(&managed, &["reflog", "expire", "--expire=now", "--all"]);
        run_git(&managed, &["gc", "--prune=now"]);
        assert_eq!(run_git(&managed, &["rev-parse", &pin(&first)]), first);
        assert_eq!(run_git(&managed, &["rev-parse", &pin(&second)]), second);
        run_git(
            &managed,
            &["cat-file", "-e", &format!("{second}^{{commit}}")],
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One fixture proves default reuse, rejection, and selected reuse.
    async fn cpu_runtime_reuses_snapshots_built_with_accelerated_ingestion() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first, second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let repository = repositories.iter().next().expect("fixture repository");
        let repository_id = repository.id().clone();
        ManagedGitStore::new(layout.clone())
            .sync_source(
                &repository_id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");

        let serving_model = SnapshotSemanticModel {
            model_id: "fake".into(),
            model_revision: "test-revision".into(),
            max_length: 512,
            ingestion: None,
        };
        let mut accelerated_model = serving_model.clone();
        accelerated_model.ingestion = Some(SnapshotSemanticIngestion {
            model_sha256: "f".repeat(64),
            provider: SemanticIngestionProvider::Migraphx,
            device_id: 0,
            max_length: 64,
            batch_size: 64,
            window_aggregation: vesc_knowledge_index::WindowAggregation::TokenWeightedMean,
        });
        let accelerated = KnowledgeSnapshotManifest::with_profile(
            vec![SnapshotRepository {
                repository: repository_id,
                history_tips: vec![first, second.clone()],
                commit: second.clone(),
                policy_digest: repository_policy_digest(repository).expect("policy digest"),
            }],
            Some(accelerated_model),
            SnapshotProfile::CompleteHistory,
        )
        .expect("accelerated snapshot manifest");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            &layout.artifact(&accelerated.id),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("portable semantic artifact");
        write_json_atomic(
            &crate::default_snapshot_path(layout.root().as_path()),
            &accelerated,
        )
        .expect("default snapshot alias");

        let mut cpu_store = KnowledgeSnapshotStore::new(layout.clone());
        cpu_store.semantic = Some(SnapshotSemanticConfig {
            model_dir: temp.path().join("unused-cpu-model"),
            model: serving_model,
        });
        let prepared = cpu_store
            .prepare_default(&repositories)
            .await
            .expect("CPU runtime reuses portable artifact");

        assert_eq!(prepared.manifest.id, accelerated.id);
        assert_eq!(prepared.disposition, SnapshotDisposition::Reused);
        assert!(layout.artifact(&accelerated.id).is_dir());

        let cpu_selected = cpu_store
            .resolve_manifest(
                &repositories,
                &BTreeMap::new(),
                SnapshotProfile::SelectedTrees,
            )
            .expect("CPU selected snapshot manifest");
        write_json_atomic(&layout.snapshot(&cpu_selected.id), &cpu_selected)
            .expect("persist CPU selected snapshot manifest");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            &layout.artifact(&cpu_selected.id),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("mismatched exact-ID artifact");
        let error = cpu_store
            .reuse(&repositories, &BTreeMap::new())
            .expect_err("mismatched exact-ID artifact is not reusable");
        assert!(matches!(error, SnapshotError::NotCached(_)));
        fs::remove_file(layout.snapshot(&cpu_selected.id)).expect("remove exact-ID manifest");
        fs::remove_dir_all(layout.artifact(&cpu_selected.id))
            .expect("remove mismatched exact-ID artifact");

        let accelerated_selected = KnowledgeSnapshotManifest::with_profile(
            vec![SnapshotRepository {
                repository: repository.id().clone(),
                history_tips: vec![second.clone()],
                commit: second,
                policy_digest: repository_policy_digest(repository).expect("policy digest"),
            }],
            accelerated.semantic.clone(),
            SnapshotProfile::SelectedTrees,
        )
        .expect("accelerated selected snapshot manifest");
        write_json_atomic(
            &layout.snapshot(&accelerated_selected.id),
            &accelerated_selected,
        )
        .expect("persist selected snapshot manifest");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            &layout.artifact(&accelerated_selected.id),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("mismatched embedded artifact");
        let error = cpu_store
            .reuse(&repositories, &BTreeMap::new())
            .expect_err("mismatched selected artifact is not reusable");
        assert!(matches!(error, SnapshotError::NotCached(_)));
        fs::remove_dir_all(layout.artifact(&accelerated_selected.id))
            .expect("remove mismatched artifact");

        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        let sources = snapshot_corpus_sources(
            &layout,
            std::slice::from_ref(repository),
            &accelerated_selected,
        )
        .expect("selected Git sources");
        vesc_knowledge_index::build_git_artifacts_with_provider(
            &layout.artifact(&accelerated_selected.id),
            &sources,
            Some((&mut provider, "fake", "test-revision")),
            None,
            None,
        )
        .expect("portable selected semantic artifact");

        let selected = cpu_store
            .reuse(&repositories, &BTreeMap::new())
            .expect("CPU runtime reuses portable selected artifact");
        assert_eq!(selected.manifest.id, accelerated_selected.id);
        assert_eq!(selected.disposition, SnapshotDisposition::Reused);

        let vector_path =
            vesc_knowledge_index::active_generation_path(&layout.artifact(&accelerated.id))
                .expect("active generation")
                .join("vectors.bin");
        fs::remove_file(vector_path).expect("remove vectors");
        let error = cpu_store
            .load_default(SnapshotDisposition::Reused)
            .expect_err("serving requires configured vectors");
        assert!(matches!(error, SnapshotError::Build(_)));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One semantic fixture proves both cache-hit and cache-miss behavior.
    async fn cached_only_derives_selected_tree_from_complete_history_vectors() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let repository = repositories.iter().next().expect("fixture repository");
        ManagedGitStore::new(layout.clone())
            .sync_source(
                repository.id(),
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository sync");

        let serving_semantic = SnapshotSemanticModel {
            model_id: "fake".into(),
            model_revision: "test-revision".into(),
            max_length: 512,
            ingestion: None,
        };
        let mut accelerated_semantic = serving_semantic.clone();
        accelerated_semantic.ingestion = Some(SnapshotSemanticIngestion {
            model_sha256: "f".repeat(64),
            provider: SemanticIngestionProvider::Migraphx,
            device_id: 0,
            max_length: 512,
            batch_size: 8,
            window_aggregation: vesc_knowledge_index::WindowAggregation::TokenWeightedMean,
        });
        let mut store = KnowledgeSnapshotStore::new(layout.clone());
        store.semantic = Some(SnapshotSemanticConfig {
            model_dir: temp.path().join("unused-model"),
            model: accelerated_semantic,
        });
        let complete_history = store
            .resolve_manifest(
                &repositories,
                &BTreeMap::new(),
                SnapshotProfile::CompleteHistory,
            )
            .expect("complete-history manifest");
        let sources =
            snapshot_corpus_sources(&layout, std::slice::from_ref(repository), &complete_history)
                .expect("complete-history sources");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        vesc_knowledge_index::build_git_history_artifacts_from_previous(
            &layout.artifact(&complete_history.id),
            &sources,
            None,
            Some((&mut provider, "fake", "test-revision")),
            None,
            &mut |_| {},
        )
        .expect("complete-history semantic artifact");
        write_json_atomic(&layout.snapshot(&complete_history.id), &complete_history)
            .expect("complete-history snapshot manifest");
        store.semantic.as_mut().expect("semantic config").model = serving_semantic;

        let prepared = store
            .derive_cached(&repositories, &BTreeMap::new())
            .await
            .expect("derive selected tree without inference");

        assert_eq!(prepared.disposition, SnapshotDisposition::Built);
        assert!(artifact_matches(&prepared.artifact_path, "betaunique"));
        assert!(!artifact_matches(&prepared.artifact_path, "alphaunique"));

        run_git(&work, &["switch", "-c", "new-content"]);
        fs::write(work.join("new.md"), "genuinelynewbranchcontent\n").expect("new branch file");
        run_git(&work, &["add", "new.md"]);
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "new branch content",
            ],
        );
        run_git(
            &work,
            &[
                "push",
                remote.to_str().expect("UTF-8 remote path"),
                "new-content",
            ],
        );
        ManagedGitStore::new(layout.clone())
            .sync_source(
                repository.id(),
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("managed repository refresh");
        let new_branch = fixture_registry(&data_root, "refs/heads/new-content");
        let missing = store
            .resolve_manifest(
                &new_branch,
                &BTreeMap::new(),
                SnapshotProfile::SelectedTrees,
            )
            .expect("new branch manifest");
        let training_checkpoint = vector_checkpoint_path(&layout, &missing.id);
        fs::create_dir_all(training_checkpoint.parent().expect("checkpoint parent"))
            .expect("checkpoint directory");
        fs::write(&training_checkpoint, b"resumable training state").expect("training checkpoint");

        let error = store
            .derive_cached(&new_branch, &BTreeMap::new())
            .await
            .expect_err("cached-only must not embed new branch content");

        assert!(matches!(error, SnapshotError::NotCached(_)));
        assert!(!layout.snapshot(&missing.id).is_file());
        assert!(!layout.artifact(&missing.id).exists());
        assert!(
            !vesc_knowledge_index::active_manifest_path(&layout.artifact(&missing.id)).is_file()
        );
        assert_eq!(
            fs::read(&training_checkpoint).expect("preserved training checkpoint"),
            b"resumable training state"
        );
        assert!(
            !layout
                .root()
                .as_path()
                .join("snapshots")
                .join(format!("{}.lock", missing.id.as_str()))
                .exists()
        );
        assert!(
            !layout
                .repository(repository.id())
                .join("refs/vesc-mcp/snapshots")
                .join(missing.id.as_str())
                .exists()
        );
    }

    fn artifact_matches(root: &Path, query: &str) -> bool {
        let repositories = root
            .parent()
            .and_then(Path::parent)
            .expect("artifact below data root")
            .join("repositories");
        let lexical = vesc_knowledge_index::LexicalIndex::open_git_search_artifact(
            &vesc_knowledge_index::active_generation_path(root)
                .expect("active generation")
                .join("lexical.json"),
            &repositories,
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
    }

    fn selected(repository: RepositoryId, commit: String) -> SnapshotRepository {
        SnapshotRepository {
            repository,
            history_tips: vec![commit.clone()],
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
        let semantic = KnowledgeSnapshotManifest::new(
            same.repositories.clone(),
            Some(SnapshotSemanticModel {
                model_id: "fake".into(),
                model_revision: "test".into(),
                max_length: 512,
                ingestion: None,
            }),
        )
        .expect("semantic manifest");
        let shorter_semantic = KnowledgeSnapshotManifest::new(
            same.repositories.clone(),
            Some(SnapshotSemanticModel {
                model_id: "fake".into(),
                model_revision: "test".into(),
                max_length: 256,
                ingestion: None,
            }),
        )
        .expect("semantic manifest with shorter inputs");
        let accelerated_semantic = KnowledgeSnapshotManifest::new(
            same.repositories.clone(),
            Some(SnapshotSemanticModel {
                model_id: "fake".into(),
                model_revision: "test".into(),
                max_length: 512,
                ingestion: Some(SnapshotSemanticIngestion {
                    model_sha256: "f".repeat(64),
                    provider: SemanticIngestionProvider::Migraphx,
                    device_id: 0,
                    max_length: 64,
                    batch_size: 64,
                    window_aggregation: vesc_knowledge_index::WindowAggregation::TokenWeightedMean,
                }),
            }),
        )
        .expect("accelerated semantic manifest");

        assert_eq!(left, same);
        assert_ne!(left.id, moved.id);
        assert_ne!(left.id, policy_changed.id);
        assert_ne!(left.id, complete_history.id);
        assert_ne!(left.id, semantic.id);
        assert_ne!(semantic.id, shorter_semantic.id);
        assert_ne!(semantic.id, accelerated_semantic.id);
        assert!(semantic_serving_contract_matches(
            accelerated_semantic.semantic.as_ref(),
            semantic.semantic.as_ref(),
        ));
        assert!(!semantic_serving_contract_matches(
            semantic.semantic.as_ref(),
            accelerated_semantic.semantic.as_ref(),
        ));
        assert_eq!(left.id.as_str().len(), 64);
    }

    #[test]
    fn selected_tree_manifest_rejects_history_beyond_the_selected_commit() {
        let repository = RepositoryId::new("fixture").expect("repository ID");
        let error = KnowledgeSnapshotManifest::new(
            vec![SnapshotRepository {
                repository,
                commit: "b".repeat(40),
                history_tips: vec!["a".repeat(40), "b".repeat(40)],
                policy_digest: "fixture-policy-v1".into(),
            }],
            None,
        )
        .expect_err("selected-tree identity cannot contain other history tips");

        assert!(matches!(error, SnapshotError::InvalidHistoryTips));
    }

    #[test]
    fn complete_history_manifest_preserves_the_configured_anchor() {
        let repository = RepositoryId::new("fixture").expect("repository ID");
        let mut manifest = KnowledgeSnapshotManifest::with_profile(
            vec![SnapshotRepository {
                repository,
                commit: "b".repeat(40),
                history_tips: vec!["a".repeat(40), "b".repeat(40)],
                policy_digest: "fixture-policy-v1".into(),
            }],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("canonical complete-history manifest");
        assert_eq!(manifest.repositories[0].commit, "b".repeat(40));
        manifest.repositories[0].commit = "c".repeat(40);
        let identity = SnapshotIdentity {
            schema: manifest.schema,
            profile: manifest.profile,
            repositories: &manifest.repositories,
            configured_repositories: &manifest.configured_repositories,
            component_versions: &manifest.component_versions,
            semantic: manifest.semantic.as_ref(),
        };
        manifest.id = KnowledgeSnapshotId::new(hex_sha256(
            &serde_json::to_vec(&identity).expect("forged identity JSON"),
        ))
        .expect("forged snapshot ID");

        assert!(
            !manifest.has_valid_identity(),
            "the configured anchor must still be one of the history tips"
        );
    }

    #[test]
    fn schema_two_manifest_remains_a_valid_incremental_predecessor() {
        let repository = RepositoryId::new("fixture").expect("repository ID");
        let repositories = vec![SnapshotRepository {
            repository: repository.clone(),
            commit: "a".repeat(40),
            history_tips: Vec::new(),
            policy_digest: "fixture-policy-v1".into(),
        }];
        let configured_repositories = vec![SnapshotRepositoryConfiguration {
            repository,
            policy_digest: "fixture-policy-v1".into(),
        }];
        let component_versions = vesc_knowledge_index::artifact_component_versions();
        let legacy_repositories = repositories
            .iter()
            .map(|repository| LegacySnapshotRepository {
                repository: &repository.repository,
                commit: &repository.commit,
                policy_digest: &repository.policy_digest,
            })
            .collect::<Vec<_>>();
        let identity = LegacySnapshotIdentity {
            schema: LEGACY_SNAPSHOT_SCHEMA,
            profile: SnapshotProfile::CompleteHistory,
            repositories: &legacy_repositories,
            configured_repositories: &configured_repositories,
            component_versions: &component_versions,
            semantic: None,
        };
        let id = KnowledgeSnapshotId::new(hex_sha256(
            &serde_json::to_vec(&identity).expect("legacy identity"),
        ))
        .expect("snapshot ID");
        let manifest = KnowledgeSnapshotManifest {
            schema: LEGACY_SNAPSHOT_SCHEMA,
            id,
            profile: SnapshotProfile::CompleteHistory,
            repositories,
            configured_repositories,
            component_versions,
            semantic: None,
        };

        assert!(manifest.has_valid_identity());
    }

    #[test]
    fn schema_two_policy_digest_bridges_to_the_corpus_only_digest() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, _remote, _first, _second) = fixture_remote(temp.path());
        let data_root = temp.path().join("data");
        let repositories = fixture_registry(&data_root, "refs/heads/main");
        let repository = repositories.iter().next().expect("fixture repository");
        let selected_repository = repository.id().clone();
        let previous = KnowledgeSnapshotManifest {
            schema: LEGACY_SNAPSHOT_SCHEMA,
            id: KnowledgeSnapshotId::new("a".repeat(64)).expect("snapshot ID"),
            profile: SnapshotProfile::CompleteHistory,
            repositories: vec![SnapshotRepository {
                repository: selected_repository.clone(),
                commit: "b".repeat(40),
                history_tips: Vec::new(),
                policy_digest: legacy_repository_policy_digest(repository)
                    .expect("legacy policy digest"),
            }],
            configured_repositories: Vec::new(),
            component_versions: vesc_knowledge_index::artifact_component_versions(),
            semantic: None,
        };
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![SnapshotRepository {
                repository: selected_repository.clone(),
                commit: "b".repeat(40),
                history_tips: vec!["b".repeat(40)],
                policy_digest: repository_policy_digest(repository).expect("corpus policy digest"),
            }],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("current manifest");
        let legacy_policy_digests = HashMap::from([(
            selected_repository,
            legacy_repository_policy_digest(repository).expect("legacy policy digest"),
        )]);

        assert!(previous_snapshot_is_incrementally_compatible_with_legacy(
            &previous,
            &current,
            &legacy_policy_digests,
        ));
        assert_ne!(
            previous.repositories[0].policy_digest, current.repositories[0].policy_digest,
            "the bridge must cover the deployed schema-two digest"
        );
    }

    #[test]
    fn incremental_snapshot_compatibility_rejects_removal_and_policy_changes() {
        let one = RepositoryId::new("one").expect("valid id");
        let two = RepositoryId::new("two").expect("valid id");
        let previous = KnowledgeSnapshotManifest::with_profile(
            vec![
                selected(one.clone(), "a".repeat(40)),
                selected(two.clone(), "b".repeat(40)),
            ],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("previous manifest");
        let added = KnowledgeSnapshotManifest::with_profile(
            vec![
                selected(one.clone(), "c".repeat(40)),
                selected(two.clone(), "b".repeat(40)),
                selected(
                    RepositoryId::new("three").expect("valid id"),
                    "d".repeat(40),
                ),
            ],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("added manifest");
        let removed = KnowledgeSnapshotManifest::with_profile(
            vec![selected(one.clone(), "c".repeat(40))],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("removed manifest");
        let mut changed = vec![selected(one, "c".repeat(40)), selected(two, "b".repeat(40))];
        changed[0].policy_digest = "changed-policy".into();
        let changed = KnowledgeSnapshotManifest::with_profile(
            changed,
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("changed manifest");

        assert!(previous_snapshot_is_incrementally_compatible(
            &previous, &added
        ));
        assert!(!previous_snapshot_is_incrementally_compatible(
            &previous, &removed
        ));
        assert!(!previous_snapshot_is_incrementally_compatible(
            &previous, &changed
        ));
    }

    #[test]
    fn semantic_snapshot_configuration_requires_a_complete_model_contract() {
        let root = tempfile::tempdir().expect("data root");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(root.path().to_path_buf()).expect("valid data root"),
        );
        let incomplete = KnowledgeConfig {
            mode: crate::config::RetrievalMode::Auto,
            semantic_model_id: Some(vesc_knowledge_index::JINA_CODE_MODEL_ID.into()),
            ..KnowledgeConfig::default()
        };

        let error = KnowledgeSnapshotStore::new(layout)
            .with_semantic_config(&incomplete)
            .err()
            .expect("incomplete semantic configuration");

        assert!(error.to_string().contains("configured together"));
    }

    #[test]
    fn lexical_mode_does_not_configure_semantic_snapshots() {
        let root = tempfile::tempdir().expect("data root");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(root.path().to_path_buf()).expect("valid data root"),
        );
        let lexical = KnowledgeConfig {
            mode: crate::config::RetrievalMode::Lexical,
            semantic_model_id: Some(vesc_knowledge_index::JINA_CODE_MODEL_ID.into()),
            ..KnowledgeConfig::default()
        };

        let store = KnowledgeSnapshotStore::new(layout)
            .with_semantic_config(&lexical)
            .expect("lexical mode ignores semantic configuration");

        assert!(store.semantic.is_none());
    }

    #[test]
    fn corpus_manifest_schema_upgrade_requires_a_new_snapshot() {
        let mut previous = vesc_knowledge_index::artifact_component_versions();
        previous.insert("corpus-schema".into(), "1.0".into());
        let current = vesc_knowledge_index::artifact_component_versions();

        assert!(!component_versions_are_incrementally_compatible(
            &previous, &current
        ));
    }

    #[test]
    fn lexical_format_upgrade_can_reuse_corpus_and_vectors() {
        let mut previous = vesc_knowledge_index::artifact_component_versions();
        previous.insert("lexical-format".into(), "previous-format".into());
        let current = vesc_knowledge_index::artifact_component_versions();

        assert!(component_versions_are_incrementally_compatible(
            &previous, &current
        ));
    }

    #[test]
    fn selected_tree_vector_aliases_require_the_current_lexical_format() {
        let repository = RepositoryId::new("one").expect("repository id");
        let selected = vec![selected(repository, "a".repeat(40))];
        let mut previous = KnowledgeSnapshotManifest::with_profile(
            selected.clone(),
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("complete-history manifest");
        previous
            .component_versions
            .insert("lexical-format".into(), "previous-format".into());
        let current =
            KnowledgeSnapshotManifest::with_profile(selected, None, SnapshotProfile::SelectedTrees)
                .expect("selected-tree manifest");

        assert!(!previous_vector_snapshot_is_compatible(&previous, &current));
    }

    #[test]
    fn selected_tree_vector_aliases_allow_a_complete_history_repository_superset() {
        let one = RepositoryId::new("one").expect("first repository id");
        let two = RepositoryId::new("two").expect("second repository id");
        let semantic = Some(SnapshotSemanticModel {
            model_id: "fake".into(),
            model_revision: "test-revision".into(),
            max_length: 1,
            ingestion: None,
        });
        let previous = KnowledgeSnapshotManifest::with_profile(
            vec![
                selected(one.clone(), "b".repeat(40)),
                selected(two, "c".repeat(40)),
            ],
            semantic.clone(),
            SnapshotProfile::CompleteHistory,
        )
        .expect("complete-history manifest");
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![selected(one, "a".repeat(40))],
            semantic,
            SnapshotProfile::SelectedTrees,
        )
        .expect("selected-tree manifest");

        assert!(previous_vector_snapshot_is_compatible(&previous, &current));
    }

    #[test]
    fn selected_tree_vectors_reuse_a_schema_two_complete_history_artifact() {
        let root = tempfile::tempdir().expect("data root");
        let data_root = root.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        fs::create_dir_all(data_root.join("snapshots")).expect("snapshot directory");
        let registry = fixture_registry(&data_root, "refs/heads/main");
        let repositories = registry.iter().cloned().collect::<Vec<_>>();
        let repository = repositories.first().expect("fixture repository");
        let semantic = SnapshotSemanticModel {
            model_id: "fake".into(),
            model_revision: "test-revision".into(),
            max_length: 1,
            ingestion: None,
        };
        let previous_repositories = vec![SnapshotRepository {
            repository: repository.id().clone(),
            commit: "a".repeat(40),
            history_tips: Vec::new(),
            policy_digest: legacy_repository_policy_digest(repository)
                .expect("legacy policy digest"),
        }];
        let previous_configuration = vec![SnapshotRepositoryConfiguration {
            repository: repository.id().clone(),
            policy_digest: previous_repositories[0].policy_digest.clone(),
        }];
        let component_versions = vesc_knowledge_index::artifact_component_versions();
        let legacy_repositories = previous_repositories
            .iter()
            .map(|repository| LegacySnapshotRepository {
                repository: &repository.repository,
                commit: &repository.commit,
                policy_digest: &repository.policy_digest,
            })
            .collect::<Vec<_>>();
        let identity = LegacySnapshotIdentity {
            schema: LEGACY_SNAPSHOT_SCHEMA,
            profile: SnapshotProfile::CompleteHistory,
            repositories: &legacy_repositories,
            configured_repositories: &previous_configuration,
            component_versions: &component_versions,
            semantic: Some(&semantic),
        };
        let previous = KnowledgeSnapshotManifest {
            schema: LEGACY_SNAPSHOT_SCHEMA,
            id: KnowledgeSnapshotId::new(hex_sha256(
                &serde_json::to_vec(&identity).expect("legacy identity"),
            ))
            .expect("snapshot ID"),
            profile: SnapshotProfile::CompleteHistory,
            repositories: previous_repositories,
            configured_repositories: previous_configuration,
            component_versions,
            semantic: Some(semantic.clone()),
        };
        write_json_atomic(&layout.snapshot(&previous.id), &previous).expect("snapshot manifest");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            &layout.artifact(&previous.id),
            &mut provider,
            "fake",
            "test-revision",
        )
        .expect("semantic predecessor artifact");
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![SnapshotRepository {
                repository: repository.id().clone(),
                commit: "a".repeat(40),
                history_tips: vec!["a".repeat(40)],
                policy_digest: repository_policy_digest(repository).expect("corpus policy digest"),
            }],
            Some(semantic),
            SnapshotProfile::SelectedTrees,
        )
        .expect("selected-tree manifest");

        assert!(load_previous_vector_artifact(&layout, &repositories, &current).is_some());
    }

    #[test]
    fn vector_only_component_changes_keep_the_corpus_incrementally_compatible() {
        let mut previous = vesc_knowledge_index::artifact_component_versions();
        previous.insert("vector-format".into(), "previous-vector-format".into());
        previous.insert(
            "vesc-knowledge-index".into(),
            "previous-package-version".into(),
        );
        let current = vesc_knowledge_index::artifact_component_versions();

        assert!(component_versions_are_incrementally_compatible(
            &previous, &current
        ));
    }

    #[test]
    fn semantic_change_keeps_a_compatible_lexical_predecessor() {
        let root = tempfile::tempdir().expect("data root");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(root.path().to_path_buf()).expect("valid data root"),
        );
        let repository = RepositoryId::new("one").expect("repository id");
        let selected = vec![selected(repository, "a".repeat(40))];
        let previous = KnowledgeSnapshotManifest::with_profile(
            selected.clone(),
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("lexical manifest");
        let current = KnowledgeSnapshotManifest::with_profile(
            selected,
            Some(SnapshotSemanticModel {
                model_id: "fake".into(),
                model_revision: "next-revision".into(),
                max_length: 1,
                ingestion: None,
            }),
            SnapshotProfile::CompleteHistory,
        )
        .expect("semantic manifest");
        write_json_atomic(&layout.snapshot(&previous.id), &previous).expect("snapshot manifest");
        vesc_knowledge_index::build_embedded_artifacts(&layout.artifact(&previous.id))
            .expect("lexical artifact");

        let candidate = load_previous_snapshot_candidate(&layout, &current, &previous)
            .expect("compatible lexical predecessor");

        assert!(candidate.vector_path.is_none());
    }

    #[test]
    fn changed_tip_prefers_default_then_nearest_reachable_predecessor() {
        let repository = RepositoryId::new("one").expect("repository id");
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository.clone(), "c".repeat(40))],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("current manifest");
        let farther = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository.clone(), "a".repeat(40))],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("farther manifest");
        let nearer = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository, "b".repeat(40))],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("nearer manifest");
        let mut candidates = vec![(farther.clone(), 2), (nearer.clone(), 1)];

        sort_previous_snapshot_candidates(&mut candidates, &current, None);

        assert_eq!(candidates[0].0.id, nearer.id);

        sort_previous_snapshot_candidates(&mut candidates, &current, Some(&farther.id));

        assert_eq!(candidates[0].0.id, farther.id);
    }

    #[test]
    fn unreachable_higher_scoring_snapshot_falls_back_to_reachable_predecessor() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first, second) = fixture_remote(temp.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let one = RepositoryId::new("one").expect("repository id");
        let two = RepositoryId::new("two").expect("repository id");
        fs::create_dir_all(layout.root().as_path().join("repositories"))
            .expect("repositories directory");
        for repository in [&one, &two] {
            run_git(
                temp.path(),
                &[
                    "clone",
                    "--bare",
                    remote.to_str().expect("UTF-8 remote"),
                    layout
                        .repository(repository)
                        .to_str()
                        .expect("UTF-8 managed repository"),
                ],
            );
        }
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![
                selected(one.clone(), second.clone()),
                selected(two.clone(), second.clone()),
            ],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("current manifest");
        let unreachable = KnowledgeSnapshotManifest::with_profile(
            vec![selected(one.clone(), "f".repeat(40)), selected(two, second)],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("unreachable manifest");
        let reachable = KnowledgeSnapshotManifest::with_profile(
            vec![selected(one, first.clone())],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("reachable manifest");
        for candidate in [&unreachable, &reachable] {
            write_json_atomic(&layout.snapshot(&candidate.id), candidate)
                .expect("snapshot manifest");
            vesc_knowledge_index::build_embedded_artifacts(&layout.artifact(&candidate.id))
                .expect("candidate artifact");
        }

        let previous =
            load_previous_snapshot(&layout, &[], &current).expect("reachable predecessor");

        assert_eq!(previous.tips[0].revision.as_str(), first.as_str());
    }

    #[test]
    fn default_switch_keeps_the_all_ref_snapshot_as_a_reachable_predecessor() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, _first, second) = fixture_remote(temp.path());
        run_git(&work, &["switch", "-c", "release/7", "v1"]);
        fs::write(work.join("README.md"), "release seven evidence\n").expect("release file");
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-am",
                "release seven",
            ],
        );
        let release = run_git(&work, &["rev-parse", "HEAD"]);
        run_git(
            &work,
            &["push", remote.to_str().expect("UTF-8 remote"), "release/7"],
        );
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let repository = RepositoryId::new("fixture").expect("repository id");
        fs::create_dir_all(layout.root().as_path().join("repositories"))
            .expect("repositories directory");
        run_git(
            temp.path(),
            &[
                "clone",
                "--bare",
                remote.to_str().expect("UTF-8 remote"),
                layout
                    .repository(&repository)
                    .to_str()
                    .expect("UTF-8 managed repository"),
            ],
        );
        let previous = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository.clone(), second.clone())],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("previous all-ref manifest");
        let mut current_repository = selected(repository, release.clone());
        current_repository.history_tips = vec![second, release];
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![current_repository],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("current all-ref manifest");

        let distances =
            reachable_previous_commit_distances(&layout, &current, std::slice::from_ref(&previous));

        assert!(
            previous_snapshot_distance(&previous, &distances).is_some(),
            "the old default remains reachable from the current all-ref tip union"
        );
    }

    #[test]
    fn corrupt_vectors_keep_best_lexical_predecessor_but_corrupt_lexical_falls_back() {
        let root = tempfile::tempdir().expect("data root");
        let (_work, remote, first, second) = fixture_remote(root.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(root.path().to_path_buf()).expect("valid data root"),
        );
        fs::create_dir_all(root.path().join("snapshots")).expect("snapshot directory");
        let repository = RepositoryId::new("one").expect("repository id");
        fs::create_dir_all(root.path().join("repositories")).expect("repositories directory");
        run_git(
            root.path(),
            &[
                "clone",
                "--bare",
                remote.to_str().expect("UTF-8 remote"),
                layout
                    .repository(&repository)
                    .to_str()
                    .expect("UTF-8 managed repository"),
            ],
        );
        let semantic = SnapshotSemanticModel {
            model_id: "fake".into(),
            model_revision: "test-revision".into(),
            max_length: 1,
            ingestion: None,
        };
        let current = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository.clone(), second.clone())],
            Some(semantic.clone()),
            SnapshotProfile::CompleteHistory,
        )
        .expect("current manifest");
        let fallback = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository.clone(), first.clone())],
            Some(semantic.clone()),
            SnapshotProfile::CompleteHistory,
        )
        .expect("fallback manifest");
        let best = KnowledgeSnapshotManifest::with_profile(
            vec![selected(repository, second.clone())],
            Some(semantic),
            SnapshotProfile::CompleteHistory,
        )
        .expect("best manifest");

        let build = |manifest: &KnowledgeSnapshotManifest| {
            write_json_atomic(&layout.snapshot(&manifest.id), manifest).expect("snapshot manifest");
            let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
            vesc_knowledge_index::build_embedded_artifacts_with_provider(
                &layout.artifact(&manifest.id),
                &mut provider,
                "fake",
                "test-revision",
            )
            .expect("candidate artifact")
        };
        let fallback_build = build(&fallback);
        let best_build = build(&best);
        let best_vector = layout
            .artifact(&best.id)
            .join("generations")
            .join(&best_build.generation)
            .join("vectors.bin");
        let mut corrupt = fs::read(&best_vector).expect("best vectors");
        corrupt[16] ^= 0xff;
        fs::write(&best_vector, corrupt).expect("corrupt best vectors");

        let previous = load_previous_snapshot(&layout, &[], &current).expect("lexical predecessor");

        assert_eq!(previous.tips[0].revision.as_str(), second);
        assert_eq!(
            previous.artifact.generation.to_string(),
            best_build.generation
        );
        assert!(previous.vector_path.is_none());
        assert!(previous.vector_checksum.is_none());

        fs::remove_file(best_vector).expect("remove best vectors");
        let previous = load_previous_snapshot(&layout, &[], &current).expect("lexical predecessor");
        assert_eq!(previous.tips[0].revision.as_str(), second);
        assert!(previous.vector_path.is_none());

        let repaired_best = build(&best);
        let repaired_lexical = layout
            .artifact(&best.id)
            .join("generations")
            .join(repaired_best.generation)
            .join("lexical.json");
        fs::remove_dir_all(repaired_lexical.with_extension("tantivy"))
            .expect("remove best lexical index");

        let previous = load_previous_snapshot(&layout, &[], &current).expect("lexical fallback");

        assert_eq!(previous.tips[0].revision.as_str(), first);
        assert_eq!(
            previous.artifact.generation.to_string(),
            fallback_build.generation
        );
    }

    #[test]
    fn snapshot_identity_validation_uses_stored_component_versions() {
        let mut manifest = KnowledgeSnapshotManifest::with_profile(
            vec![selected(
                RepositoryId::new("one").expect("valid id"),
                "a".repeat(40),
            )],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("manifest");
        manifest
            .component_versions
            .insert("corpus-schema".into(), "1.0".into());
        let identity = SnapshotIdentity {
            schema: manifest.schema,
            profile: manifest.profile,
            repositories: &manifest.repositories,
            configured_repositories: &manifest.configured_repositories,
            component_versions: &manifest.component_versions,
            semantic: manifest.semantic.as_ref(),
        };
        manifest.id = KnowledgeSnapshotId::new(hex_sha256(
            &serde_json::to_vec(&identity).expect("identity JSON"),
        ))
        .expect("snapshot id");

        assert!(manifest.has_valid_identity());
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn semantic_provider_defers_model_initialization_until_inference() {
        let semantic = SnapshotSemanticConfig {
            model_dir: PathBuf::from("/model/must/not/be/opened"),
            model: SnapshotSemanticModel {
                model_id: vesc_knowledge_index::JINA_CODE_MODEL_ID.into(),
                model_revision: vesc_knowledge_index::JINA_CODE_MODEL_REVISION.into(),
                max_length: vesc_knowledge_index::JINA_CODE_MAX_LENGTH,
                ingestion: None,
            },
        };

        let provider = semantic_provider(&semantic).expect("deferred provider");

        assert_eq!(
            provider.embedding_dimension(),
            Some(vesc_knowledge_index::EmbeddingProfile::jina_v2_base_code().dimension)
        );
    }

    #[test]
    fn semantic_ingestion_configuration_rejects_the_wrong_model() {
        let root = tempfile::tempdir().expect("data root");
        let model = tempfile::tempdir().expect("model root");
        fs::write(model.path().join("model.onnx"), b"wrong model").expect("model file");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(root.path().to_path_buf()).expect("valid data root"),
        );
        let config = KnowledgeConfig {
            mode: crate::config::RetrievalMode::Auto,
            semantic_model_dir: Some(model.path().to_path_buf()),
            semantic_model_id: Some(vesc_knowledge_index::JINA_CODE_MODEL_ID.into()),
            semantic_model_revision: Some(vesc_knowledge_index::JINA_CODE_MODEL_REVISION.into()),
            semantic_max_length: Some(vesc_knowledge_index::JINA_CODE_MAX_LENGTH),
            semantic_ingestion: Some(crate::config::SemanticIngestionConfig {
                model_dir: model.path().to_path_buf(),
                model_sha256: "f".repeat(64),
                provider: SemanticIngestionProvider::Migraphx,
                device_id: 0,
                max_length: vesc_knowledge_index::JINA_CODE_INGEST_MAX_LENGTH,
                batch_size: vesc_knowledge_index::JINA_CODE_INGEST_BATCH_SIZE,
                window_aggregation: vesc_knowledge_index::WindowAggregation::TokenWeightedMean,
            }),
            ..KnowledgeConfig::default()
        };

        let error = KnowledgeSnapshotStore::new(layout)
            .with_semantic_config(&config)
            .err()
            .expect("wrong model must fail");

        assert!(error.to_string().contains("SHA-256"));
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

        assert!(
            prepared.default.manifest.repositories[0]
                .history_tips
                .contains(&second)
        );
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
                managed_git: true,
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
        assert_eq!(
            index.repositories.get("fixture"),
            Some(&prepared.default.manifest.repositories[0].commit)
        );
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
    async fn reusable_snapshot_bypasses_the_global_working_set() {
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
        store
            .prepare(&repositories, &BTreeMap::new())
            .await
            .expect("initial snapshot");
        let held = snapshot_build_gate()
            .acquire_owned()
            .await
            .expect("snapshot build gate");

        let reused = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.prepare(&repositories, &BTreeMap::new()),
        )
        .await
        .expect("reuse must not wait for another build")
        .expect("reused snapshot");

        assert_eq!(reused.disposition, SnapshotDisposition::Reused);
        drop(held);
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
    async fn cached_snapshot_repairs_an_incomplete_artifact() {
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
        fs::remove_file(vesc_knowledge_index::active_manifest_path(
            &initial.artifact_path,
        ))
        .expect("remove active selector");
        fs::remove_dir_all(remote).expect("remove fixture remote");

        let repaired = KnowledgeSnapshotStore::new(layout)
            .prepare_default(&repositories)
            .await
            .expect("repair cached artifact");

        assert_eq!(repaired.manifest, initial.manifest);
        assert_eq!(repaired.disposition, SnapshotDisposition::Built);
        assert!(artifact_matches(&repaired.artifact_path, "alphaunique"));
    }

    #[tokio::test]
    async fn moved_default_branch_retains_the_previous_snapshot_without_history_copies() {
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
        assert_eq!(
            store.default_manifest().expect("default alias").id,
            current.manifest.id
        );
    }

    #[tokio::test]
    async fn changing_default_ref_records_a_distinct_all_ref_anchor() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, _first, _second) = fixture_remote(temp.path());
        run_git(&work, &["switch", "-c", "release/7", "v1"]);
        fs::write(work.join("README.md"), "release seven evidence\n").expect("release file");
        run_git(
            &work,
            &[
                "-c",
                "user.name=Test Author",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-am",
                "release seven",
            ],
        );
        run_git(
            &work,
            &["push", remote.to_str().expect("UTF-8 remote"), "release/7"],
        );
        let data_root = temp.path().join("data");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let main = fixture_registry(&data_root, "refs/heads/main");
        let release = fixture_registry(&data_root, "refs/heads/release/7");
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
            .prepare_default(&main)
            .await
            .expect("initial all-ref snapshot");
        assert!(
            store
                .default_configuration_is_current(&main)
                .expect("initial configuration is current")
        );
        assert!(
            !store
                .default_configuration_is_current(&release)
                .expect("changed default ref is inspected")
        );
        let switched = store
            .prepare_default(&release)
            .await
            .expect("switched all-ref snapshot");

        assert_eq!(switched.disposition, SnapshotDisposition::Built);
        assert_ne!(switched.manifest.id, initial.manifest.id);
        assert_ne!(switched.artifact_path, initial.artifact_path);
        assert_ne!(
            switched.manifest.repositories[0].commit,
            initial.manifest.repositories[0].commit
        );
    }

    #[tokio::test]
    async fn failed_default_build_does_not_mask_the_error_with_a_stale_snapshot() {
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

        let error = store
            .prepare_default(&fixture_registry_with_include(
                &data_root,
                "refs/heads/main",
                "[unsupported-glob]",
            ))
            .await
            .expect_err("invalid build policy must fail");

        assert!(matches!(error, SnapshotError::Build(_)));
        assert_eq!(
            store.default_manifest().expect("unchanged default").id,
            initial.manifest.id
        );
        assert!(artifact_matches(&initial.artifact_path, "betaunique"));
    }

    #[tokio::test]
    async fn source_unavailable_stale_fallback_requires_compatible_policy() {
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
        assert!(
            store
                .default_configuration_is_current(&repositories)
                .expect("current configuration")
        );
        assert!(
            !store
                .default_configuration_is_current(&fixture_registry_with_include(
                    &data_root,
                    "refs/heads/main",
                    "**/*.c",
                ))
                .expect("changed configuration")
        );
        fs::remove_file(data_root.join("repositories/fixture.refs.json"))
            .expect("remove source catalog");

        let changed_default = store
            .prepare_default(&fixture_registry(&data_root, "refs/tags/v1"))
            .await
            .expect("all-ref snapshot remains valid when only the default selector changes");
        assert_eq!(changed_default.manifest.id, initial.manifest.id);
        assert_eq!(changed_default.disposition, SnapshotDisposition::Stale);
        store
            .prepare_default(&fixture_registry_with_source(
                &data_root,
                "refs/heads/main",
                "**/*.md",
                "required",
                "https://example.invalid/replacement.git",
            ))
            .await
            .expect_err("changed remote cannot use stale snapshot");
        store
            .prepare_default(&fixture_registry_with_include(
                &data_root,
                "refs/heads/main",
                "**/*.c",
            ))
            .await
            .expect_err("changed policy cannot use stale snapshot");

        let stale = store
            .prepare_default(&repositories)
            .await
            .expect("compatible source outage uses stale snapshot");
        assert_eq!(stale.manifest.id, initial.manifest.id);
        assert_eq!(stale.disposition, SnapshotDisposition::Stale);
    }

    #[test]
    fn configuration_contract_distinguishes_new_from_unavailable_optional_repository() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let data_root = temp.path().join("data");
        let remote = temp.path().join("remote.git");
        let layout =
            KnowledgeDataLayout::new(DataRoot::new(data_root.clone()).expect("absolute data root"));
        let store = KnowledgeSnapshotStore::new(layout);
        let registry = |second_policy: &str| {
            McpConfig::from_toml(
                &format!(
                    r#"
[knowledge]
data_root = "{}"

[[knowledge.repositories]]
id = "required"
remote_url = "{}"
default_ref = "refs/heads/main"
policy = "required"
include = ["**/*.md"]
exclude = []
trust_tier = "official"
license = "MIT"
attribution = "Required fixture"
max_file_bytes = 1048576
max_files = 100
max_total_bytes = 10485760

[[knowledge.repositories]]
id = "sometimes-unavailable"
remote_url = "{}"
default_ref = "refs/heads/main"
policy = "{second_policy}"
include = ["**/*.md"]
exclude = []
trust_tier = "official"
license = "MIT"
attribution = "Sometimes unavailable fixture"
max_file_bytes = 1048576
max_files = 100
max_total_bytes = 10485760
"#,
                    data_root.display(),
                    remote.display(),
                    remote.display(),
                ),
                &DataRootInputs::default(),
            )
            .expect("repository configuration")
            .knowledge
            .repositories
        };
        let repositories = registry("optional");
        let required = repositories
            .iter()
            .find(|repository| repository.id().as_str() == "required")
            .expect("required repository");
        let manifest_before_optional = KnowledgeSnapshotManifest::with_profile(
            vec![SnapshotRepository {
                repository: required.id().clone(),
                history_tips: vec!["a".repeat(40)],
                commit: "a".repeat(40),
                policy_digest: repository_policy_digest(required).expect("policy digest"),
            }],
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("snapshot with required repository");
        let manifest_after_optional = KnowledgeSnapshotManifest::with_profile_and_configuration(
            manifest_before_optional.repositories.clone(),
            repository_configuration(&repositories).expect("configured repositories"),
            None,
            SnapshotProfile::CompleteHistory,
        )
        .expect("snapshot with attempted optional repository");

        assert!(
            !store
                .snapshot_contract_matches(&manifest_before_optional, &repositories)
                .expect("new optional contract")
        );
        assert!(
            store
                .snapshot_contract_matches(&manifest_after_optional, &repositories)
                .expect("attempted optional contract")
        );
        assert!(
            !store
                .snapshot_contract_matches(&manifest_after_optional, &registry("required"))
                .expect("required contract")
        );
    }

    #[test]
    fn policy_digest_canonicalizes_set_like_path_rules() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let data_root = temp.path().join("data");
        let remote = temp.path().join("remote.git");
        let registry = |include: &str, exclude: &str| {
            McpConfig::from_toml(
                &format!(
                    r#"
[knowledge]
data_root = "{}"

[[knowledge.repositories]]
id = "fixture"
remote_url = "{}"
default_ref = "refs/heads/main"
policy = "required"
include = [{include}]
exclude = [{exclude}]
trust_tier = "official"
license = "MIT"
attribution = "Fixture"
max_file_bytes = 1048576
max_files = 100
max_total_bytes = 10485760
"#,
                    data_root.display(),
                    remote.display(),
                ),
                &DataRootInputs::default(),
            )
            .expect("repository configuration")
            .knowledge
            .repositories
        };
        let left = registry(r#""**/*.md", "**/*.c""#, r#""target/**", "build/**""#);
        let right = registry(r#""**/*.c", "**/*.md""#, r#""build/**", "target/**""#);

        assert_eq!(
            repository_policy_digest(left.iter().next().expect("left repository"))
                .expect("left digest"),
            repository_policy_digest(right.iter().next().expect("right repository"))
                .expect("right digest")
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One monotonic retry sequence covers every commit boundary.
    async fn distributed_publication_exposes_only_an_old_or_new_complete_snapshot() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, _first, _second) = fixture_remote(temp.path());
        let live_root = temp.path().join("live");
        let staged_root = temp.path().join("staged");
        let live_layout = KnowledgeDataLayout::new(
            DataRoot::new(live_root.clone()).expect("absolute live data root"),
        );
        let staged_layout = KnowledgeDataLayout::new(
            DataRoot::new(staged_root.clone()).expect("absolute staged data root"),
        );
        let live_repositories = fixture_registry(&live_root, "refs/heads/main");
        let staged_repositories = fixture_registry(&staged_root, "refs/heads/main");
        let repository_id = RepositoryId::new("fixture").expect("repository id");

        ManagedGitStore::new(live_layout.clone())
            .sync_source(
                &repository_id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("sync old live repository");
        let live_store = KnowledgeSnapshotStore::new(live_layout.clone());
        let old = live_store
            .prepare_default(&live_repositories)
            .await
            .expect("prepare old live snapshot");

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
        ManagedGitStore::new(staged_layout.clone())
            .sync_source(
                &repository_id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("sync staged repository");
        let staged = KnowledgeSnapshotStore::new(staged_layout)
            .prepare_default(&staged_repositories)
            .await
            .expect("prepare staged snapshot");
        assert_ne!(old.manifest.id, staged.manifest.id);

        let boundaries = [
            DistributedPublicationBoundary::ArtifactGeneration,
            DistributedPublicationBoundary::ArtifactActivation,
            DistributedPublicationBoundary::RepositoryObjects,
            DistributedPublicationBoundary::RepositoryReferences,
            DistributedPublicationBoundary::RepositoryCatalog,
            DistributedPublicationBoundary::RepositoryPins,
            DistributedPublicationBoundary::SnapshotManifest,
            DistributedPublicationBoundary::DefaultAlias,
        ];
        for boundary in boundaries {
            let error = live_store
                .install_distributed_default_with_hook(
                    DataRoot::new(staged_root.clone()).expect("staged data root"),
                    &live_repositories,
                    |reached| {
                        if reached == boundary {
                            return Err(SnapshotError::Build(format!(
                                "interrupted after {reached:?}"
                            )));
                        }
                        Ok(())
                    },
                )
                .expect_err("injected publication interruption");
            assert!(error.to_string().contains("interrupted after"));

            let expected = if boundary == DistributedPublicationBoundary::DefaultAlias {
                &staged.manifest.id
            } else {
                &old.manifest.id
            };
            let visible = live_store
                .load_default(SnapshotDisposition::Reused)
                .expect("visible default remains complete");
            assert_eq!(&visible.manifest.id, expected);

            let current = if boundary < DistributedPublicationBoundary::RepositoryCatalog {
                &old.manifest.id
            } else {
                &staged.manifest.id
            };
            prune_obsolete_incomplete_snapshots(&live_layout, current)
                .expect("restart pruning remains safe");
            let visible = live_store
                .load_default(SnapshotDisposition::Reused)
                .expect("visible default survives restart pruning");
            assert_eq!(&visible.manifest.id, expected);
        }

        let installed = live_store
            .install_distributed_default(
                DataRoot::new(staged_root).expect("staged data root"),
                &live_repositories,
            )
            .expect("retry completes publication");
        assert_eq!(installed.manifest.id, staged.manifest.id);
        assert_eq!(
            live_store
                .load_default(SnapshotDisposition::Reused)
                .expect("new default is complete")
                .manifest
                .id,
            staged.manifest.id
        );
    }

    #[tokio::test]
    async fn distributed_publication_rejects_corrupt_staging_before_live_writes() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first, _second) = fixture_remote(temp.path());
        let staged_root = temp.path().join("staged");
        let live_root = temp.path().join("live");
        let staged_layout = KnowledgeDataLayout::new(
            DataRoot::new(staged_root.clone()).expect("absolute staged data root"),
        );
        let staged_repositories = fixture_registry(&staged_root, "refs/heads/main");
        let live_repositories = fixture_registry(&live_root, "refs/heads/main");
        let repository_id = RepositoryId::new("fixture").expect("repository id");
        ManagedGitStore::new(staged_layout.clone())
            .sync_source(
                &repository_id,
                remote.to_str().expect("UTF-8 remote path"),
                "refs/heads/main",
            )
            .await
            .expect("sync staged repository");
        let staged = KnowledgeSnapshotStore::new(staged_layout.clone())
            .prepare_default(&staged_repositories)
            .await
            .expect("prepare staged snapshot");
        let semantic = SnapshotSemanticModel {
            model_id: "fake".into(),
            model_revision: "test-revision".into(),
            max_length: 512,
            ingestion: None,
        };
        let semantic_manifest = KnowledgeSnapshotManifest::with_profile_and_configuration(
            staged.manifest.repositories.clone(),
            staged.manifest.configured_repositories,
            Some(semantic.clone()),
            SnapshotProfile::CompleteHistory,
        )
        .expect("semantic staged manifest");
        let configured = staged_repositories.iter().cloned().collect::<Vec<_>>();
        let sources = snapshot_corpus_sources(&staged_layout, &configured, &semantic_manifest)
            .expect("semantic staged sources");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(4);
        vesc_knowledge_index::build_git_history_artifacts_from_previous(
            &staged_layout.artifact(&semantic_manifest.id),
            &sources,
            None,
            Some((&mut provider, "fake", "test-revision")),
            None,
            &mut |_| {},
        )
        .expect("semantic staged artifact");
        write_json_atomic(
            &staged_layout.snapshot(&semantic_manifest.id),
            &semantic_manifest,
        )
        .expect("semantic snapshot manifest");
        write_json_atomic(
            &crate::default_snapshot_path(staged_layout.root().as_path()),
            &semantic_manifest,
        )
        .expect("semantic default alias");
        let semantic_artifact = staged_layout.artifact(&semantic_manifest.id);
        let generation = vesc_knowledge_index::active_generation_path(&semantic_artifact)
            .expect("active staged generation");
        let vectors_path = generation.join("vectors.bin");
        let mut vectors = fs::read(&vectors_path).expect("read staged vector artifact");
        vectors[0] ^= 0xff;
        fs::write(vectors_path, vectors).expect("corrupt staged vector artifact");

        let live_layout = KnowledgeDataLayout::new(
            DataRoot::new(live_root.clone()).expect("absolute live data root"),
        );
        let mut live_store = KnowledgeSnapshotStore::new(live_layout);
        live_store.semantic = Some(SnapshotSemanticConfig {
            model_dir: temp.path().join("unused-model"),
            model: semantic,
        });
        let error = live_store
            .install_distributed_default(
                DataRoot::new(staged_root).expect("staged data root"),
                &live_repositories,
            )
            .expect_err("corrupt stage must be rejected");

        assert!(
            error.to_string().contains("checksum mismatch"),
            "unexpected validation error: {error}"
        );
        assert!(!live_root.join("artifacts").exists());
        assert!(!live_root.join("repositories").exists());
        assert!(!crate::default_snapshot_path(&live_root).exists());
    }
}
