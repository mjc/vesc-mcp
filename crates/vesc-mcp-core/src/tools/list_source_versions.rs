//! Read-only discovery of configured repositories and locally cached refs.

use crate::config::KnowledgeConfig;
use crate::managed_git::{ManagedGitStore, ManagedRef, ManagedRefKind, RefreshStatus};
use crate::managed_repositories::{
    KnowledgeDataLayout, KnowledgeRepository, RepositoryId, RepositoryPolicy,
};
use crate::managed_snapshots::{KnowledgeSnapshotManifest, SnapshotProfile};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

/// Version kinds accepted by the discovery filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceRefKind {
    Default,
    Branch,
    Tag,
}

/// Bounded filters for locally cached source versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListVescSourceVersionsParams {
    /// Configured repository IDs; empty selects every configured repository.
    #[serde(default)]
    pub repository_ids: Vec<String>,
    /// Ref kinds to retain; empty selects defaults, branches, and tags.
    #[serde(default)]
    pub ref_kinds: Vec<SourceRefKind>,
    /// Optional prefix matched against a display name or full ref.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Maximum version rows across all returned repositories.
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Opaque continuation cursor returned by the previous call.
    #[serde(default)]
    pub cursor: Option<String>,
}

const fn default_limit() -> usize {
    DEFAULT_LIMIT
}

/// Availability of a repository's persisted catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceSyncState {
    Fresh,
    Stale,
    Unavailable,
}

/// One immutable source version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceVersion {
    pub full_ref: String,
    pub display_name: String,
    pub kind: SourceRefKind,
    pub commit: String,
    pub ready: bool,
    pub default_snapshot: bool,
    pub prewarmed: bool,
}

/// One configured repository and its bounded page of versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceRepositoryVersions {
    pub id: String,
    pub remote: String,
    pub default_ref: String,
    pub sync_state: SourceSyncState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    pub versions: Vec<SourceVersion>,
}

/// Stable public error shape for discovery failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceVersionsError {
    pub code: String,
    pub message: String,
    pub hint: String,
}

/// Bounded repository/version discovery response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListVescSourceVersionsResponse {
    pub ok: bool,
    pub repositories: Vec<SourceRepositoryVersions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SourceVersionsError>,
}

#[derive(Debug, Clone, Copy, Default)]
struct SnapshotMembership {
    ready: bool,
    default_snapshot: bool,
    prewarmed: bool,
}

type SnapshotMemberships = BTreeMap<RepositoryId, BTreeMap<String, SnapshotMembership>>;

fn record_snapshot(
    memberships: &mut SnapshotMemberships,
    manifest: &KnowledgeSnapshotManifest,
    default_id: Option<&str>,
) {
    for selected in &manifest.repositories {
        let membership = memberships
            .entry(selected.repository.clone())
            .or_default()
            .entry(selected.commit.clone())
            .or_default();
        membership.ready = true;
        membership.default_snapshot |= default_id == Some(manifest.id.as_str());
        membership.prewarmed |= manifest.profile == SnapshotProfile::SelectedTrees
            && default_id != Some(manifest.id.as_str());
    }
}

fn snapshot_memberships(root: &Path) -> SnapshotMemberships {
    let default = fs::read(root.join("default-snapshot.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<KnowledgeSnapshotManifest>(&bytes).ok());
    let mut memberships = SnapshotMemberships::new();
    if let Some(manifest) = &default {
        record_snapshot(&mut memberships, manifest, Some(manifest.id.as_str()));
    }
    let Ok(entries) = fs::read_dir(root.join("snapshots")) else {
        return memberships;
    };
    for manifest in entries
        .flatten()
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<KnowledgeSnapshotManifest>(&bytes).ok())
    {
        record_snapshot(
            &mut memberships,
            &manifest,
            default.as_ref().map(|default| default.id.as_str()),
        );
    }
    memberships
}

fn failure(code: &str, message: &str, hint: &str) -> ListVescSourceVersionsResponse {
    ListVescSourceVersionsResponse {
        ok: false,
        repositories: Vec::new(),
        next_cursor: None,
        error: Some(SourceVersionsError {
            code: code.to_owned(),
            message: message.to_owned(),
            hint: hint.to_owned(),
        }),
    }
}

fn remote_identity(remote: &str) -> String {
    remote
        .strip_prefix("https://")
        .unwrap_or(remote)
        .strip_suffix(".git")
        .unwrap_or_else(|| remote.strip_prefix("https://").unwrap_or(remote))
        .to_owned()
}

fn requested_kind(entry: &ManagedRef, requested: &[SourceRefKind]) -> bool {
    requested.is_empty()
        || requested.iter().any(|kind| match kind {
            SourceRefKind::Default => entry.is_default,
            SourceRefKind::Branch => entry.kind == ManagedRefKind::Branch,
            SourceRefKind::Tag => entry.kind == ManagedRefKind::Tag,
        })
}

const fn public_kind(entry: &ManagedRef) -> SourceRefKind {
    if entry.is_default {
        SourceRefKind::Default
    } else {
        match entry.kind {
            ManagedRefKind::Branch => SourceRefKind::Branch,
            ManagedRefKind::Tag => SourceRefKind::Tag,
        }
    }
}

fn repository_versions(
    store: &ManagedGitStore,
    memberships: &SnapshotMemberships,
    params: &ListVescSourceVersionsParams,
    repository: &KnowledgeRepository,
) -> SourceRepositoryVersions {
    let Ok(mut catalog) = store.catalog(repository.id()) else {
        return SourceRepositoryVersions {
            id: repository.id().as_str().to_owned(),
            remote: remote_identity(repository.remote_url()),
            default_ref: repository.default_ref().to_owned(),
            sync_state: SourceSyncState::Unavailable,
            diagnostic: Some(
                "no readable cached ref catalog; an operator refresh is required".to_owned(),
            ),
            versions: Vec::new(),
        };
    };
    order_versions(&mut catalog.refs);
    let stale = catalog
        .refs
        .iter()
        .any(|entry| entry.refresh_status == RefreshStatus::Stale);
    let versions = catalog
        .refs
        .into_iter()
        .filter(|entry| requested_kind(entry, &params.ref_kinds))
        .filter(|entry| {
            params.prefix.as_ref().is_none_or(|prefix| {
                entry.display_version.starts_with(prefix) || entry.full_name.starts_with(prefix)
            })
        })
        .map(|entry| {
            let membership = memberships
                .get(repository.id())
                .and_then(|versions| versions.get(&entry.commit))
                .copied()
                .unwrap_or_default();
            SourceVersion {
                full_ref: entry.full_name.clone(),
                display_name: entry.display_version.clone(),
                kind: public_kind(&entry),
                commit: entry.commit,
                ready: membership.ready,
                default_snapshot: membership.default_snapshot,
                prewarmed: membership.prewarmed,
            }
        })
        .collect();
    SourceRepositoryVersions {
        id: repository.id().as_str().to_owned(),
        remote: remote_identity(repository.remote_url()),
        default_ref: repository.default_ref().to_owned(),
        sync_state: if stale {
            SourceSyncState::Stale
        } else {
            SourceSyncState::Fresh
        },
        diagnostic: stale
            .then(|| "cached ref catalog is stale; an operator refresh is recommended".to_owned()),
        versions,
    }
}

fn paginate_versions(
    repositories: &mut [SourceRepositoryVersions],
    offset: usize,
    requested_limit: usize,
) -> Option<String> {
    let total = repositories
        .iter()
        .map(|repository| repository.versions.len())
        .sum::<usize>();
    let limit = if requested_limit == 0 {
        DEFAULT_LIMIT
    } else {
        requested_limit.min(MAX_LIMIT)
    };
    let end = offset.saturating_add(limit);
    let mut position = 0;
    for repository in repositories {
        repository.versions.retain(|_| {
            let keep = (offset..end).contains(&position);
            position += 1;
            keep
        });
    }
    (end < total).then(|| format!("v1:{end}"))
}

/// List configured repositories and cached refs without network I/O.
#[must_use]
pub fn list_vesc_source_versions_tool(
    params: &ListVescSourceVersionsParams,
    config: &KnowledgeConfig,
) -> ListVescSourceVersionsResponse {
    let Some(data_root) = config.data_root.clone() else {
        return failure(
            "source_catalog_not_configured",
            "managed source catalogs are not configured",
            "configure knowledge.data_root and at least one repository",
        );
    };
    let offset = match params.cursor.as_deref() {
        None => 0,
        Some(cursor) => match cursor
            .strip_prefix("v1:")
            .and_then(|value| value.parse::<usize>().ok())
        {
            Some(offset) => offset,
            None => {
                return failure(
                    "invalid_cursor",
                    "the source version cursor is invalid",
                    "retry without a cursor",
                );
            }
        },
    };
    let unknown = params.repository_ids.iter().find(|requested| {
        !config
            .repositories
            .iter()
            .any(|repository| repository.id().as_str() == requested.as_str())
    });
    if let Some(unknown) = unknown {
        return failure(
            "unknown_repository",
            &format!("repository {unknown} is not configured"),
            "call without repository_ids to list configured repositories",
        );
    }

    let layout = KnowledgeDataLayout::new(data_root);
    let memberships = snapshot_memberships(layout.root().as_path());
    let store = ManagedGitStore::new(layout);
    let mut repositories = config
        .repositories
        .iter()
        .filter(|repository| repository.policy() != RepositoryPolicy::Disabled)
        .filter(|repository| {
            params.repository_ids.is_empty()
                || params
                    .repository_ids
                    .iter()
                    .any(|requested| requested == repository.id().as_str())
        })
        .map(|repository| repository_versions(&store, &memberships, params, repository))
        .collect::<Vec<_>>();
    let next_cursor = paginate_versions(&mut repositories, offset, params.limit);

    ListVescSourceVersionsResponse {
        ok: true,
        repositories,
        next_cursor,
        error: None,
    }
}

/// Serialize locally cached source discovery for an MCP handler.
#[must_use]
pub fn list_vesc_source_versions_json(
    params: &ListVescSourceVersionsParams,
    config: &KnowledgeConfig,
) -> String {
    serde_json::to_string(&list_vesc_source_versions_tool(params, config)).unwrap_or_else(|_| {
        String::from(
            r#"{"ok":false,"repositories":[],"error":{"code":"serialization_failed","message":"source version response serialization failed","hint":"retry the request"}}"#,
        )
    })
}

fn release_numbers(name: &str) -> Option<Vec<u64>> {
    let candidate = name
        .strip_prefix("release_")
        .or_else(|| name.strip_prefix('v'))
        .unwrap_or(name);
    if !candidate.starts_with(|character: char| character.is_ascii_digit()) {
        return None;
    }
    candidate
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|parts| !parts.is_empty())
}

fn order_versions(versions: &mut [ManagedRef]) {
    versions.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| {
                match (
                    release_numbers(&left.display_version),
                    release_numbers(&right.display_version),
                ) {
                    (Some(left), Some(right)) => left.cmp(&right),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            })
            .then_with(|| left.full_name.cmp(&right.full_name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpConfig;
    use crate::managed_git::{ManagedRefKind, RefCatalog, RefreshStatus};
    use crate::managed_repositories::DataRootInputs;
    use crate::managed_snapshots::{KnowledgeSnapshotManifest, SnapshotRepository};
    use std::fs;
    use std::path::Path;

    fn version(name: &str, kind: ManagedRefKind, is_default: bool) -> ManagedRef {
        let prefix = match kind {
            ManagedRefKind::Branch => "refs/remotes/origin/",
            ManagedRefKind::Tag => "refs/tags/",
        };
        ManagedRef {
            full_name: format!("{prefix}{name}"),
            display_version: name.to_owned(),
            kind,
            commit: "a".repeat(40),
            is_default,
            refresh_status: RefreshStatus::Fresh,
        }
    }

    fn config(root: &Path) -> KnowledgeConfig {
        McpConfig::from_toml(
            &format!(
                r#"
[knowledge]
data_root = "{}"

[[knowledge.repositories]]
id = "bldc"
remote_url = "https://github.com/vedderb/bldc.git"
default_ref = "refs/heads/main"
policy = "required"
include = ["**/*.c"]
exclude = []
trust_tier = "official"
license = "GPL-3.0-only"
attribution = "VESC"
max_file_bytes = 1000
max_files = 10
max_total_bytes = 10000

[[knowledge.repositories]]
id = "vesc-tool"
remote_url = "https://github.com/vedderb/vesc_tool.git"
default_ref = "refs/heads/release_6_06"
policy = "required"
include = ["**/*.cpp"]
exclude = []
trust_tier = "official"
license = "GPL-3.0-only"
attribution = "VESC Tool"
max_file_bytes = 1000
max_files = 10
max_total_bytes = 10000

[[knowledge.repositories]]
id = "refloat"
remote_url = "https://github.com/lukash/refloat.git"
default_ref = "refs/heads/main"
policy = "required"
include = ["**/*.lisp"]
exclude = []
trust_tier = "community"
license = "GPL-3.0-only"
attribution = "Refloat"
max_file_bytes = 1000
max_files = 10
max_total_bytes = 10000
"#,
                root.display()
            ),
            &DataRootInputs::default(),
        )
        .expect("valid fixture config")
        .knowledge
    }

    fn write_catalog(root: &Path, id: &str, refs: Vec<ManagedRef>) {
        let repositories = root.join("repositories");
        fs::create_dir_all(&repositories).expect("catalog directory");
        let catalog = RefCatalog {
            repository: crate::managed_repositories::RepositoryId::new(id).expect("repository id"),
            refs,
        };
        fs::write(
            repositories.join(format!("{id}.refs.json")),
            serde_json::to_vec(&catalog).expect("serialize catalog"),
        )
        .expect("write catalog");
    }

    fn write_snapshot(root: &Path, repository: &str, commit: &str, is_default: bool) {
        let manifest = KnowledgeSnapshotManifest::new(
            vec![SnapshotRepository {
                repository: crate::managed_repositories::RepositoryId::new(repository)
                    .expect("repository id"),
                commit: commit.to_owned(),
                policy_digest: String::from("fixture-policy"),
            }],
            None,
        )
        .expect("snapshot manifest");
        let snapshots = root.join("snapshots");
        fs::create_dir_all(&snapshots).expect("snapshot directory");
        fs::write(
            snapshots.join(format!("{}.json", manifest.id.as_str())),
            serde_json::to_vec(&manifest).expect("serialize snapshot"),
        )
        .expect("write snapshot");
        if is_default {
            fs::write(
                root.join("default-snapshot.json"),
                serde_json::to_vec(&manifest).expect("serialize default snapshot"),
            )
            .expect("write default snapshot");
        }
    }

    #[test]
    fn versions_sort_default_then_natural_release_then_lexical() {
        let mut versions = vec![
            version("z-development", ManagedRefKind::Branch, false),
            version("release_6_9", ManagedRefKind::Branch, false),
            version("v6.10", ManagedRefKind::Tag, false),
            version("release_6_10", ManagedRefKind::Branch, false),
            version("main", ManagedRefKind::Branch, true),
            version("v6.9", ManagedRefKind::Tag, false),
        ];

        order_versions(&mut versions);

        assert_eq!(
            versions
                .iter()
                .map(|version| version.display_version.as_str())
                .collect::<Vec<_>>(),
            [
                "main",
                "release_6_9",
                "v6.9",
                "release_6_10",
                "v6.10",
                "z-development",
            ]
        );
    }

    #[test]
    fn configured_repositories_include_branch_only_catalogs() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let config = config(temp.path());
        write_catalog(
            temp.path(),
            "bldc",
            vec![
                version("main", ManagedRefKind::Branch, true),
                version("v6.06", ManagedRefKind::Tag, false),
            ],
        );
        write_catalog(
            temp.path(),
            "vesc-tool",
            vec![version("release_6_06", ManagedRefKind::Branch, true)],
        );
        write_catalog(
            temp.path(),
            "refloat",
            vec![
                version("main", ManagedRefKind::Branch, true),
                version("v1.2.3", ManagedRefKind::Tag, false),
            ],
        );

        let response = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: Vec::new(),
                ref_kinds: Vec::new(),
                prefix: None,
                limit: 20,
                cursor: None,
            },
            &config,
        );

        assert!(response.ok);
        assert_eq!(response.repositories.len(), 3);
        assert_eq!(response.repositories[0].remote, "github.com/vedderb/bldc");
        assert_eq!(response.repositories[2].versions.len(), 1);
        assert_eq!(
            response.repositories[2].versions[0].kind,
            SourceRefKind::Default
        );
        assert!(
            !serde_json::to_string(&response)
                .expect("serialize response")
                .contains(&temp.path().display().to_string())
        );
    }

    #[test]
    fn versions_report_default_and_prewarmed_snapshot_membership() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let config = config(temp.path());
        let default_commit = "a".repeat(40);
        let prewarmed_commit = "b".repeat(40);
        let mut main = version("main", ManagedRefKind::Branch, true);
        main.commit.clone_from(&default_commit);
        let mut tag = version("v6.06", ManagedRefKind::Tag, false);
        tag.commit.clone_from(&prewarmed_commit);
        write_catalog(temp.path(), "bldc", vec![main, tag]);
        write_snapshot(temp.path(), "bldc", &default_commit, true);
        write_snapshot(temp.path(), "bldc", &prewarmed_commit, false);

        let response = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: vec![String::from("bldc")],
                ref_kinds: Vec::new(),
                prefix: None,
                limit: 20,
                cursor: None,
            },
            &config,
        );

        assert!(response.repositories[0].versions[0].default_snapshot);
        assert!(response.repositories[0].versions[0].ready);
        assert!(response.repositories[0].versions[1].prewarmed);
        assert!(response.repositories[0].versions[1].ready);
    }

    #[test]
    fn filters_and_cursor_page_versions_deterministically() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let config = config(temp.path());
        write_catalog(
            temp.path(),
            "bldc",
            vec![
                version("release_6_10", ManagedRefKind::Branch, false),
                version("main", ManagedRefKind::Branch, true),
                version("release_6_9", ManagedRefKind::Branch, false),
                version("v6.9", ManagedRefKind::Tag, false),
            ],
        );

        let first = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: vec![String::from("bldc")],
                ref_kinds: vec![SourceRefKind::Branch],
                prefix: Some(String::from("release_")),
                limit: 1,
                cursor: None,
            },
            &config,
        );
        let second = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: vec![String::from("bldc")],
                ref_kinds: vec![SourceRefKind::Branch],
                prefix: Some(String::from("release_")),
                limit: 1,
                cursor: first.next_cursor.clone(),
            },
            &config,
        );

        assert_eq!(
            first.repositories[0].versions[0].display_name,
            "release_6_9"
        );
        assert_eq!(first.next_cursor.as_deref(), Some("v1:1"));
        assert_eq!(
            second.repositories[0].versions[0].display_name,
            "release_6_10"
        );
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn stale_empty_and_missing_catalogs_have_bounded_states() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let config = config(temp.path());
        let mut stale = version("main", ManagedRefKind::Branch, true);
        stale.refresh_status = RefreshStatus::Stale;
        write_catalog(temp.path(), "bldc", vec![stale]);

        let response = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: Vec::new(),
                ref_kinds: Vec::new(),
                prefix: None,
                limit: 20,
                cursor: None,
            },
            &config,
        );

        assert_eq!(response.repositories[0].sync_state, SourceSyncState::Stale);
        assert_eq!(
            response.repositories[1].sync_state,
            SourceSyncState::Unavailable
        );
        assert!(
            response.repositories[1]
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.len() < 128)
        );

        write_catalog(temp.path(), "bldc", Vec::new());
        let empty = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: vec![String::from("bldc")],
                ref_kinds: Vec::new(),
                prefix: None,
                limit: 20,
                cursor: None,
            },
            &config,
        );
        assert_eq!(empty.repositories[0].sync_state, SourceSyncState::Fresh);
        assert!(empty.repositories[0].versions.is_empty());
    }

    #[test]
    fn unknown_repository_and_invalid_cursor_are_structured_errors() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let config = config(temp.path());
        let unknown = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: vec![String::from("unknown")],
                ref_kinds: Vec::new(),
                prefix: None,
                limit: 20,
                cursor: None,
            },
            &config,
        );
        let invalid_cursor = list_vesc_source_versions_tool(
            &ListVescSourceVersionsParams {
                repository_ids: Vec::new(),
                ref_kinds: Vec::new(),
                prefix: None,
                limit: 20,
                cursor: Some(String::from("/secret/path")),
            },
            &config,
        );

        assert_eq!(
            unknown.error.as_ref().map(|error| error.code.as_str()),
            Some("unknown_repository")
        );
        assert_eq!(
            invalid_cursor
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("invalid_cursor")
        );
        assert!(
            !serde_json::to_string(&invalid_cursor)
                .unwrap()
                .contains("/secret/path")
        );
    }
}
