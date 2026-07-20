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
    Indexing,
    Serving,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KnowledgePreparationStatus {
    pub state: PreparationState,
    pub phase: PreparationPhase,
    pub repositories_completed: usize,
    pub repositories_total: usize,
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
        }
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
        let status = KnowledgePreparationStatus::preparing(PreparationPhase::Indexing, 3, 3);

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
}
