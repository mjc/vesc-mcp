//! Shared, bounded readiness state for background knowledge preparation.

use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const STATUS_FILE: &str = "preparation-status.json";
static VALIDATED_VECTORS: OnceLock<Mutex<HashMap<PathBuf, ValidatedVectorArtifact>>> =
    OnceLock::new();

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validated_vector: Option<ValidatedVectorArtifact>,
}

/// File identity of a vector generation that completed lifecycle validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ValidatedVectorArtifact {
    artifact: String,
    generation: String,
    checksum: String,
    file_bytes: u64,
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl ValidatedVectorArtifact {
    /// Capture the current immutable-generation file identity.
    #[must_use]
    pub fn current_identity(root: &Path) -> Option<Self> {
        let summary = vesc_knowledge_index::inspect_previous_artifact(
            &vesc_knowledge_index::active_manifest_path(root),
        )
        .ok()?;
        let checksum = summary.vector_checksum?;
        let vector = root
            .join("generations")
            .join(summary.generation.to_string())
            .join("vectors.bin");
        let metadata = vector.metadata().ok()?;
        #[cfg(unix)]
        {
            Some(Self {
                artifact: root.file_name()?.to_str()?.to_owned(),
                generation: summary.generation.to_string(),
                checksum: checksum.to_string(),
                file_bytes: metadata.len(),
                device: metadata.dev(),
                inode: metadata.ino(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            None
        }
    }

    /// Return whether the selected generation is still the validated immutable file.
    #[must_use]
    pub fn matches(&self, root: &Path) -> bool {
        Self::current_identity(root).as_ref() == Some(self)
    }
}

pub(crate) fn record_validated_vector(root: &Path, identity: ValidatedVectorArtifact) {
    VALIDATED_VECTORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.to_owned(), identity);
}

/// Return a vector identity recorded by full lifecycle validation in this process.
#[must_use]
pub fn validated_vector(root: &Path) -> Option<ValidatedVectorArtifact> {
    let identity = VALIDATED_VECTORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
        .cloned()?;
    identity.matches(root).then_some(identity)
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
            validated_vector: None,
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
            validated_vector: None,
        }
    }

    #[must_use]
    pub const fn with_freshness_required(mut self, freshness_required: bool) -> Self {
        self.freshness_required = freshness_required;
        self
    }

    #[must_use]
    pub fn with_validated_vector(
        mut self,
        validated_vector: Option<ValidatedVectorArtifact>,
    ) -> Self {
        self.validated_vector = validated_vector;
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

    #[cfg(unix)]
    #[test]
    fn vector_validation_proof_is_invalidated_by_same_length_changes() {
        let root = tempfile::tempdir().expect("artifact root");
        let mut provider = vesc_knowledge_index::FakeEmbeddingProvider::new(8);
        vesc_knowledge_index::build_embedded_artifacts_with_provider(
            root.path(),
            &mut provider,
            "fake",
            "test",
        )
        .expect("semantic artifact");
        let proof = ValidatedVectorArtifact::current_identity(root.path()).expect("vector proof");
        assert!(proof.matches(root.path()));
        let vector = vesc_knowledge_index::active_generation_path(root.path())
            .expect("active generation")
            .join("vectors.bin");
        let mut bytes = fs::read(&vector).expect("vector bytes");
        let payload_byte = bytes.len() / 2;
        bytes[payload_byte] ^= 1;
        fs::write(vector, bytes).expect("same-length corruption");

        assert!(!proof.matches(root.path()));
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
