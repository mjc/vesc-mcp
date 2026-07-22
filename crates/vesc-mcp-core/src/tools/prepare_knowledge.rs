use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::KnowledgeConfig;
use crate::managed_git::ManagedGitError;
use crate::managed_repositories::{KnowledgeDataLayout, RepositoryId};
use crate::managed_snapshots::{
    KnowledgeSnapshotStore, PreparedSnapshot, SnapshotDisposition, SnapshotError,
};

const VERSION_HINT: &str = "call list_vesc_source_versions and select configured refs";

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PrepareVescKnowledgeParams {
    /// Configured repository ID to ref or commit selector.
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
    /// Maximum preparation time; defaults to 120 seconds and is capped at 600.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrepareVescKnowledgeResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PrepareVescKnowledgeError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrepareVescKnowledgeError {
    pub code: &'static str,
    pub message: String,
    pub hint: &'static str,
}

pub async fn prepare_vesc_knowledge_tool(
    params: &PrepareVescKnowledgeParams,
    config: &KnowledgeConfig,
) -> PrepareVescKnowledgeResponse {
    let Some(root) = config.data_root.clone() else {
        return failure(
            "not_configured",
            "managed knowledge storage is not configured",
        );
    };
    if config.repositories.is_empty() {
        return failure("not_configured", "managed repositories are not configured");
    }
    let mut selectors = BTreeMap::new();
    for (id, selector) in &params.sources {
        let Ok(id) = RepositoryId::new(id.clone()) else {
            return failure("unknown_repository", "source repository is not configured");
        };
        selectors.insert(id, selector.clone());
    }
    let store = match KnowledgeSnapshotStore::new(KnowledgeDataLayout::new(root))
        .with_semantic_config(config)
    {
        Ok(store) => store,
        Err(error) => return snapshot_failure(&error),
    };
    let timeout_secs = params.timeout_secs.unwrap_or(120);
    if timeout_secs > 600 {
        return failure("invalid_selection", "timeout exceeds 600 seconds");
    }
    let prepared = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        store.prepare(&config.repositories, &selectors),
    )
    .await;
    match prepared {
        Err(_) => failure("timeout", "snapshot preparation timed out"),
        Ok(Ok(prepared)) => success(&prepared),
        Ok(Err(error)) => snapshot_failure(&error),
    }
}

pub async fn prepare_vesc_knowledge_json(
    params: &PrepareVescKnowledgeParams,
    config: &KnowledgeConfig,
) -> String {
    serde_json::to_string(&prepare_vesc_knowledge_tool(params, config).await)
        .unwrap_or_else(|_| r#"{"ok":false,"error":{"code":"serialization","message":"response serialization failed","hint":"retry the request"}}"#.to_owned())
}

fn success(prepared: &PreparedSnapshot) -> PrepareVescKnowledgeResponse {
    let sources = prepared
        .manifest
        .repositories
        .iter()
        .map(|source| (source.repository.as_str().to_owned(), source.commit.clone()))
        .collect();
    let status = match prepared.disposition {
        SnapshotDisposition::Built => "built",
        SnapshotDisposition::Reused => "reused",
        SnapshotDisposition::Deduplicated => "deduplicated",
        SnapshotDisposition::Stale => "stale",
    };
    PrepareVescKnowledgeResponse {
        ok: true,
        snapshot_id: Some(prepared.manifest.id.as_str().to_owned()),
        sources,
        status: Some(status),
        error: None,
    }
}

fn snapshot_failure(error: &SnapshotError) -> PrepareVescKnowledgeResponse {
    let code = match error {
        SnapshotError::UnknownRepository(_) => "unknown_repository",
        SnapshotError::ManagedGit(error) => match error {
            ManagedGitError::UnknownSelector | ManagedGitError::NotACommit => "unknown_ref",
            ManagedGitError::UnreachableCommit => "unreachable_commit",
            ManagedGitError::Storage(_) | ManagedGitError::Git(_) => "source_unavailable",
            ManagedGitError::Task(_) => "cancelled",
        },
        SnapshotError::Build(_) => "build_failed",
        SnapshotError::Task(_) => "cancelled",
        SnapshotError::Storage(_)
        | SnapshotError::Serialization(_)
        | SnapshotError::IdentityMismatch => "not_ready",
        SnapshotError::EmptySelection | SnapshotError::DuplicateRepository => "invalid_selection",
    };
    failure(code, &error.to_string())
}

fn failure(code: &'static str, message: &str) -> PrepareVescKnowledgeResponse {
    PrepareVescKnowledgeResponse {
        ok: false,
        snapshot_id: None,
        sources: BTreeMap::new(),
        status: None,
        error: Some(PrepareVescKnowledgeError {
            code,
            message: message.to_owned(),
            hint: VERSION_HINT,
        }),
    }
}
