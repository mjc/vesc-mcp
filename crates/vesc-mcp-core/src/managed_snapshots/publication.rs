use std::fs::{self, File};
use std::path::Path;

use crate::managed_repositories::DataRoot;

use super::{
    KnowledgeDataLayout, KnowledgeRepository, KnowledgeSnapshotManifest, KnowledgeSnapshotStore,
    ManagedGitStore, PreparedSnapshot, RepositoryRegistry, SnapshotDisposition, SnapshotError,
    acquire_snapshot_build_lock, pin_snapshot_commits, read_and_validate_manifest,
    validate_serving_compatible_artifact, validate_snapshot_artifact, write_json_atomic,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DistributedPublicationBoundary {
    ArtifactGeneration,
    ArtifactActivation,
    RepositoryObjects,
    RepositoryReferences,
    RepositoryCatalog,
    RepositoryPins,
    SnapshotManifest,
    DefaultAlias,
}

impl KnowledgeSnapshotStore {
    /// Validate and publish a complete staged default snapshot into this store.
    ///
    /// The staged data root must not overlap the live data root. Publication
    /// shares the snapshot build lock and each target repository lock, so a
    /// concurrent refresh or preparation cannot observe the pre-commit state.
    /// The canonical snapshot manifest is the commit marker and the default
    /// alias advances only after that marker is durable.
    ///
    /// # Errors
    ///
    /// Returns an error without changing live state when staged manifests,
    /// repositories, artifacts, or the configured serving contract are invalid.
    pub fn install_distributed_default(
        &self,
        staged_root: DataRoot,
        repositories: &RepositoryRegistry,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        self.install_distributed_default_inner(staged_root, repositories, |_| Ok(()))
    }

    #[cfg(test)]
    pub(super) fn install_distributed_default_with_hook(
        &self,
        staged_root: DataRoot,
        repositories: &RepositoryRegistry,
        hook: impl FnMut(DistributedPublicationBoundary) -> Result<(), SnapshotError>,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        self.install_distributed_default_inner(staged_root, repositories, hook)
    }

    fn install_distributed_default_inner(
        &self,
        staged_root: DataRoot,
        repositories: &RepositoryRegistry,
        mut hook: impl FnMut(DistributedPublicationBoundary) -> Result<(), SnapshotError>,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        reject_overlapping_roots(staged_root.as_path(), self.layout.root().as_path())?;
        let staged = KnowledgeDataLayout::new(staged_root);
        let manifest = validate_staged_default(self, &staged, repositories)?;
        let configured = configured_snapshot_repositories(&manifest, repositories)?;
        let staged_git = ManagedGitStore::new(staged.clone());
        let live_git = ManagedGitStore::new(self.layout.clone());

        let _build_lock = acquire_snapshot_build_lock(&self.layout)?;
        let _repository_locks = configured
            .iter()
            .map(|repository| live_git.lock_for_cache_publication(repository.id()))
            .collect::<Result<Vec<_>, _>>()?;

        publish_artifact_generation(&staged, &self.layout, &manifest)?;
        hook(DistributedPublicationBoundary::ArtifactGeneration)?;
        publish_artifact_activation(&staged, &self.layout, &manifest)?;
        hook(DistributedPublicationBoundary::ArtifactActivation)?;

        for repository in &configured {
            live_git.publish_cached_objects_from(&staged_git, repository)?;
        }
        hook(DistributedPublicationBoundary::RepositoryObjects)?;
        for repository in &configured {
            live_git.publish_cached_references_from(&staged_git, repository)?;
        }
        hook(DistributedPublicationBoundary::RepositoryReferences)?;
        for repository in &configured {
            live_git.publish_cached_catalog_from(&staged_git, repository)?;
        }
        hook(DistributedPublicationBoundary::RepositoryCatalog)?;

        validate_serving_compatible_artifact(
            &self.layout,
            &configured,
            &self.layout.artifact(&manifest.id),
            &manifest,
        )?;
        validate_snapshot_repository_tips(&self.layout, &configured, &manifest)?;
        pin_snapshot_commits(&self.layout, &manifest)?;
        hook(DistributedPublicationBoundary::RepositoryPins)?;

        write_json_atomic(&self.layout.snapshot(&manifest.id), &manifest)?;
        hook(DistributedPublicationBoundary::SnapshotManifest)?;
        write_json_atomic(&self.default_alias_path(), &manifest)?;
        hook(DistributedPublicationBoundary::DefaultAlias)?;

        Ok(PreparedSnapshot {
            artifact_path: self.layout.artifact(&manifest.id),
            manifest,
            disposition: SnapshotDisposition::Reused,
        })
    }
}

fn reject_overlapping_roots(staged: &Path, live: &Path) -> Result<(), SnapshotError> {
    if staged == live || staged.starts_with(live) || live.starts_with(staged) {
        return Err(SnapshotError::Build(
            "distributed cache staging must not overlap the live data root".to_owned(),
        ));
    }
    Ok(())
}

fn validate_staged_default(
    target: &KnowledgeSnapshotStore,
    staged: &KnowledgeDataLayout,
    repositories: &RepositoryRegistry,
) -> Result<KnowledgeSnapshotManifest, SnapshotError> {
    let default =
        read_and_validate_manifest(&crate::default_snapshot_path(staged.root().as_path()))?;
    let canonical = read_and_validate_manifest(&staged.snapshot(&default.id))?;
    if default != canonical {
        return Err(SnapshotError::IdentityMismatch);
    }
    if !target.snapshot_contract_matches(&default, repositories)? {
        return Err(SnapshotError::Build(
            "distributed snapshot is incompatible with the configured serving contract".to_owned(),
        ));
    }
    let configured = configured_snapshot_repositories(&default, repositories)?;
    let git = ManagedGitStore::new(staged.clone());
    for repository in &configured {
        git.validate_cached_repository(repository)?;
        let selected = default
            .repositories
            .iter()
            .find(|selected| selected.repository == *repository.id())
            .expect("configured snapshot repository was selected");
        if git.history_tips_configured(repository)? != selected.history_tips {
            return Err(SnapshotError::Build(format!(
                "distributed repository {} does not match snapshot history tips",
                repository.id()
            )));
        }
    }
    validate_snapshot_repository_tips(staged, &configured, &default)?;
    validate_serving_compatible_artifact(
        staged,
        &configured,
        &staged.artifact(&default.id),
        &default,
    )?;
    Ok(default)
}

fn configured_snapshot_repositories(
    manifest: &KnowledgeSnapshotManifest,
    repositories: &RepositoryRegistry,
) -> Result<Vec<KnowledgeRepository>, SnapshotError> {
    manifest
        .repositories
        .iter()
        .map(|selected| {
            repositories
                .iter()
                .find(|repository| repository.id() == &selected.repository)
                .cloned()
                .ok_or_else(|| SnapshotError::UnknownRepository(selected.repository.clone()))
        })
        .collect()
}

fn validate_snapshot_repository_tips(
    layout: &KnowledgeDataLayout,
    repositories: &[KnowledgeRepository],
    manifest: &KnowledgeSnapshotManifest,
) -> Result<(), SnapshotError> {
    for selected in &manifest.repositories {
        let repository = repositories
            .iter()
            .find(|repository| repository.id() == &selected.repository)
            .ok_or_else(|| SnapshotError::UnknownRepository(selected.repository.clone()))?;
        let cached = gix::open(layout.repository(repository.id())).map_err(|error| {
            SnapshotError::Build(format!("open distributed Git repository: {error}"))
        })?;
        for tip in &selected.history_tips {
            let object = gix::ObjectId::from_hex(tip.as_bytes()).map_err(|error| {
                SnapshotError::Build(format!("parse distributed snapshot commit: {error}"))
            })?;
            cached.find_commit(object).map_err(|error| {
                SnapshotError::Build(format!(
                    "distributed snapshot commit {tip} is unreachable: {error}"
                ))
            })?;
        }
    }
    Ok(())
}

fn publish_artifact_generation(
    staged: &KnowledgeDataLayout,
    live: &KnowledgeDataLayout,
    manifest: &KnowledgeSnapshotManifest,
) -> Result<(), SnapshotError> {
    let staged_artifact = staged.artifact(&manifest.id);
    let live_artifact = live.artifact(&manifest.id);
    let source = vesc_knowledge_index::active_generation_path(&staged_artifact)
        .map_err(|error| SnapshotError::Build(error.to_string()))?;
    if validate_snapshot_artifact(&live_artifact, manifest).is_ok()
        && vesc_knowledge_index::active_generation_path(&live_artifact)
            .is_ok_and(|live| live.file_name() == source.file_name())
    {
        return Ok(());
    }
    let generation = source.file_name().ok_or_else(|| {
        SnapshotError::Build("distributed artifact generation has no name".to_owned())
    })?;
    let generations = live_artifact.join("generations");
    fs::create_dir_all(&generations)?;
    let target = generations.join(generation);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    let temporary = tempfile::tempdir_in(&generations)?;
    let candidate = temporary.path().join("generation");
    copy_directory(&source, &candidate)?;
    fs::rename(candidate, target)?;
    Ok(())
}

fn publish_artifact_activation(
    staged: &KnowledgeDataLayout,
    live: &KnowledgeDataLayout,
    manifest: &KnowledgeSnapshotManifest,
) -> Result<(), SnapshotError> {
    let source = staged.artifact(&manifest.id).join("active.json");
    let target = live.artifact(&manifest.id).join("active.json");
    copy_file_atomic(&source, &target)?;
    validate_snapshot_artifact(&live.artifact(&manifest.id), manifest)
}

fn copy_directory(source: &Path, target: &Path) -> Result<(), SnapshotError> {
    fs::create_dir(target)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            let mut input = File::open(entry.path())?;
            let mut output = File::create(destination)?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        } else {
            return Err(SnapshotError::Build(
                "distributed artifact contains a non-file entry".to_owned(),
            ));
        }
    }
    Ok(())
}

fn copy_file_atomic(source: &Path, target: &Path) -> Result<(), SnapshotError> {
    let parent = target
        .parent()
        .expect("distributed artifact selector path has parent");
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut input = File::open(source)?;
    std::io::copy(&mut input, temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    temporary.persist(target).map_err(|error| error.error)?;
    Ok(())
}
