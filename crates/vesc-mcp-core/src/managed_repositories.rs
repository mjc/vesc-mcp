//! Validated repository registry and portable on-disk knowledge layout.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

/// Validation failures for managed repository configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedRepositoryError {
    #[error("invalid repository id")]
    InvalidRepositoryId,
    #[error("invalid snapshot id")]
    InvalidSnapshotId,
    #[error("invalid remote_url: only credential-free https URLs are allowed")]
    InvalidRemoteUrl,
    #[error("invalid default_ref")]
    InvalidDefaultRef,
    #[error("invalid {field} path pattern")]
    InvalidPattern { field: &'static str },
    #[error("invalid repository metadata field {field}")]
    InvalidMetadata { field: &'static str },
    #[error("invalid repository source limits")]
    InvalidSourceLimits,
    #[error("duplicate repository id {0}")]
    DuplicateRepositoryId(RepositoryId),
    #[error("invalid application data root")]
    InvalidDataRoot,
    #[error("no platform application-data directory is available")]
    DataRootUnavailable,
}

/// Stable lowercase identifier used in configuration and path derivation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RepositoryId(String);

impl RepositoryId {
    /// Validate a repository identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedRepositoryError::InvalidRepositoryId`] unless the ID is
    /// 1–64 lowercase ASCII letters, digits, dashes, or underscores and starts
    /// with an alphanumeric character.
    pub fn new(value: impl Into<String>) -> Result<Self, ManagedRepositoryError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let valid = (1..=64).contains(&value.len())
            && bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            && bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            });
        valid
            .then_some(Self(value))
            .ok_or(ManagedRepositoryError::InvalidRepositoryId)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RepositoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RepositoryId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RepositoryId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Immutable knowledge snapshot identifier used only after validation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct KnowledgeSnapshotId(String);

/// Compatibility name retained for callers of the original data-layout API.
pub type SnapshotId = KnowledgeSnapshotId;

impl KnowledgeSnapshotId {
    /// Validate an immutable snapshot identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedRepositoryError::InvalidSnapshotId`] for values unsafe
    /// to use as one path component.
    pub fn new(value: impl Into<String>) -> Result<Self, ManagedRepositoryError> {
        let value = value.into();
        let valid = (1..=128).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        valid
            .then_some(Self(value))
            .ok_or(ManagedRepositoryError::InvalidSnapshotId)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a configured source is ignored, best-effort, or mandatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryPolicy {
    Disabled,
    Optional,
    Required,
}

/// Declared authority of repository content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    Official,
    Community,
    Untrusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RepositoryWire {
    id: String,
    remote_url: String,
    default_ref: String,
    policy: RepositoryPolicy,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    trust_tier: TrustTier,
    license: String,
    attribution: String,
    max_file_bytes: u64,
    max_files: usize,
    max_total_bytes: u64,
}

/// One approved and resource-bounded source repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnowledgeRepository {
    id: RepositoryId,
    remote_url: String,
    default_ref: String,
    policy: RepositoryPolicy,
    include: Vec<String>,
    exclude: Vec<String>,
    trust_tier: TrustTier,
    license: String,
    attribution: String,
    max_file_bytes: u64,
    max_files: usize,
    max_total_bytes: u64,
}

impl TryFrom<RepositoryWire> for KnowledgeRepository {
    type Error = ManagedRepositoryError;

    fn try_from(repository: RepositoryWire) -> Result<Self, Self::Error> {
        let id = RepositoryId::new(repository.id)?;
        validate_remote_url(&repository.remote_url)?;
        validate_default_ref(&repository.default_ref)?;
        validate_patterns("include", &repository.include)?;
        validate_patterns("exclude", &repository.exclude)?;
        validate_metadata("license", &repository.license)?;
        validate_metadata("attribution", &repository.attribution)?;
        if repository.max_file_bytes == 0
            || repository.max_files == 0
            || repository.max_total_bytes == 0
            || repository.max_file_bytes > repository.max_total_bytes
        {
            return Err(ManagedRepositoryError::InvalidSourceLimits);
        }
        Ok(Self {
            id,
            remote_url: repository.remote_url,
            default_ref: repository.default_ref,
            policy: repository.policy,
            include: repository.include,
            exclude: repository.exclude,
            trust_tier: repository.trust_tier,
            license: repository.license,
            attribution: repository.attribution,
            max_file_bytes: repository.max_file_bytes,
            max_files: repository.max_files,
            max_total_bytes: repository.max_total_bytes,
        })
    }
}

impl<'de> Deserialize<'de> for KnowledgeRepository {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        RepositoryWire::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

impl KnowledgeRepository {
    #[must_use]
    pub const fn id(&self) -> &RepositoryId {
        &self.id
    }

    #[must_use]
    pub fn remote_url(&self) -> &str {
        &self.remote_url
    }

    #[must_use]
    pub fn default_ref(&self) -> &str {
        &self.default_ref
    }

    #[must_use]
    pub const fn policy(&self) -> RepositoryPolicy {
        self.policy
    }

    #[must_use]
    pub fn include(&self) -> &[String] {
        &self.include
    }

    #[must_use]
    pub fn exclude(&self) -> &[String] {
        &self.exclude
    }

    #[must_use]
    pub const fn trust_tier(&self) -> TrustTier {
        self.trust_tier
    }

    #[must_use]
    pub fn license(&self) -> &str {
        &self.license
    }

    #[must_use]
    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    #[must_use]
    pub const fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
    }

    #[must_use]
    pub const fn max_files(&self) -> usize {
        self.max_files
    }

    #[must_use]
    pub const fn max_total_bytes(&self) -> u64 {
        self.max_total_bytes
    }
}

#[derive(Serialize, Deserialize)]
struct RegistryWire {
    #[serde(default)]
    repositories: Vec<KnowledgeRepository>,
}

/// Deterministically ordered repository collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RepositoryRegistry {
    repositories: Vec<KnowledgeRepository>,
}

impl RepositoryRegistry {
    /// Validate uniqueness and sort by stable repository ID.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedRepositoryError::DuplicateRepositoryId`] for duplicates.
    pub fn new(mut repositories: Vec<KnowledgeRepository>) -> Result<Self, ManagedRepositoryError> {
        repositories.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(pair) = repositories
            .windows(2)
            .find(|pair| pair[0].id == pair[1].id)
        {
            return Err(ManagedRepositoryError::DuplicateRepositoryId(
                pair[0].id.clone(),
            ));
        }
        Ok(Self { repositories })
    }

    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &KnowledgeRepository> {
        self.repositories.iter()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repositories.is_empty()
    }
}

pub(crate) fn validate_repository_config(
    repositories: Vec<RepositoryWire>,
) -> Result<RepositoryRegistry, ManagedRepositoryError> {
    RepositoryRegistry::new(
        repositories
            .into_iter()
            .map(KnowledgeRepository::try_from)
            .collect::<Result<Vec<_>, _>>()?,
    )
}

impl<'de> Deserialize<'de> for RepositoryRegistry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(RegistryWire::deserialize(deserializer)?.repositories).map_err(D::Error::custom)
    }
}

fn validate_remote_url(value: &str) -> Result<(), ManagedRepositoryError> {
    let authority = value
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next());
    let valid = authority.is_some_and(|authority| {
        !authority.is_empty()
            && !authority.contains('@')
            && !value.contains(['?', '#'])
            && !value.chars().any(char::is_whitespace)
    });
    valid
        .then_some(())
        .ok_or(ManagedRepositoryError::InvalidRemoteUrl)
}

fn validate_default_ref(value: &str) -> Result<(), ManagedRepositoryError> {
    let valid = value.starts_with("refs/")
        && !value.contains("..")
        && !value.contains("@{")
        && !value.ends_with('/')
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace());
    valid
        .then_some(())
        .ok_or(ManagedRepositoryError::InvalidDefaultRef)
}

fn validate_patterns(
    field: &'static str,
    patterns: &[String],
) -> Result<(), ManagedRepositoryError> {
    patterns.iter().try_for_each(|pattern| {
        let path = Path::new(pattern);
        let valid = !pattern.is_empty()
            && !pattern.contains(['\\', ':', '\0'])
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
        valid
            .then_some(())
            .ok_or(ManagedRepositoryError::InvalidPattern { field })
    })
}

fn validate_metadata(field: &'static str, value: &str) -> Result<(), ManagedRepositoryError> {
    (!value.trim().is_empty() && !value.chars().any(char::is_control))
        .then_some(())
        .ok_or(ManagedRepositoryError::InvalidMetadata { field })
}

/// Environment-derived candidates for the application data directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataRootInputs {
    pub state_directory: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl DataRootInputs {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            state_directory: std::env::var_os("STATE_DIRECTORY").map(PathBuf::from),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            home: std::env::var_os("HOME").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}

/// Validated absolute root owned by the application.
#[derive(Clone, PartialEq, Eq)]
pub struct DataRoot(PathBuf);

impl DataRoot {
    /// Validate an absolute application data root without touching disk.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedRepositoryError::InvalidDataRoot`] for relative or
    /// lexically escaping paths.
    pub fn new(path: PathBuf) -> Result<Self, ManagedRepositoryError> {
        let valid = path.is_absolute()
            && path
                .components()
                .all(|component| component != Component::ParentDir);
        valid
            .then_some(Self(path))
            .ok_or(ManagedRepositoryError::InvalidDataRoot)
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Debug for DataRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DataRoot(<redacted>)")
    }
}

/// Resolve configured, systemd, and per-user data roots in priority order.
///
/// # Errors
///
/// Returns a typed error when the selected root is invalid or no platform
/// application-data location can be determined.
pub fn resolve_data_root(
    configured: Option<&Path>,
    inputs: &DataRootInputs,
) -> Result<DataRoot, ManagedRepositoryError> {
    let candidate = configured
        .map(Path::to_path_buf)
        .or_else(|| inputs.state_directory.clone())
        .or_else(|| {
            inputs
                .xdg_data_home
                .as_ref()
                .map(|path| path.join("vesc-mcp"))
        })
        .or_else(|| platform_user_data_root(inputs))
        .ok_or(ManagedRepositoryError::DataRootUnavailable)?;
    DataRoot::new(candidate)
}

#[cfg(target_os = "macos")]
fn platform_user_data_root(inputs: &DataRootInputs) -> Option<PathBuf> {
    inputs
        .home
        .as_ref()
        .map(|home| home.join("Library/Application Support/vesc-mcp"))
}

#[cfg(target_os = "windows")]
fn platform_user_data_root(inputs: &DataRootInputs) -> Option<PathBuf> {
    inputs
        .local_app_data
        .as_ref()
        .map(|root| root.join("vesc-mcp"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_user_data_root(inputs: &DataRootInputs) -> Option<PathBuf> {
    inputs
        .home
        .as_ref()
        .map(|home| home.join(".local/share/vesc-mcp"))
}

/// Portable paths derived from a validated application data root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeDataLayout {
    root: DataRoot,
}

impl KnowledgeDataLayout {
    #[must_use]
    pub const fn new(root: DataRoot) -> Self {
        Self { root }
    }

    #[must_use]
    pub const fn root(&self) -> &DataRoot {
        &self.root
    }

    #[must_use]
    pub fn repository(&self, id: &RepositoryId) -> PathBuf {
        self.root
            .as_path()
            .join("repositories")
            .join(format!("{}.git", id.as_str()))
    }

    #[must_use]
    pub fn snapshot(&self, id: &SnapshotId) -> PathBuf {
        self.root
            .as_path()
            .join("snapshots")
            .join(format!("{}.json", id.as_str()))
    }

    #[must_use]
    pub fn artifact(&self, id: &SnapshotId) -> PathBuf {
        self.root.as_path().join("artifacts").join(id.as_str())
    }

    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.root.as_path().join("tmp")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const REPOSITORIES: &str = r#"
[[repositories]]
id = "vesc-tool"
remote_url = "https://github.com/vedderb/vesc_tool.git"
default_ref = "refs/heads/master"
policy = "required"
include = ["*.pro", "commands/**"]
exclude = ["build/**"]
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824

[[repositories]]
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
"#;

    #[test]
    fn data_root_precedence_is_explicit_then_systemd_then_xdg() {
        let inputs = DataRootInputs {
            state_directory: Some(PathBuf::from("/state/systemd")),
            xdg_data_home: Some(PathBuf::from("/home/user/.local/share")),
            home: Some(PathBuf::from("/home/user")),
            local_app_data: None,
        };

        assert_eq!(
            resolve_data_root(Some(Path::new("/state/explicit")), &inputs)
                .expect("explicit root")
                .as_path(),
            Path::new("/state/explicit")
        );
        assert_eq!(
            resolve_data_root(None, &inputs)
                .expect("systemd root")
                .as_path(),
            Path::new("/state/systemd")
        );
        assert_eq!(
            resolve_data_root(
                None,
                &DataRootInputs {
                    state_directory: None,
                    ..inputs
                },
            )
            .expect("XDG root")
            .as_path(),
            Path::new("/home/user/.local/share/vesc-mcp")
        );
    }

    #[test]
    fn relative_data_roots_are_rejected_without_disclosing_the_path() {
        let error = resolve_data_root(
            Some(Path::new("private/user/path")),
            &DataRootInputs::default(),
        )
        .expect_err("relative root");

        assert!(matches!(error, ManagedRepositoryError::InvalidDataRoot));
        assert!(!error.to_string().contains("private/user/path"));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn linux_user_data_root_falls_back_to_home() {
        let root = resolve_data_root(
            None,
            &DataRootInputs {
                home: Some(PathBuf::from("/home/user")),
                ..DataRootInputs::default()
            },
        )
        .expect("Linux user data root");

        assert_eq!(
            root.as_path(),
            Path::new("/home/user/.local/share/vesc-mcp")
        );
    }

    #[test]
    fn selected_relative_xdg_root_is_rejected() {
        let error = resolve_data_root(
            None,
            &DataRootInputs {
                xdg_data_home: Some(PathBuf::from("relative/private")),
                home: Some(PathBuf::from("/home/user")),
                ..DataRootInputs::default()
            },
        )
        .expect_err("relative XDG root");

        assert_eq!(error, ManagedRepositoryError::InvalidDataRoot);
    }

    #[test]
    fn missing_platform_data_root_is_typed() {
        assert_eq!(
            resolve_data_root(None, &DataRootInputs::default()).expect_err("missing data root"),
            ManagedRepositoryError::DataRootUnavailable
        );
    }

    #[test]
    fn repository_toml_round_trip_is_sorted_by_id() {
        let registry: RepositoryRegistry = toml::from_str(REPOSITORIES).expect("valid registry");
        let encoded = toml::to_string(&registry).expect("encode registry");
        let reparsed: RepositoryRegistry = toml::from_str(&encoded).expect("round trip");

        assert_eq!(registry, reparsed);
        assert_eq!(
            registry
                .iter()
                .map(|repository| repository.id().as_str())
                .collect::<Vec<_>>(),
            ["refloat", "vesc-tool"]
        );
    }

    #[test]
    fn invalid_repository_inputs_return_typed_errors() {
        for (field, value) in [
            ("id", "../vesc"),
            ("remote_url", "file:///private/repository"),
            ("include", "../private/**"),
        ] {
            let invalid = REPOSITORIES.replacen(
                match field {
                    "id" => "id = \"vesc-tool\"",
                    "remote_url" => "remote_url = \"https://github.com/vedderb/vesc_tool.git\"",
                    _ => "include = [\"*.pro\", \"commands/**\"]",
                },
                &match field {
                    "include" => format!("include = [\"{value}\"]"),
                    _ => format!("{field} = \"{value}\""),
                },
                1,
            );
            let error = toml::from_str::<RepositoryRegistry>(&invalid).expect_err(field);
            assert!(error.to_string().contains(field));
        }
    }

    #[test]
    fn duplicate_repository_ids_are_rejected() {
        let duplicate = REPOSITORIES.replace("id = \"refloat\"", "id = \"vesc-tool\"");
        let error = toml::from_str::<RepositoryRegistry>(&duplicate).expect_err("duplicate id");
        assert!(error.to_string().contains("duplicate repository id"));
    }

    #[test]
    fn layout_derives_only_paths_below_the_data_root() {
        let root = DataRoot::new(PathBuf::from("/var/lib/vesc-mcp")).expect("absolute root");
        let layout = KnowledgeDataLayout::new(root);
        let repository = RepositoryId::new("vesc-tool").expect("repository id");
        let snapshot = SnapshotId::new("sha256-0123456789abcdef").expect("snapshot id");

        assert_eq!(
            layout.repository(&repository),
            Path::new("/var/lib/vesc-mcp/repositories/vesc-tool.git")
        );
        assert_eq!(
            layout.snapshot(&snapshot),
            Path::new("/var/lib/vesc-mcp/snapshots/sha256-0123456789abcdef.json")
        );
        assert_eq!(
            layout.artifact(&snapshot),
            Path::new("/var/lib/vesc-mcp/artifacts/sha256-0123456789abcdef")
        );
        assert_eq!(layout.staging(), Path::new("/var/lib/vesc-mcp/tmp"));
        assert!(SnapshotId::new("../escape").is_err());
    }
}
