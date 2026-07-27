//! Shared, bounded readiness state for background knowledge preparation.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STATUS_FILE: &str = "preparation-status.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreparationState {
    Preparing,
    Ready,
    Stale,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreparationPhase {
    Starting,
    SynchronizingRepositories,
    PlanningHistory,
    BuildingLexicalIndex,
    BuildingSemanticIndex,
    Publishing,
    Serving,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KnowledgePreparationStatus {
    pub state: PreparationState,
    pub phase: PreparationPhase,
    pub repositories_completed: usize,
    pub repositories_total: usize,
    #[serde(default)]
    pub freshness_required: bool,
}

impl KnowledgePreparationStatus {
    #[must_use]
    pub const fn preparing(
        phase: PreparationPhase,
        repositories_completed: usize,
        repositories_total: usize,
    ) -> Self {
        Self {
            state: PreparationState::Preparing,
            phase,
            repositories_completed,
            repositories_total,
            freshness_required: false,
        }
    }

    #[must_use]
    pub const fn finished(
        state: PreparationState,
        repositories_completed: usize,
        repositories_total: usize,
    ) -> Self {
        Self {
            state,
            phase: PreparationPhase::Serving,
            repositories_completed,
            repositories_total,
            freshness_required: false,
        }
    }

    #[must_use]
    pub const fn with_freshness_required(mut self, freshness_required: bool) -> Self {
        self.freshness_required = freshness_required;
        self
    }
}

#[must_use]
pub fn read_preparation_status(data_root: &Path) -> Option<KnowledgePreparationStatus> {
    serde_json::from_slice(&fs::read(status_path(data_root)).ok()?).ok()
}

#[must_use]
pub fn read_or_starting(data_root: &Path, repositories_total: usize) -> KnowledgePreparationStatus {
    read_preparation_status(data_root).unwrap_or_else(|| {
        KnowledgePreparationStatus::preparing(PreparationPhase::Starting, 0, repositories_total)
    })
}

/// Whether the current managed artifact may be exposed to search.
#[must_use]
pub fn managed_artifact_is_servable(data_root: &Path) -> bool {
    read_preparation_status(data_root)
        .is_none_or(|status| !status.freshness_required || status.state == PreparationState::Ready)
}

/// Atomically publish knowledge preparation progress for all server sessions.
///
/// # Errors
///
/// Returns an error when the data root cannot be created, serialized, or updated.
pub fn write_preparation_status(
    data_root: &Path,
    status: &KnowledgePreparationStatus,
) -> anyhow::Result<()> {
    fs::create_dir_all(data_root)?;
    let path = status_path(data_root);
    let temporary = data_root.join(format!(".{STATUS_FILE}.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(status)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn status_path(data_root: &Path) -> PathBuf {
    data_root.join(STATUS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_status_roundtrips_through_the_shared_data_root() {
        let root = tempfile::tempdir().expect("data root");
        let status =
            KnowledgePreparationStatus::preparing(PreparationPhase::BuildingSemanticIndex, 3, 3);

        write_preparation_status(root.path(), &status).expect("write status");

        assert_eq!(read_preparation_status(root.path()), Some(status));
    }

    #[test]
    fn missing_status_reports_starting_without_inventing_progress() {
        let root = tempfile::tempdir().expect("data root");

        assert_eq!(
            read_or_starting(root.path(), 3),
            KnowledgePreparationStatus::preparing(PreparationPhase::Starting, 0, 3)
        );
    }

    #[test]
    fn managed_artifact_serving_respects_freshness_and_failure_state() {
        let root = tempfile::tempdir().expect("data root");

        write_preparation_status(
            root.path(),
            &KnowledgePreparationStatus::preparing(PreparationPhase::PlanningHistory, 1, 2)
                .with_freshness_required(true),
        )
        .expect("strict preparing status");
        assert!(!managed_artifact_is_servable(root.path()));

        write_preparation_status(
            root.path(),
            &KnowledgePreparationStatus::finished(PreparationState::Ready, 2, 2)
                .with_freshness_required(true),
        )
        .expect("strict ready status");
        assert!(managed_artifact_is_servable(root.path()));

        write_preparation_status(
            root.path(),
            &KnowledgePreparationStatus::finished(PreparationState::Stale, 1, 2),
        )
        .expect("offline stale status");
        assert!(managed_artifact_is_servable(root.path()));

        write_preparation_status(
            root.path(),
            &KnowledgePreparationStatus::finished(PreparationState::Failed, 1, 2),
        )
        .expect("offline failed status");
        assert!(managed_artifact_is_servable(root.path()));

        write_preparation_status(
            root.path(),
            &KnowledgePreparationStatus::finished(PreparationState::Failed, 1, 2)
                .with_freshness_required(true),
        )
        .expect("strict failed status");
        assert!(!managed_artifact_is_servable(root.path()));
    }
}
