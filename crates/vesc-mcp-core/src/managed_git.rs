//! Persistent bare Git repositories used by managed knowledge sources.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};

use crate::managed_repositories::{
    KnowledgeDataLayout, KnowledgeRepository, RepositoryId, RepositoryPolicy, RepositoryRegistry,
};

const HEADS_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";
const TAGS_REFSPEC: &str = "+refs/tags/*:refs/tags/*";

/// Failure to synchronize, catalog, or resolve a managed repository.
#[derive(Debug, thiserror::Error)]
pub enum ManagedGitError {
    #[error("managed repository storage failed")]
    Storage(#[source] std::io::Error),
    #[error("managed repository operation failed: {0}")]
    Git(String),
    #[error("managed repository task failed")]
    Task(#[from] tokio::task::JoinError),
    #[error("repository selector was not found")]
    UnknownSelector,
    #[error("repository selector does not identify a commit")]
    NotACommit,
    #[error("repository selector identifies an unreachable commit")]
    UnreachableCommit,
}

impl From<std::io::Error> for ManagedGitError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error)
    }
}

/// Kind of version-bearing reference retained in the bounded catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRefKind {
    Branch,
    Tag,
}

/// Whether a catalog entry came from the latest successful refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshStatus {
    Fresh,
    Stale,
}

/// One branch or tag resolved to an immutable commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRef {
    pub full_name: String,
    pub display_version: String,
    pub kind: ManagedRefKind,
    pub commit: String,
    pub is_default: bool,
    pub refresh_status: RefreshStatus,
}

/// Deterministic branch and tag inventory for one managed repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefCatalog {
    pub repository: RepositoryId,
    pub refs: Vec<ManagedRef>,
}

impl RefCatalog {
    fn mark_stale(&mut self) {
        for entry in &mut self.refs {
            entry.refresh_status = RefreshStatus::Stale;
        }
    }
}

/// Why a synchronization call returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDisposition {
    Refreshed,
    Deduplicated,
    Stale,
}

/// Result of one startup or operator-requested synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySync {
    pub catalog: RefCatalog,
    pub disposition: SyncDisposition,
    pub warning: Option<String>,
}

/// An immutable commit selected for ingestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRevision {
    pub commit: String,
}

#[derive(Default)]
struct SyncSlot {
    generation: Mutex<u64>,
}

/// Path-based handle for managed bare repositories.
#[derive(Clone)]
pub struct ManagedGitStore {
    layout: KnowledgeDataLayout,
    slots: Arc<Mutex<HashMap<RepositoryId, Arc<SyncSlot>>>>,
}

impl ManagedGitStore {
    #[must_use]
    pub fn new(layout: KnowledgeDataLayout) -> Self {
        Self {
            layout,
            slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn repository_path(&self, id: &RepositoryId) -> PathBuf {
        self.layout.repository(id)
    }

    /// Read the last persisted ref catalog without performing network I/O.
    ///
    /// # Errors
    ///
    /// Returns an error when no valid catalog is available.
    pub fn catalog(&self, id: &RepositoryId) -> Result<RefCatalog, ManagedGitError> {
        read_catalog(&self.layout, id)
    }

    /// Synchronize every enabled configured repository during explicit startup.
    pub async fn startup_sync(
        &self,
        repositories: &RepositoryRegistry,
    ) -> Vec<(RepositoryId, Result<RepositorySync, ManagedGitError>)> {
        let mut outcomes = Vec::new();
        for repository in repositories.iter() {
            if repository.policy() != RepositoryPolicy::Disabled {
                outcomes.push((repository.id().clone(), self.refresh(repository).await));
            }
        }
        outcomes
    }

    /// Fetch one configured repository on explicit operator request.
    ///
    /// # Errors
    ///
    /// Returns an error when the repository has no usable cached catalog and
    /// cloning, fetching, catalog persistence, or the blocking task fails.
    pub async fn refresh(
        &self,
        repository: &KnowledgeRepository,
    ) -> Result<RepositorySync, ManagedGitError> {
        self.sync_source(
            repository.id(),
            repository.remote_url(),
            repository.default_ref(),
        )
        .await
    }

    pub(crate) async fn sync_source(
        &self,
        id: &RepositoryId,
        remote_url: &str,
        default_ref: &str,
    ) -> Result<RepositorySync, ManagedGitError> {
        let slot = {
            let mut slots = self.slots.lock().expect("managed Git slots mutex poisoned");
            Arc::clone(slots.entry(id.clone()).or_default())
        };
        let observed_generation = *slot
            .generation
            .lock()
            .expect("managed Git generation mutex poisoned");
        let layout = self.layout.clone();
        let id = id.clone();
        let remote_url = remote_url.to_owned();
        let default_ref = default_ref.to_owned();
        let interrupt = Arc::new(AtomicBool::new(false));
        Self::run_sync(
            layout,
            id,
            remote_url,
            default_ref,
            slot,
            observed_generation,
            interrupt,
        )
        .await
    }

    async fn run_sync(
        layout: KnowledgeDataLayout,
        id: RepositoryId,
        remote_url: String,
        default_ref: String,
        slot: Arc<SyncSlot>,
        observed_generation: u64,
        interrupt: Arc<AtomicBool>,
    ) -> Result<RepositorySync, ManagedGitError> {
        tokio::task::spawn_blocking(move || {
            let mut generation = slot
                .generation
                .lock()
                .expect("managed Git generation mutex poisoned");
            if *generation != observed_generation {
                drop(generation);
                let catalog = read_catalog(&layout, &id)?;
                return Ok(RepositorySync {
                    catalog,
                    disposition: SyncDisposition::Deduplicated,
                    warning: None,
                });
            }

            let result = synchronize(&layout, &id, &remote_url, &default_ref, &interrupt);
            if result.is_ok() {
                *generation += 1;
            }
            drop(generation);
            match result {
                Ok(catalog) => Ok(RepositorySync {
                    catalog,
                    disposition: SyncDisposition::Refreshed,
                    warning: None,
                }),
                Err(error) => match read_catalog(&layout, &id) {
                    Ok(mut catalog) => {
                        catalog.mark_stale();
                        Ok(RepositorySync {
                            catalog,
                            disposition: SyncDisposition::Stale,
                            warning: Some(error.to_string()),
                        })
                    }
                    Err(_) => Err(error),
                },
            }
        })
        .await?
    }

    #[cfg(test)]
    async fn sync_source_interrupted(
        &self,
        id: &RepositoryId,
        remote_url: &str,
        default_ref: &str,
    ) -> Result<RepositorySync, ManagedGitError> {
        let slot = {
            let mut slots = self.slots.lock().expect("managed Git slots mutex poisoned");
            Arc::clone(slots.entry(id.clone()).or_default())
        };
        let observed_generation = *slot
            .generation
            .lock()
            .expect("managed Git generation mutex poisoned");
        let interrupt = Arc::new(AtomicBool::new(true));
        Self::run_sync(
            self.layout.clone(),
            id.clone(),
            remote_url.to_owned(),
            default_ref.to_owned(),
            slot,
            observed_generation,
            interrupt,
        )
        .await
    }

    /// Resolve a branch, tag, or reachable exact object ID to a commit ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the catalog or repository cannot be read, the
    /// selector is unknown, its object is not a commit, or it is unreachable.
    pub fn resolve(
        &self,
        id: &RepositoryId,
        selector: &str,
    ) -> Result<ResolvedRevision, ManagedGitError> {
        let catalog = read_catalog(&self.layout, id)?;
        let configured_branch = selector.strip_prefix("refs/heads/");
        if let Some(entry) = catalog.refs.iter().find(|entry| {
            entry.full_name == selector
                || entry.display_version == selector
                || configured_branch.is_some_and(|branch| {
                    entry.full_name.strip_prefix("refs/remotes/origin/") == Some(branch)
                })
        }) {
            return Ok(ResolvedRevision {
                commit: entry.commit.clone(),
            });
        }

        let object_id = gix::hash::ObjectId::from_hex(selector.as_bytes())
            .map_err(|_| ManagedGitError::UnknownSelector)?;
        let repo = gix::open(self.repository_path(id)).map_err(git_error)?;
        let object = repo
            .find_object(object_id)
            .map_err(|_| ManagedGitError::UnknownSelector)?
            .peel_tags_to_end()
            .map_err(git_error)?;
        if object.kind != gix::object::Kind::Commit {
            return Err(ManagedGitError::NotACommit);
        }
        let reachable = catalog.refs.iter().any(|entry| {
            let Ok(tip) = gix::hash::ObjectId::from_hex(entry.commit.as_bytes()) else {
                return false;
            };
            repo.rev_walk([tip])
                .all()
                .is_ok_and(|mut walk| walk.any(|item| item.is_ok_and(|info| info.id == object_id)))
        });
        if !reachable {
            return Err(ManagedGitError::UnreachableCommit);
        }
        Ok(ResolvedRevision {
            commit: object_id.to_string(),
        })
    }
}

fn synchronize(
    layout: &KnowledgeDataLayout,
    id: &RepositoryId,
    remote_url: &str,
    default_ref: &str,
    interrupt: &AtomicBool,
) -> Result<RefCatalog, ManagedGitError> {
    if interrupt.load(Ordering::Relaxed) {
        return Err(ManagedGitError::Git("refresh interrupted".to_owned()));
    }
    let repositories = layout.root().as_path().join("repositories");
    fs::create_dir_all(&repositories)?;
    fs::create_dir_all(layout.staging())?;
    let lock = lock_repository(&repositories.join(format!("{}.lock", id.as_str())))?;
    let repository_path = layout.repository(id);

    if repository_path.exists() {
        fetch_existing(&repository_path, interrupt)?;
    } else {
        clone_missing(layout, &repository_path, remote_url, interrupt)?;
    }
    let catalog = build_catalog(&repository_path, id, default_ref)?;
    write_catalog(layout, &catalog)?;
    FileExt::unlock(&lock)?;
    Ok(catalog)
}

fn lock_repository(path: &Path) -> Result<File, ManagedGitError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn clone_missing(
    layout: &KnowledgeDataLayout,
    repository_path: &Path,
    remote_url: &str,
    interrupt: &AtomicBool,
) -> Result<(), ManagedGitError> {
    let staging = tempfile::Builder::new()
        .prefix("managed-clone-")
        .tempdir_in(layout.staging())?;
    let candidate = staging.path().join("repository.git");
    let mut prepare = gix::clone::PrepareFetch::new(
        remote_url,
        &candidate,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
        gix::open::Options::default(),
    )
    .map_err(git_error)?
    .configure_remote(|remote| {
        remote
            .with_refspecs([HEADS_REFSPEC, TAGS_REFSPEC], gix::remote::Direction::Fetch)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    });
    let (repository, _) = prepare
        .fetch_only(gix::progress::Discard, interrupt)
        .map_err(git_error)?;
    drop(repository);
    fs::rename(candidate, repository_path)?;
    Ok(())
}

fn fetch_existing(repository_path: &Path, interrupt: &AtomicBool) -> Result<(), ManagedGitError> {
    let repo = gix::open(repository_path).map_err(git_error)?;
    let remote = repo
        .find_remote("origin")
        .map_err(git_error)?
        .with_refspecs([HEADS_REFSPEC, TAGS_REFSPEC], gix::remote::Direction::Fetch)
        .map_err(git_error)?;
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(git_error)?;
    connection
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(git_error)?
        .receive(gix::progress::Discard, interrupt)
        .map_err(git_error)?;
    Ok(())
}

fn build_catalog(
    repository_path: &Path,
    id: &RepositoryId,
    default_ref: &str,
) -> Result<RefCatalog, ManagedGitError> {
    let repo = gix::open(repository_path).map_err(git_error)?;
    let expected_default = default_ref.strip_prefix("refs/heads/").map_or_else(
        || default_ref.to_owned(),
        |name| format!("refs/remotes/origin/{name}"),
    );
    let references = repo.references().map_err(git_error)?;
    let mut refs = Vec::new();
    for reference in references.all().map_err(git_error)? {
        let mut reference = reference.map_err(git_error)?;
        let full_name = reference.name().as_bstr().to_string();
        let (kind, display_version) = if let Some(name) = full_name.strip_prefix("refs/tags/") {
            (ManagedRefKind::Tag, name.to_owned())
        } else if let Some(name) = full_name.strip_prefix("refs/remotes/origin/") {
            if name == "HEAD" {
                continue;
            }
            (ManagedRefKind::Branch, name.to_owned())
        } else {
            continue;
        };
        let commit = reference
            .peel_to_commit()
            .map_err(git_error)?
            .id
            .to_string();
        refs.push(ManagedRef {
            is_default: full_name == expected_default,
            full_name,
            display_version,
            kind,
            commit,
            refresh_status: RefreshStatus::Fresh,
        });
    }
    refs.sort_by(|left, right| left.full_name.cmp(&right.full_name));
    Ok(RefCatalog {
        repository: id.clone(),
        refs,
    })
}

fn catalog_path(layout: &KnowledgeDataLayout, id: &RepositoryId) -> PathBuf {
    layout
        .root()
        .as_path()
        .join("repositories")
        .join(format!("{}.refs.json", id.as_str()))
}

fn read_catalog(
    layout: &KnowledgeDataLayout,
    id: &RepositoryId,
) -> Result<RefCatalog, ManagedGitError> {
    let bytes = fs::read(catalog_path(layout, id))?;
    serde_json::from_slice(&bytes).map_err(|error| ManagedGitError::Git(error.to_string()))
}

fn write_catalog(
    layout: &KnowledgeDataLayout,
    catalog: &RefCatalog,
) -> Result<(), ManagedGitError> {
    let target = catalog_path(layout, &catalog.repository);
    let parent = target.parent().expect("catalog path has parent");
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temporary, catalog)
        .map_err(|error| ManagedGitError::Git(error.to_string()))?;
    temporary.as_file().sync_all()?;
    temporary.persist(&target).map_err(|error| error.error)?;
    Ok(())
}

fn git_error(error: impl std::fmt::Display) -> ManagedGitError {
    ManagedGitError::Git(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;
    use crate::managed_repositories::{DataRoot, KnowledgeDataLayout, RepositoryId};

    fn run_git(directory: &std::path::Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-C", directory.to_str().expect("UTF-8 fixture path")])
            .args(arguments)
            .output()
            .expect("git fixture command starts");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn fixture_remote(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let work = root.join("work");
        let remote = root.join("remote.git");
        fs::create_dir(&work).expect("create work tree");
        run_git(&work, &["init", "-b", "main"]);
        run_git(&work, &["config", "user.name", "Test Author"]);
        run_git(&work, &["config", "user.email", "test@example.invalid"]);
        fs::write(work.join("README.md"), "first\n").expect("write fixture");
        run_git(&work, &["add", "README.md"]);
        run_git(&work, &["commit", "-m", "first"]);
        let first = run_git(&work, &["rev-parse", "HEAD"]);
        run_git(&work, &["tag", "v1"]);
        run_git(&work, &["tag", "-a", "v1-annotated", "-m", "version one"]);
        let remote_arg = remote.to_str().expect("UTF-8 fixture path");
        run_git(&work, &["clone", "--bare", ".", remote_arg]);
        run_git(&work, &["remote", "add", "origin", remote_arg]);
        (work, remote, first)
    }

    #[tokio::test]
    async fn first_sync_creates_bare_store_and_resolves_refs() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first) = fixture_remote(temp.path());
        let data = temp.path().join("data");
        let layout = KnowledgeDataLayout::new(DataRoot::new(data).expect("absolute data root"));
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("fixture").expect("valid repository id");

        let result = store
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 fixture path"),
                "refs/heads/main",
            )
            .await
            .expect("initial sync succeeds");

        let repository = gix::open(store.repository_path(&id)).expect("bare repository opens");
        assert!(repository.workdir().is_none());
        assert!(
            result
                .catalog
                .refs
                .iter()
                .any(|entry| entry.full_name == "refs/remotes/origin/main")
        );
        assert_eq!(store.resolve(&id, "main").expect("branch").commit, first);
        assert_eq!(
            store.resolve(&id, "v1").expect("lightweight tag").commit,
            first
        );
        assert_eq!(
            store
                .resolve(&id, "v1-annotated")
                .expect("annotated tag")
                .commit,
            first
        );
    }

    #[tokio::test]
    async fn persisted_catalog_is_readable_without_refresh() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, first) = fixture_remote(temp.path());
        let data = temp.path().join("data");
        let layout = KnowledgeDataLayout::new(DataRoot::new(data).expect("absolute data root"));
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("fixture").expect("valid repository id");
        store
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 fixture path"),
                "refs/heads/main",
            )
            .await
            .expect("initial sync succeeds");

        fs::remove_dir_all(remote).expect("remove remote");
        let catalog = store.catalog(&id).expect("read persisted catalog");

        assert_eq!(catalog.repository, id);
        assert!(
            catalog
                .refs
                .iter()
                .any(|entry| entry.kind == ManagedRefKind::Tag && entry.commit == first)
        );
    }

    #[tokio::test]
    async fn refresh_fetches_new_refs_and_keeps_old_commits_resolvable() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, first) = fixture_remote(temp.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("fixture").expect("valid repository id");
        let remote = remote.to_str().expect("UTF-8 fixture path");
        store
            .sync_source(&id, remote, "refs/heads/main")
            .await
            .expect("initial sync succeeds");

        fs::write(work.join("README.md"), "second\n").expect("update fixture");
        run_git(&work, &["commit", "-am", "second"]);
        let second = run_git(&work, &["rev-parse", "HEAD"]);
        run_git(&work, &["branch", "release/1"]);
        run_git(&work, &["tag", "v2"]);
        run_git(&work, &["push", "origin", "main", "release/1", "--tags"]);

        let refreshed = store
            .sync_source(&id, remote, "refs/heads/main")
            .await
            .expect("refresh succeeds");
        assert_eq!(refreshed.disposition, SyncDisposition::Refreshed);
        assert_eq!(
            store.resolve(&id, "main").expect("moved branch").commit,
            second
        );
        assert_eq!(
            store.resolve(&id, "release/1").expect("new branch").commit,
            second
        );
        assert_eq!(store.resolve(&id, "v2").expect("new tag").commit, second);
        assert_eq!(
            store.resolve(&id, &first).expect("old exact commit").commit,
            first
        );
    }

    #[tokio::test]
    async fn exact_selectors_reject_unknown_and_non_commit_objects() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (work, remote, _first) = fixture_remote(temp.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("fixture").expect("valid repository id");
        store
            .sync_source(
                &id,
                remote.to_str().expect("UTF-8 fixture path"),
                "refs/heads/main",
            )
            .await
            .expect("initial sync succeeds");
        let blob = run_git(&work, &["rev-parse", "HEAD:README.md"]);

        assert!(matches!(
            store.resolve(&id, "missing"),
            Err(ManagedGitError::UnknownSelector)
        ));
        assert!(matches!(
            store.resolve(&id, "0000000000000000000000000000000000000000"),
            Err(ManagedGitError::UnknownSelector)
        ));
        assert!(matches!(
            store.resolve(&id, &blob),
            Err(ManagedGitError::NotACommit)
        ));
    }

    #[tokio::test]
    async fn interrupted_refresh_returns_stale_last_good_catalog() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first) = fixture_remote(temp.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("fixture").expect("valid repository id");
        let remote = remote.to_str().expect("UTF-8 fixture path");
        store
            .sync_source(&id, remote, "refs/heads/main")
            .await
            .expect("initial sync succeeds");

        let stale = store
            .sync_source_interrupted(&id, remote, "refs/heads/main")
            .await
            .expect("cached catalog survives interrupted fetch");

        assert_eq!(stale.disposition, SyncDisposition::Stale);
        assert!(stale.warning.is_some());
        assert!(
            stale
                .catalog
                .refs
                .iter()
                .all(|entry| entry.refresh_status == RefreshStatus::Stale)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_refresh_callers_share_one_fetch() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let (_work, remote, _first) = fixture_remote(temp.path());
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("fixture").expect("valid repository id");
        let remote = remote.to_str().expect("UTF-8 fixture path");

        let (left, right) = tokio::join!(
            store.sync_source(&id, remote, "refs/heads/main"),
            store.sync_source(&id, remote, "refs/heads/main")
        );
        let dispositions = [
            left.expect("first caller succeeds").disposition,
            right.expect("second caller succeeds").disposition,
        ];

        assert!(dispositions.contains(&SyncDisposition::Refreshed));
        assert!(dispositions.contains(&SyncDisposition::Deduplicated));
    }

    #[tokio::test]
    #[ignore = "requires public network access"]
    async fn public_https_clone_uses_rustls_transport() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let layout = KnowledgeDataLayout::new(
            DataRoot::new(temp.path().join("data")).expect("absolute data root"),
        );
        let store = ManagedGitStore::new(layout);
        let id = RepositoryId::new("hello-world").expect("valid repository id");

        let result = store
            .sync_source(
                &id,
                "https://github.com/octocat/Hello-World.git",
                "refs/heads/master",
            )
            .await
            .expect("public HTTPS clone succeeds");

        assert!(!result.catalog.refs.is_empty());
        assert!(gix::open(store.repository_path(&id)).is_ok());
    }
}
