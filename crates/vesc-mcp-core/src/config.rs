//! MCP server configuration from `~/.config/vesc-mcp/config.toml` and environment.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::catalog::CatalogRepo;
use crate::managed_repositories::{
    DataRoot, DataRootInputs, KnowledgeDataLayout, KnowledgeSnapshotId, ManagedRepositoryError,
    RepositoryId, RepositoryRegistry, RepositoryWire, resolve_data_root,
    validate_repository_config,
};
use crate::workspace;

/// Environment variable for comma- or colon-separated package sandbox roots.
pub const VESC_PACKAGE_ROOTS_ENV: &str = "VESC_PACKAGE_ROOTS";
/// Environment variable overriding the `vesc_tool` binary path.
pub const VESC_TOOL_PATH_ENV: &str = "VESC_TOOL_PATH";
/// Environment variable gating flash/upload tools (default off).
pub const VESC_MCP_ENABLE_FLASH_ENV: &str = "VESC_MCP_ENABLE_FLASH";
/// Environment variable overriding the config TOML file path.
pub const VESC_MCP_CONFIG_ENV: &str = "VESC_MCP_CONFIG";
/// Environment variable selecting the durable model-feedback directory.
pub const VESC_RAG_FEEDBACK_PATH_ENV: &str = "VESC_RAG_FEEDBACK_PATH";
/// Environment variable enabling model-feedback write tools.
pub const VESC_RAG_FEEDBACK_WRITES_ENV: &str = "VESC_RAG_FEEDBACK_WRITES";
/// Environment variable selecting the staged knowledge retrieval mode.
pub const VESC_RAG_MODE_ENV: &str = "VESC_RAG_MODE";
/// Environment variable selecting the generated knowledge artifact path.
pub const VESC_RAG_ARTIFACT_ENV: &str = "VESC_RAG_ARTIFACT";
/// Environment variable selecting the local semantic model directory.
pub const VESC_RAG_SEMANTIC_MODEL_DIR_ENV: &str = "VESC_RAG_SEMANTIC_MODEL_DIR";
/// Environment variable identifying the provisioned semantic model.
pub const VESC_RAG_SEMANTIC_MODEL_ID_ENV: &str = "VESC_RAG_SEMANTIC_MODEL_ID";
/// Environment variable identifying the provisioned semantic model revision.
pub const VESC_RAG_SEMANTIC_MODEL_REVISION_ENV: &str = "VESC_RAG_SEMANTIC_MODEL_REVISION";
/// Environment variable limiting semantic model inputs below the registered maximum.
pub const VESC_RAG_SEMANTIC_MAX_LENGTH_ENV: &str = "VESC_RAG_SEMANTIC_MAX_LENGTH";
/// Environment variable controlling how long an idle semantic model remains loaded.
pub const VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS_ENV: &str = "VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS";

/// Typed configuration loading and validation failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("invalid configuration TOML")]
    Toml,
    #[error(transparent)]
    ManagedRepository(#[from] ManagedRepositoryError),
    #[error("prewarm selection references unknown repository {0}")]
    UnknownPrewarmRepository(RepositoryId),
    #[error("invalid prewarm selector for repository {0}")]
    InvalidPrewarmSelector(RepositoryId),
}

/// Knowledge retrieval rollout mode.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    #[default]
    Lexical,
    Legacy,
    Auto,
    Hybrid,
}

impl FromStr for RetrievalMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "lexical" => Ok(Self::Lexical),
            "auto" => Ok(Self::Auto),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(format!("unsupported retrieval mode {other:?}")),
        }
    }
}

/// Durable model-feedback configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedbackConfig {
    pub path: Option<PathBuf>,
    pub writes_enabled: bool,
}

/// Bounded knowledge retrieval configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeConfig {
    pub mode: RetrievalMode,
    pub artifact_path: Option<PathBuf>,
    pub data_root: Option<DataRoot>,
    pub repositories: RepositoryRegistry,
    pub prewarm: Vec<BTreeMap<RepositoryId, String>>,
    pub semantic_model_dir: Option<PathBuf>,
    pub semantic_model_id: Option<String>,
    pub semantic_model_revision: Option<String>,
    pub semantic_max_length: Option<usize>,
    pub semantic_idle_timeout_secs: u64,
    pub max_limit: usize,
    pub max_query_bytes: usize,
    pub max_response_bytes: usize,
    pub max_passage_bytes: usize,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            mode: RetrievalMode::Lexical,
            artifact_path: None,
            data_root: None,
            repositories: RepositoryRegistry::default(),
            prewarm: Vec::new(),
            semantic_model_dir: None,
            semantic_model_id: None,
            semantic_model_revision: None,
            semantic_max_length: None,
            semantic_idle_timeout_secs: 5 * 60,
            max_limit: 50,
            max_query_bytes: 4 * 1024,
            max_response_bytes: 64 * 1024,
            max_passage_bytes: 8 * 1024,
        }
    }
}

impl KnowledgeConfig {
    pub(crate) fn resolved_artifact(&self) -> Option<ResolvedKnowledgeArtifact> {
        if let Some(path) = self.artifact_path.clone() {
            return Some(ResolvedKnowledgeArtifact {
                path,
                snapshot_id: None,
                snapshot_profile: None,
                repositories: BTreeMap::new(),
            });
        }
        if self.repositories.is_empty() {
            return None;
        }
        let layout = KnowledgeDataLayout::new(self.data_root.clone()?);
        let alias: DefaultSnapshotAlias = serde_json::from_slice(
            &std::fs::read(layout.root().as_path().join("default-snapshot.json")).ok()?,
        )
        .ok()?;
        let snapshot: StoredSnapshotManifest =
            serde_json::from_slice(&std::fs::read(layout.snapshot(&alias.id)).ok()?).ok()?;
        if snapshot.id != alias.id {
            return None;
        }
        let path = layout.artifact(&alias.id);
        path.is_dir().then(|| ResolvedKnowledgeArtifact {
            path,
            snapshot_id: Some(snapshot.id),
            snapshot_profile: snapshot.profile,
            repositories: snapshot
                .repositories
                .into_iter()
                .map(|repository| (repository.repository, repository.commit))
                .collect(),
        })
    }

    /// Resolve the explicit compatibility artifact or the current managed default.
    #[must_use]
    pub fn resolved_artifact_path(&self) -> Option<PathBuf> {
        self.resolved_artifact().map(|artifact| artifact.path)
    }
}

pub(crate) struct ResolvedKnowledgeArtifact {
    pub path: PathBuf,
    pub snapshot_id: Option<KnowledgeSnapshotId>,
    pub snapshot_profile: Option<String>,
    pub repositories: BTreeMap<RepositoryId, String>,
}

#[derive(Deserialize)]
struct DefaultSnapshotAlias {
    id: KnowledgeSnapshotId,
}

#[derive(Deserialize)]
struct StoredSnapshotManifest {
    id: KnowledgeSnapshotId,
    #[serde(default)]
    profile: Option<String>,
    repositories: Vec<StoredSnapshotRepository>,
}

#[derive(Deserialize)]
struct StoredSnapshotRepository {
    repository: RepositoryId,
    commit: String,
}

static CONFIG: OnceLock<McpConfig> = OnceLock::new();

/// Resolved MCP server configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub package_roots: Vec<PathBuf>,
    pub refloat_root: PathBuf,
    pub vesc_root: PathBuf,
    pub poc_root: PathBuf,
    pub vesc_tool_root: PathBuf,
    pub vesc_tool_path: PathBuf,
    pub enable_flash: bool,
    pub knowledge: KnowledgeConfig,
    pub feedback: FeedbackConfig,
}

impl McpConfig {
    /// Load configuration once per process (env overrides file overrides defaults).
    #[must_use]
    pub fn load() -> &'static Self {
        CONFIG.get_or_init(Self::from_sources)
    }

    fn from_sources() -> Self {
        Self::try_from_sources().unwrap_or_else(|error| panic!("vesc-mcp configuration: {error}"))
    }

    fn try_from_sources() -> Result<Self, ConfigError> {
        let file = read_config_file(&config_file_path())?;
        let env = read_env_overrides();
        let mut config = try_merge_config(&file, &env, &DataRootInputs::from_env())?;
        apply_jina_code_query_defaults(&mut config, &file, &env);
        if config.package_roots.is_empty() {
            #[cfg(any(test, feature = "test-fixtures"))]
            {
                return Ok(Self {
                    package_roots: vec![crate::workspace::fixtures_root()],
                    ..config
                });
            }
        }
        Ok(config)
    }

    /// Parse and validate a complete TOML configuration without touching disk.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed TOML, repository policy violations,
    /// duplicate IDs, unsafe paths, or an unavailable application data root.
    pub fn from_toml(content: &str, data_roots: &DataRootInputs) -> Result<Self, ConfigError> {
        let file = toml::from_str(content).map_err(|_| ConfigError::Toml)?;
        try_merge_config(&file, &EnvOverrides::default(), data_roots)
    }
}

fn apply_jina_code_query_defaults(config: &mut McpConfig, file: &ConfigFile, env: &EnvOverrides) {
    let knowledge = file.knowledge.as_ref();
    let explicit = env.knowledge_mode.is_some()
        || env.knowledge_artifact.is_some()
        || env.semantic_model_dir.is_some()
        || env.semantic_model_id.is_some()
        || env.semantic_model_revision.is_some()
        || env.semantic_max_length.is_some()
        || knowledge.is_some_and(|section| {
            section.mode.is_some() || section.artifact_path.is_some() || section.semantic.is_some()
        });
    if explicit {
        return;
    }
    let Some(root) = workspace::workspace_root() else {
        return;
    };
    let Some(profile) = vesc_knowledge_index::JinaCodeQueryProfile::detect(&root) else {
        return;
    };
    apply_jina_code_query_profile(config, profile);
}

fn apply_jina_code_query_profile(
    config: &mut McpConfig,
    profile: vesc_knowledge_index::JinaCodeQueryProfile,
) {
    config.knowledge.mode = RetrievalMode::Auto;
    config.knowledge.artifact_path = Some(profile.artifact_dir);
    config.knowledge.semantic_model_dir = Some(profile.query_model_dir);
    config.knowledge.semantic_model_id = Some(vesc_knowledge_index::JINA_CODE_MODEL_ID.into());
    config.knowledge.semantic_model_revision =
        Some(vesc_knowledge_index::JINA_CODE_MODEL_REVISION.into());
    config.knowledge.semantic_max_length = Some(vesc_knowledge_index::JINA_CODE_MAX_LENGTH);
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ConfigFile {
    paths: Option<PathsSection>,
    features: Option<FeaturesSection>,
    knowledge: Option<KnowledgeSection>,
    feedback: Option<FeedbackSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PathsSection {
    package_roots: Option<Vec<String>>,
    refloat_root: Option<String>,
    vesc_root: Option<String>,
    poc_root: Option<String>,
    vesc_tool_root: Option<String>,
    vesc_tool: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FeaturesSection {
    enable_flash: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct KnowledgeSection {
    mode: Option<RetrievalMode>,
    artifact_path: Option<String>,
    data_root: Option<String>,
    #[serde(default)]
    repositories: Vec<RepositoryWire>,
    #[serde(default)]
    prewarm: Vec<BTreeMap<String, String>>,
    semantic: Option<SemanticSection>,
    max_limit: Option<usize>,
    max_query_bytes: Option<usize>,
    max_response_bytes: Option<usize>,
    max_passage_bytes: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(clippy::struct_field_names)]
struct SemanticSection {
    model_dir: Option<String>,
    model_id: Option<String>,
    model_revision: Option<String>,
    max_length: Option<usize>,
    idle_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FeedbackSection {
    path: Option<String>,
    writes_enabled: Option<bool>,
}

#[derive(Debug, Clone, Default)]
struct EnvOverrides {
    package_roots: Option<Vec<PathBuf>>,
    refloat_root: Option<PathBuf>,
    vesc_root: Option<PathBuf>,
    poc_root: Option<PathBuf>,
    vesc_tool_root: Option<PathBuf>,
    vesc_tool_path: Option<PathBuf>,
    enable_flash: Option<bool>,
    knowledge_mode: Option<RetrievalMode>,
    knowledge_artifact: Option<PathBuf>,
    semantic_model_dir: Option<PathBuf>,
    semantic_model_id: Option<String>,
    semantic_model_revision: Option<String>,
    semantic_max_length: Option<usize>,
    semantic_idle_timeout_secs: Option<u64>,
    feedback_path: Option<PathBuf>,
    feedback_writes_enabled: Option<bool>,
}

/// Default config file location: `~/.config/vesc-mcp/config.toml`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    env::var("HOME").map_or_else(
        |_| PathBuf::from(".config/vesc-mcp/config.toml"),
        |home| PathBuf::from(home).join(".config/vesc-mcp/config.toml"),
    )
}

/// Resolved config file path (`VESC_MCP_CONFIG` or [`default_config_path`]).
#[must_use]
pub fn config_file_path() -> PathBuf {
    resolve_config_file_path(env::var(VESC_MCP_CONFIG_ENV).ok().as_deref())
}

fn resolve_config_file_path(env_override: Option<&str>) -> PathBuf {
    env_override.map_or_else(default_config_path, workspace::expand_path)
}

fn read_config_file(path: &Path) -> Result<ConfigFile, ConfigError> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(ConfigFile::default());
    };
    toml::from_str(&content).map_err(|_| ConfigError::Toml)
}

fn read_env_overrides() -> EnvOverrides {
    EnvOverrides {
        package_roots: env::var(VESC_PACKAGE_ROOTS_ENV)
            .ok()
            .map(|value| split_path_list(&value)),
        refloat_root: env::var(CatalogRepo::Refloat.env_var())
            .ok()
            .map(PathBuf::from),
        vesc_root: env::var(CatalogRepo::Vesc.env_var())
            .ok()
            .map(PathBuf::from),
        poc_root: env::var(CatalogRepo::Poc.env_var()).ok().map(PathBuf::from),
        vesc_tool_root: env::var(CatalogRepo::VescTool.env_var())
            .ok()
            .map(PathBuf::from),
        vesc_tool_path: env::var(VESC_TOOL_PATH_ENV).ok().map(PathBuf::from),
        enable_flash: env::var(VESC_MCP_ENABLE_FLASH_ENV)
            .ok()
            .as_deref()
            .map(parse_bool_env),
        knowledge_mode: env::var(VESC_RAG_MODE_ENV)
            .ok()
            .and_then(|value| RetrievalMode::from_str(&value).ok()),
        knowledge_artifact: env::var(VESC_RAG_ARTIFACT_ENV)
            .ok()
            .map(|value| workspace::expand_path(&value)),
        semantic_model_dir: env::var(VESC_RAG_SEMANTIC_MODEL_DIR_ENV)
            .ok()
            .map(|value| workspace::expand_path(&value)),
        semantic_model_id: env::var(VESC_RAG_SEMANTIC_MODEL_ID_ENV).ok(),
        semantic_model_revision: env::var(VESC_RAG_SEMANTIC_MODEL_REVISION_ENV).ok(),
        semantic_max_length: env::var(VESC_RAG_SEMANTIC_MAX_LENGTH_ENV)
            .ok()
            .and_then(|value| value.parse().ok()),
        semantic_idle_timeout_secs: env::var(VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS_ENV)
            .ok()
            .and_then(|value| value.parse().ok()),
        feedback_path: env::var(VESC_RAG_FEEDBACK_PATH_ENV)
            .ok()
            .map(|value| workspace::expand_path(&value)),
        feedback_writes_enabled: env::var(VESC_RAG_FEEDBACK_WRITES_ENV)
            .ok()
            .as_deref()
            .map(parse_bool_env),
    }
}

#[allow(clippy::too_many_lines)]
fn try_merge_config(
    file: &ConfigFile,
    env: &EnvOverrides,
    data_roots: &DataRootInputs,
) -> Result<McpConfig, ConfigError> {
    let paths = file.paths.as_ref();
    let features = file.features.as_ref();
    let knowledge = file.knowledge.as_ref();
    let feedback = file.feedback.as_ref();

    let package_roots = env
        .package_roots
        .clone()
        .or_else(|| {
            paths.and_then(|section| {
                section.package_roots.as_ref().map(|roots| {
                    roots
                        .iter()
                        .map(|entry| workspace::expand_path(entry))
                        .collect()
                })
            })
        })
        .unwrap_or_default();

    let defaults = KnowledgeConfig::default();
    let data_root = resolve_data_root(
        knowledge
            .and_then(|section| section.data_root.as_deref())
            .map(Path::new),
        data_roots,
    )?;
    let repositories = validate_repository_config(
        knowledge.map_or_else(Vec::new, |section| section.repositories.clone()),
    )?;
    let prewarm = validate_prewarm(
        knowledge.map_or_else(Vec::new, |section| section.prewarm.clone()),
        &repositories,
    )?;
    Ok(McpConfig {
        package_roots,
        refloat_root: env.refloat_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.refloat_root.as_deref())
                .map_or_else(
                    || CatalogRepo::Refloat.resolve_root(),
                    workspace::expand_path,
                )
        }),
        vesc_root: env.vesc_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.vesc_root.as_deref())
                .map_or_else(|| CatalogRepo::Vesc.resolve_root(), workspace::expand_path)
        }),
        poc_root: env.poc_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.poc_root.as_deref())
                .map_or_else(|| CatalogRepo::Poc.resolve_root(), workspace::expand_path)
        }),
        vesc_tool_root: env.vesc_tool_root.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.vesc_tool_root.as_deref())
                .map_or_else(
                    || CatalogRepo::VescTool.resolve_root(),
                    workspace::expand_path,
                )
        }),
        vesc_tool_path: env.vesc_tool_path.clone().unwrap_or_else(|| {
            paths
                .and_then(|section| section.vesc_tool.as_deref())
                .map_or_else(|| PathBuf::from("vesc_tool"), workspace::expand_path)
        }),
        enable_flash: env
            .enable_flash
            .or_else(|| features.and_then(|section| section.enable_flash))
            .unwrap_or(false),
        knowledge: KnowledgeConfig {
            mode: env
                .knowledge_mode
                .or_else(|| knowledge.and_then(|section| section.mode))
                .unwrap_or(defaults.mode),
            artifact_path: env.knowledge_artifact.clone().or_else(|| {
                knowledge
                    .and_then(|section| section.artifact_path.as_deref())
                    .map(workspace::expand_path)
            }),
            data_root: Some(data_root),
            repositories,
            prewarm,
            semantic_model_dir: env.semantic_model_dir.clone().or_else(|| {
                knowledge
                    .and_then(|section| section.semantic.as_ref())
                    .and_then(|semantic| semantic.model_dir.as_deref())
                    .map(workspace::expand_path)
            }),
            semantic_model_id: env.semantic_model_id.clone().or_else(|| {
                knowledge
                    .and_then(|section| section.semantic.as_ref())
                    .and_then(|semantic| semantic.model_id.clone())
            }),
            semantic_model_revision: env.semantic_model_revision.clone().or_else(|| {
                knowledge
                    .and_then(|section| section.semantic.as_ref())
                    .and_then(|semantic| semantic.model_revision.clone())
            }),
            semantic_max_length: env.semantic_max_length.or_else(|| {
                knowledge
                    .and_then(|section| section.semantic.as_ref())
                    .and_then(|semantic| semantic.max_length)
            }),
            semantic_idle_timeout_secs: env
                .semantic_idle_timeout_secs
                .or_else(|| {
                    knowledge
                        .and_then(|section| section.semantic.as_ref())
                        .and_then(|semantic| semantic.idle_timeout_secs)
                })
                .unwrap_or(defaults.semantic_idle_timeout_secs),
            max_limit: knowledge
                .and_then(|section| section.max_limit)
                .unwrap_or(defaults.max_limit),
            max_query_bytes: knowledge
                .and_then(|section| section.max_query_bytes)
                .unwrap_or(defaults.max_query_bytes),
            max_response_bytes: knowledge
                .and_then(|section| section.max_response_bytes)
                .unwrap_or(defaults.max_response_bytes),
            max_passage_bytes: knowledge
                .and_then(|section| section.max_passage_bytes)
                .unwrap_or(defaults.max_passage_bytes),
        },
        feedback: FeedbackConfig {
            path: env.feedback_path.clone().or_else(|| {
                feedback
                    .and_then(|section| section.path.as_deref())
                    .map(workspace::expand_path)
            }),
            writes_enabled: env
                .feedback_writes_enabled
                .or_else(|| feedback.and_then(|section| section.writes_enabled))
                .unwrap_or(false),
        },
    })
}

fn validate_prewarm(
    selections: Vec<BTreeMap<String, String>>,
    repositories: &RepositoryRegistry,
) -> Result<Vec<BTreeMap<RepositoryId, String>>, ConfigError> {
    selections
        .into_iter()
        .map(|selection| {
            selection
                .into_iter()
                .map(|(raw_id, selector)| {
                    let id = RepositoryId::new(raw_id)?;
                    if !repositories.iter().any(|repository| repository.id() == &id) {
                        return Err(ConfigError::UnknownPrewarmRepository(id));
                    }
                    let valid = !selector.is_empty()
                        && selector.len() <= 256
                        && !selector
                            .chars()
                            .any(|character| character.is_control() || character.is_whitespace());
                    valid
                        .then_some((id.clone(), selector))
                        .ok_or(ConfigError::InvalidPrewarmSelector(id))
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
fn merge_config(file: &ConfigFile, env: &EnvOverrides) -> McpConfig {
    try_merge_config(file, env, &DataRootInputs::from_env()).expect("valid test configuration")
}

/// Resolve package roots from explicit tool params or loaded config.
#[must_use]
pub fn resolve_package_roots(explicit: &[String], config: &McpConfig) -> Vec<PathBuf> {
    if !explicit.is_empty() {
        return explicit.iter().map(PathBuf::from).collect();
    }
    config.package_roots.clone()
}

/// Split a comma- or colon-separated path list (env `VESC_PACKAGE_ROOTS` format).
#[must_use]
pub fn split_path_list(value: &str) -> Vec<PathBuf> {
    value
        .split([',', ':'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(workspace::expand_path)
        .collect()
}

#[must_use]
pub fn expand_tilde(path: &str) -> PathBuf {
    workspace::expand_path(path)
}

fn parse_bool_env(value: &str) -> bool {
    matches!(
        value.trim(),
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

/// Resolve allowed roots from an override or loaded config.
#[must_use]
pub fn allowed_package_roots(override_roots: Option<&[PathBuf]>) -> Vec<PathBuf> {
    override_roots.map_or_else(
        || McpConfig::load().package_roots.clone(),
        <[PathBuf]>::to_vec,
    )
}

/// Validate that `path` is a directory under one of the configured package roots.
///
/// # Errors
///
/// Returns an error when no roots are configured, the path is not a directory,
/// canonicalization fails, or the path lies outside all allowed roots.
pub fn validate_sandbox_path(path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    if allowed_roots.is_empty() {
        return Err(format!(
            "path sandbox: set {VESC_PACKAGE_ROOTS_ENV} to allow package roots (comma-separated paths)"
        ));
    }

    if !path.is_dir() {
        return Err(format!("path is not a directory: {}", path.display()));
    }

    let canonical = path
        .canonicalize()
        .map_err(|err| format!("resolve path {}: {err}", path.display()))?;

    for allowed in allowed_roots {
        let Ok(canonical_allowed) = allowed.canonicalize() else {
            continue;
        };
        if path_within_root(&canonical, &canonical_allowed) {
            return Ok(canonical);
        }
    }

    Err(format!(
        "path {} is outside configured {VESC_PACKAGE_ROOTS_ENV}",
        path.display()
    ))
}

/// Validate any filesystem path (file or directory) lies under configured roots.
///
/// # Errors
///
/// Returns an error when no roots are configured, the path does not exist,
/// canonicalization fails, or the path lies outside all allowed roots.
pub fn validate_sandbox_file(path: &Path, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    if allowed_roots.is_empty() {
        return Err(format!(
            "path sandbox: set {VESC_PACKAGE_ROOTS_ENV} to allow package roots (comma-separated paths)"
        ));
    }

    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }

    let canonical = path
        .canonicalize()
        .map_err(|err| format!("resolve path {}: {err}", path.display()))?;

    for allowed in allowed_roots {
        let Ok(canonical_allowed) = allowed.canonicalize() else {
            continue;
        };
        if path_within_root(&canonical, &canonical_allowed) {
            return Ok(canonical);
        }
    }

    Err(format!(
        "path {} is outside configured {VESC_PACKAGE_ROOTS_ENV}",
        path.display()
    ))
}

/// True when `path` is equal to or nested under `root` (prefix-safe).
#[must_use]
pub fn path_within_root(path: &Path, root: &Path) -> bool {
    let mut root_components = root.components();
    for component in path.components() {
        match root_components.next() {
            Some(expected) if expected == component => {}
            Some(_) => return false,
            None => return true,
        }
    }
    root_components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempWorkspace, fixture_path};

    #[test]
    fn config_resolves_roots_from_env() {
        let merged = merge_config(
            &ConfigFile::default(),
            &EnvOverrides {
                package_roots: Some(split_path_list("/tmp/pkg-a,/tmp/pkg-b")),
                ..EnvOverrides::default()
            },
        );
        assert_eq!(
            merged.package_roots,
            vec![PathBuf::from("/tmp/pkg-a"), PathBuf::from("/tmp/pkg-b")]
        );
    }

    #[test]
    fn config_resolves_roots_from_toml() {
        let file: ConfigFile = toml::from_str(
            r#"
[paths]
package_roots = ["/data/refloat", "/data/poc"]
vesc_tool = "/usr/local/bin/vesc_tool"

[features]
enable_flash = true
"#,
        )
        .expect("parse example toml");

        let merged = merge_config(&file, &EnvOverrides::default());
        assert_eq!(
            merged.package_roots,
            vec![PathBuf::from("/data/refloat"), PathBuf::from("/data/poc")]
        );
        assert_eq!(
            merged.vesc_tool_path,
            PathBuf::from("/usr/local/bin/vesc_tool")
        );
        assert!(merged.enable_flash);
    }

    #[test]
    fn config_env_overrides_toml() {
        let file: ConfigFile = toml::from_str(
            r#"
[paths]
package_roots = ["/from/file"]
"#,
        )
        .expect("parse toml");

        let merged = merge_config(
            &file,
            &EnvOverrides {
                package_roots: Some(vec![PathBuf::from("/from/env")]),
                ..EnvOverrides::default()
            },
        );
        assert_eq!(merged.package_roots, vec![PathBuf::from("/from/env")]);
    }

    #[test]
    fn config_rejects_paths_outside_sandbox() {
        let allowed = vec![fixture_path("refloat-minimal")];
        let workspace = TempWorkspace::new();

        let err = validate_sandbox_path(&workspace.root, &allowed).expect_err("outside roots");
        assert!(err.contains("outside configured VESC_PACKAGE_ROOTS"));
    }

    #[test]
    fn config_accepts_path_within_sandbox() {
        let allowed = vec![fixture_path("")];
        let root = fixture_path("refloat-minimal");

        let canonical =
            validate_sandbox_path(&root, &allowed).expect("fixture root should be allowed");
        assert!(canonical.is_dir());
    }

    #[test]
    fn split_path_list_accepts_colon_separator() {
        assert_eq!(
            split_path_list("/tmp/a:/tmp/b"),
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        );
    }

    #[test]
    fn path_within_root_rejects_prefix_collision() {
        let root = PathBuf::from("/tmp/vesc");
        let sibling = PathBuf::from("/tmp/vesc-other");
        assert!(!path_within_root(&sibling, &root));
        assert!(path_within_root(&root.join("pkg"), &root));
    }

    #[test]
    fn enable_flash_defaults_off() {
        let merged = merge_config(&ConfigFile::default(), &EnvOverrides::default());
        assert!(!merged.enable_flash);
        assert_eq!(merged.knowledge.mode, RetrievalMode::Lexical);
        assert_eq!(merged.knowledge.max_limit, 50);
    }

    #[test]
    fn knowledge_config_reads_toml_and_env_precedence() {
        let file: ConfigFile = toml::from_str(
            r#"
[knowledge]
mode = "lexical"
artifact_path = "cache/knowledge.json"
max_limit = 20

[knowledge.semantic]
model_dir = "models/bge-small"
model_id = "bge-small-en-v1.5"
model_revision = "sha256:model"
max_length = 512
idle_timeout_secs = 60
"#,
        )
        .expect("parse knowledge config");
        let merged = merge_config(
            &file,
            &EnvOverrides {
                knowledge_mode: Some(RetrievalMode::Auto),
                ..EnvOverrides::default()
            },
        );
        assert_eq!(merged.knowledge.mode, RetrievalMode::Auto);
        assert_eq!(merged.knowledge.max_limit, 20);
        assert_eq!(
            merged.knowledge.artifact_path,
            Some(workspace::expand_path("cache/knowledge.json"))
        );
        assert_eq!(
            merged.knowledge.semantic_model_dir,
            Some(workspace::expand_path("models/bge-small"))
        );
        assert_eq!(
            merged.knowledge.semantic_model_id.as_deref(),
            Some("bge-small-en-v1.5")
        );
        assert_eq!(
            merged.knowledge.semantic_model_revision.as_deref(),
            Some("sha256:model")
        );
        assert_eq!(merged.knowledge.semantic_max_length, Some(512));
        assert_eq!(merged.knowledge.semantic_idle_timeout_secs, 60);
    }

    #[test]
    fn managed_repositories_parse_without_checkout_and_sort_deterministically() {
        let config = McpConfig::from_toml(
            r#"
[knowledge]
data_root = "/var/lib/vesc-mcp-empty"

[[knowledge.repositories]]
id = "vesc-tool"
remote_url = "https://github.com/vedderb/vesc_tool.git"
default_ref = "refs/heads/master"
policy = "required"
include = ["**/*.cpp"]
exclude = ["build/**"]
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824

[[knowledge.repositories]]
id = "refloat"
remote_url = "https://github.com/vedderb/vesc_pkg.git"
default_ref = "refs/heads/main"
policy = "optional"
include = ["**/*.lisp"]
exclude = []
trust_tier = "community"
license = "GPL-3.0-or-later"
attribution = "VESC contributors"
max_file_bytes = 524288
max_files = 10000
max_total_bytes = 268435456
"#,
            &DataRootInputs::default(),
        )
        .expect("managed repositories");

        assert_eq!(
            config
                .knowledge
                .repositories
                .iter()
                .map(|repository| repository.id().as_str())
                .collect::<Vec<_>>(),
            ["refloat", "vesc-tool"]
        );
        assert_eq!(
            config
                .knowledge
                .data_root
                .expect("resolved data root")
                .as_path(),
            Path::new("/var/lib/vesc-mcp-empty")
        );
    }

    #[test]
    fn managed_snapshot_prewarm_selections_are_typed_and_deterministic() {
        let config = McpConfig::from_toml(
            r#"
[knowledge]
data_root = "/var/lib/vesc-mcp"
prewarm = [
  { vesc = "refs/heads/release_6_06", refloat = "refs/tags/v1.2.3" },
]

[[knowledge.repositories]]
id = "vesc"
remote_url = "https://github.com/vedderb/bldc.git"
default_ref = "refs/heads/master"
policy = "required"
include = ["**/*.c"]
exclude = []
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824

[[knowledge.repositories]]
id = "refloat"
remote_url = "https://github.com/vedderb/vesc_pkg.git"
default_ref = "refs/heads/main"
policy = "optional"
include = ["**/*.lisp"]
exclude = []
trust_tier = "community"
license = "GPL-3.0-or-later"
attribution = "VESC contributors"
max_file_bytes = 524288
max_files = 10000
max_total_bytes = 268435456
"#,
            &DataRootInputs::default(),
        )
        .expect("managed snapshot configuration");

        assert_eq!(
            config.knowledge.prewarm[0]
                .iter()
                .map(|(id, selector)| (id.as_str(), selector.as_str()))
                .collect::<Vec<_>>(),
            [
                ("refloat", "refs/tags/v1.2.3"),
                ("vesc", "refs/heads/release_6_06"),
            ]
        );
    }

    #[test]
    fn managed_default_snapshot_resolves_as_the_active_artifact() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let config = McpConfig::from_toml(
            &format!(
                r#"
[knowledge]
data_root = "{}"

[[knowledge.repositories]]
id = "vesc"
remote_url = "https://github.com/vedderb/bldc.git"
default_ref = "refs/heads/master"
policy = "required"
include = ["**/*.c"]
exclude = []
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824
"#,
                temp.path().display()
            ),
            &DataRootInputs::default(),
        )
        .expect("managed configuration");
        let id = "a".repeat(64);
        std::fs::write(
            temp.path().join("default-snapshot.json"),
            serde_json::to_vec(&serde_json::json!({ "id": id })).expect("alias JSON"),
        )
        .expect("default alias");
        std::fs::create_dir_all(temp.path().join("snapshots")).expect("snapshot directory");
        std::fs::write(
            temp.path()
                .join("snapshots")
                .join(format!("{}.json", "a".repeat(64))),
            serde_json::to_vec(&serde_json::json!({
                "id": "a".repeat(64),
                "repositories": [{
                    "repository": "vesc",
                    "commit": "b".repeat(40),
                }],
            }))
            .expect("snapshot JSON"),
        )
        .expect("snapshot manifest");
        let artifact = temp.path().join("artifacts").join("a".repeat(64));
        std::fs::create_dir_all(&artifact).expect("artifact directory");

        assert_eq!(config.knowledge.resolved_artifact_path(), Some(artifact));
        let resolved = config
            .knowledge
            .resolved_artifact()
            .expect("resolved snapshot");
        assert_eq!(
            resolved.snapshot_id.expect("snapshot id").as_str(),
            "a".repeat(64)
        );
        assert_eq!(
            resolved
                .repositories
                .get(&RepositoryId::new("vesc").expect("repository id"))
                .expect("resolved repository"),
            &"b".repeat(40)
        );
    }

    #[test]
    fn duplicate_managed_repository_ids_return_typed_error() {
        let error = McpConfig::from_toml(
            r#"
[knowledge]
data_root = "/var/lib/vesc-mcp"

[[knowledge.repositories]]
id = "vesc"
remote_url = "https://example.com/one.git"
default_ref = "refs/heads/main"
policy = "optional"
trust_tier = "community"
license = "MIT"
attribution = "Example"
max_file_bytes = 1
max_files = 1
max_total_bytes = 1

[[knowledge.repositories]]
id = "vesc"
remote_url = "https://example.com/two.git"
default_ref = "refs/heads/main"
policy = "optional"
trust_tier = "community"
license = "MIT"
attribution = "Example"
max_file_bytes = 1
max_files = 1
max_total_bytes = 1
"#,
            &DataRootInputs::default(),
        )
        .expect_err("duplicate IDs");

        assert!(matches!(
            error,
            ConfigError::ManagedRepository(ManagedRepositoryError::DuplicateRepositoryId(_))
        ));
    }

    #[test]
    fn unsafe_managed_repository_url_returns_typed_error() {
        let error = McpConfig::from_toml(
            r#"
[knowledge]
data_root = "/var/lib/vesc-mcp"

[[knowledge.repositories]]
id = "vesc"
remote_url = "file:///private/source"
default_ref = "refs/heads/main"
policy = "optional"
trust_tier = "community"
license = "MIT"
attribution = "Example"
max_file_bytes = 1
max_files = 1
max_total_bytes = 1
"#,
            &DataRootInputs::default(),
        )
        .expect_err("unsafe URL");

        assert!(matches!(
            error,
            ConfigError::ManagedRepository(ManagedRepositoryError::InvalidRemoteUrl)
        ));
        assert!(!error.to_string().contains("/private/source"));
    }

    #[test]
    fn empty_repository_configuration_preserves_existing_retrieval_defaults() {
        let config = McpConfig::from_toml(
            "",
            &DataRootInputs {
                home: Some(PathBuf::from("/home/user")),
                ..DataRootInputs::default()
            },
        )
        .expect("default config");

        assert!(config.knowledge.repositories.is_empty());
        assert_eq!(config.knowledge.mode, RetrievalMode::Lexical);
        assert_eq!(config.knowledge.artifact_path, None);
    }

    #[test]
    fn jina_code_profile_selects_int8_queries_on_any_cpu() {
        let mut merged = merge_config(&ConfigFile::default(), &EnvOverrides::default());
        apply_jina_code_query_profile(
            &mut merged,
            vesc_knowledge_index::JinaCodeQueryProfile {
                query_model_dir: PathBuf::from("int8"),
                artifact_dir: PathBuf::from("artifact"),
            },
        );

        assert_eq!(merged.knowledge.mode, RetrievalMode::Auto);
        assert_eq!(
            merged.knowledge.artifact_path,
            Some(PathBuf::from("artifact"))
        );
        assert_eq!(
            merged.knowledge.semantic_model_dir,
            Some(PathBuf::from("int8"))
        );
        assert_eq!(merged.knowledge.semantic_max_length, Some(512));
    }

    #[test]
    fn feedback_config_defaults_off() {
        let merged = merge_config(&ConfigFile::default(), &EnvOverrides::default());
        assert_eq!(merged.feedback.path, None);
        assert!(!merged.feedback.writes_enabled);
    }

    #[test]
    fn feedback_config_reads_toml_and_env_precedence() {
        let file: ConfigFile = toml::from_str(
            r#"
[feedback]
path = "state/feedback"
writes_enabled = false
"#,
        )
        .expect("parse feedback config");
        let merged = merge_config(
            &file,
            &EnvOverrides {
                feedback_path: Some(PathBuf::from("/tmp/feedback")),
                feedback_writes_enabled: Some(true),
                ..EnvOverrides::default()
            },
        );
        assert_eq!(merged.feedback.path, Some(PathBuf::from("/tmp/feedback")));
        assert!(merged.feedback.writes_enabled);
    }

    #[test]
    fn knowledge_mode_rejects_unknown_values() {
        let error = RetrievalMode::from_str("not-a-mode").expect_err("invalid mode");
        assert!(error.contains("unsupported retrieval mode"));
    }

    #[test]
    fn config_file_path_uses_env_override() {
        let custom = PathBuf::from("/tmp/vesc-mcp/custom-config.toml");
        assert_eq!(
            resolve_config_file_path(Some("/tmp/vesc-mcp/custom-config.toml")),
            custom
        );
    }

    #[test]
    fn config_file_path_falls_back_to_default() {
        assert_eq!(resolve_config_file_path(None), default_config_path());
    }

    #[test]
    fn config_reads_custom_file_path() {
        let workspace = TempWorkspace::new();
        let config_path = workspace.root.join("custom.toml");
        std::fs::write(
            &config_path,
            r#"
[paths]
package_roots = ["/custom/from/file"]
"#,
        )
        .expect("write custom config");

        let file = read_config_file(&config_path).expect("read custom config");
        let merged = merge_config(&file, &EnvOverrides::default());
        assert_eq!(
            merged.package_roots,
            vec![PathBuf::from("/custom/from/file")]
        );
    }
}
