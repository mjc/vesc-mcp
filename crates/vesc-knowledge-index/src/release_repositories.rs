//! Managed bare repositories for reproducible release corpus builds.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use crate::{RepositoryId, Revision};

const MAX_GIT_DIAGNOSTIC_CHARS: usize = 1_024;

/// Revisions used by the published release-corpus evidence.
pub const PINNED_RELEASE_REPOSITORIES: [PinnedRepository<'static>; 4] = [
    PinnedRepository {
        id: "vesc",
        remote_url: "https://github.com/vedderb/bldc.git",
        revision: "c835e9f10989f217269efb4ec943dfea7d280dfd",
    },
    PinnedRepository {
        id: "vesc-tool",
        remote_url: "https://github.com/vedderb/vesc_tool.git",
        revision: "005a08a0189f6df83bb47fbe2f93a3320c15c11a",
    },
    PinnedRepository {
        id: "refloat",
        remote_url: "https://github.com/lukash/refloat.git",
        revision: "0ef6e99d8701886feeb7fe6c07cc4ec53fb3d97a",
    },
    PinnedRepository {
        id: "vesc-pkg",
        remote_url: "https://github.com/vedderb/vesc_pkg.git",
        revision: "10825f313fd35a798db5ec1f5c9aef2b41f947d3",
    },
];

/// One upstream repository and immutable release-corpus revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedRepository<'a> {
    pub id: &'a str,
    pub remote_url: &'a str,
    pub revision: &'a str,
}

/// One verified bare repository ready for ingestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedRepository {
    pub id: RepositoryId,
    pub path: PathBuf,
    pub revision: Revision,
}

/// Maintains the local cache using a specific Git executable.
pub struct ReleaseRepositoryCache {
    root: PathBuf,
    git: PathBuf,
}

impl ReleaseRepositoryCache {
    #[must_use]
    pub const fn new(root: PathBuf, git: PathBuf) -> Self {
        Self { root, git }
    }

    /// Clone or refresh every pinned bare repository.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be created or verified.
    pub fn maintain(
        &self,
        repositories: &[PinnedRepository<'_>],
    ) -> Result<Vec<CachedRepository>, ReleaseRepositoryError> {
        fs::create_dir_all(&self.root)?;
        repositories
            .iter()
            .map(|repository| self.maintain_one(repository))
            .collect()
    }

    fn maintain_one(
        &self,
        repository: &PinnedRepository<'_>,
    ) -> Result<CachedRepository, ReleaseRepositoryError> {
        validate_repository(repository)?;
        let path = self.root.join(format!("{}.git", repository.id));
        if path.exists() {
            if !path.is_dir() {
                return Err(ReleaseRepositoryError::InvalidCacheEntry {
                    repository: repository.id.to_owned(),
                });
            }
            self.verify_remote(repository, &path)?;
        } else {
            self.run(
                "clone pinned repository",
                [
                    "clone",
                    "--bare",
                    "--origin",
                    "origin",
                    repository.remote_url,
                ]
                .into_iter()
                .map(std::ffi::OsStr::new)
                .chain(std::iter::once(path.as_os_str())),
            )?;
        }
        self.verify_remote(repository, &path)?;
        self.run(
            "refresh pinned repository",
            [
                std::ffi::OsStr::new("--git-dir"),
                path.as_os_str(),
                std::ffi::OsStr::new("fetch"),
                std::ffi::OsStr::new("--force"),
                std::ffi::OsStr::new("--prune"),
                std::ffi::OsStr::new("--prune-tags"),
                std::ffi::OsStr::new("origin"),
                std::ffi::OsStr::new("+refs/heads/*:refs/remotes/origin/*"),
                std::ffi::OsStr::new("+refs/tags/*:refs/tags/*"),
            ]
            .into_iter(),
        )?;
        if !self.contains_commit(&path, repository.revision)? {
            let pin_ref = format!(
                "+{}:refs/vesc-mcp/pins/{}",
                repository.revision, repository.id
            );
            let _ = self.output(
                [
                    std::ffi::OsStr::new("--git-dir"),
                    path.as_os_str(),
                    std::ffi::OsStr::new("fetch"),
                    std::ffi::OsStr::new("--force"),
                    std::ffi::OsStr::new("origin"),
                    std::ffi::OsStr::new(&pin_ref),
                ]
                .into_iter(),
            )?;
        }
        if self.contains_commit(&path, repository.revision)? {
            let pin_ref = format!("refs/vesc-mcp/pins/{}", repository.id);
            self.run(
                "retain pinned repository revision",
                [
                    std::ffi::OsStr::new("--git-dir"),
                    path.as_os_str(),
                    std::ffi::OsStr::new("update-ref"),
                    std::ffi::OsStr::new(&pin_ref),
                    std::ffi::OsStr::new(repository.revision),
                ]
                .into_iter(),
            )?;
            Ok(CachedRepository {
                id: RepositoryId::try_from(repository.id)
                    .map_err(|_| ReleaseRepositoryError::InvalidRepositoryId)?,
                path,
                revision: Revision::try_from(repository.revision)
                    .map_err(|_| ReleaseRepositoryError::InvalidRevision)?,
            })
        } else {
            Err(ReleaseRepositoryError::MissingPinnedRevision {
                repository: repository.id.to_owned(),
                revision: repository.revision.to_owned(),
            })
        }
    }

    fn contains_commit(
        &self,
        path: &std::path::Path,
        revision: &str,
    ) -> Result<bool, ReleaseRepositoryError> {
        let commit = format!("{revision}^{{commit}}");
        Ok(self
            .output(
                [
                    std::ffi::OsStr::new("--git-dir"),
                    path.as_os_str(),
                    std::ffi::OsStr::new("cat-file"),
                    std::ffi::OsStr::new("-e"),
                    std::ffi::OsStr::new(&commit),
                ]
                .into_iter(),
            )?
            .status
            .success())
    }

    fn verify_remote(
        &self,
        repository: &PinnedRepository<'_>,
        path: &std::path::Path,
    ) -> Result<(), ReleaseRepositoryError> {
        let output = self.run(
            "read pinned repository origin",
            [
                std::ffi::OsStr::new("--git-dir"),
                path.as_os_str(),
                std::ffi::OsStr::new("config"),
                std::ffi::OsStr::new("--get"),
                std::ffi::OsStr::new("remote.origin.url"),
            ]
            .into_iter(),
        )?;
        (String::from_utf8_lossy(&output.stdout).trim() == repository.remote_url)
            .then_some(())
            .ok_or_else(|| ReleaseRepositoryError::RemoteMismatch {
                repository: repository.id.to_owned(),
            })
    }

    fn run<'a>(
        &self,
        action: &'static str,
        args: impl Iterator<Item = &'a std::ffi::OsStr>,
    ) -> Result<Output, ReleaseRepositoryError> {
        let output = self.output(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(ReleaseRepositoryError::Git {
                action,
                diagnostic: bounded_diagnostic(&output),
            })
        }
    }

    fn output<'a>(
        &self,
        args: impl Iterator<Item = &'a std::ffi::OsStr>,
    ) -> Result<Output, ReleaseRepositoryError> {
        Command::new(&self.git)
            .args(args)
            .output()
            .map_err(ReleaseRepositoryError::GitUnavailable)
    }
}

fn validate_repository(repository: &PinnedRepository<'_>) -> Result<(), ReleaseRepositoryError> {
    let valid_id = !repository.id.is_empty()
        && repository
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !valid_id {
        return Err(ReleaseRepositoryError::InvalidRepositoryId);
    }
    if repository.revision.len() != 40
        || !repository
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ReleaseRepositoryError::InvalidRevision);
    }
    Ok(())
}

fn bounded_diagnostic(output: &Output) -> String {
    let raw = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let diagnostic: String = String::from_utf8_lossy(raw)
        .trim()
        .chars()
        .take(MAX_GIT_DIAGNOSTIC_CHARS)
        .collect();
    if diagnostic.is_empty() {
        format!("Git exited with {}", output.status)
    } else {
        diagnostic
    }
}

/// Release repository acquisition failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReleaseRepositoryError {
    #[error("release repository ID must contain only ASCII letters, digits, and hyphens")]
    InvalidRepositoryId,
    #[error("release repository revision must be a 40-character hexadecimal commit ID")]
    InvalidRevision,
    #[error("release repository cache entry for {repository} is not a directory")]
    InvalidCacheEntry { repository: String },
    #[error(
        "release repository {repository} has an unexpected origin; remove or relocate that cache entry"
    )]
    RemoteMismatch { repository: String },
    #[error("release repository {repository} does not contain pinned commit {revision}")]
    MissingPinnedRevision {
        repository: String,
        revision: String,
    },
    #[error("Git executable is unavailable: {0}")]
    GitUnavailable(std::io::Error),
    #[error("cannot {action}: {diagnostic}")]
    Git {
        action: &'static str,
        diagnostic: String,
    },
    #[error("release repository cache I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run fixture Git");
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("Git output")
            .trim()
            .to_owned()
    }

    fn remote_fixture(root: &Path) -> (PathBuf, PathBuf, String) {
        let remote = root.join("upstream.git");
        let work = root.join("work");
        fs::create_dir(&work).expect("work directory");
        git(
            root,
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "user.email", "fixture@example.invalid"]);
        git(&work, &["config", "user.name", "Fixture"]);
        fs::write(work.join("README.md"), "fixture\n").expect("fixture content");
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "fixture"]);
        git(
            &work,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&work, &["push", "-q", "origin", "main"]);
        let revision = git(&work, &["rev-parse", "HEAD"]);
        (remote, work, revision)
    }

    #[test]
    fn cache_clones_and_reuses_a_verified_pinned_revision() {
        let root = tempdir().expect("temporary root");
        let (remote, _work, revision) = remote_fixture(root.path());
        let remote_url = remote.to_str().expect("remote URL").to_owned();
        let repository = PinnedRepository {
            id: "fixture",
            remote_url: &remote_url,
            revision: &revision,
        };
        let cache = ReleaseRepositoryCache::new(root.path().join("cache"), "git".into());

        let first = cache.maintain(&[repository]).expect("cold cache");
        let second = cache.maintain(&[repository]).expect("warm cache");

        assert_eq!(first, second);
        assert!(first[0].path.join("HEAD").is_file());
        assert_eq!(
            git(
                root.path(),
                &[
                    "--git-dir",
                    first[0].path.to_str().expect("cache path"),
                    "rev-parse",
                    "refs/vesc-mcp/pins/fixture",
                ],
            ),
            revision
        );
    }

    #[test]
    fn cache_fetches_a_pin_no_longer_advertised_by_upstream_refs() {
        let root = tempdir().expect("temporary root");
        let (remote, work, revision) = remote_fixture(root.path());
        git(&work, &["checkout", "-q", "--orphan", "replacement"]);
        git(&work, &["rm", "-q", "-r", "-f", "."]);
        fs::write(work.join("replacement.txt"), "replacement\n").expect("replacement content");
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "replacement"]);
        git(&work, &["push", "-q", "--force", "origin", "HEAD:main"]);
        let remote_url = remote.to_str().expect("remote URL").to_owned();
        let repository = PinnedRepository {
            id: "fixture",
            remote_url: &remote_url,
            revision: &revision,
        };
        let cache = ReleaseRepositoryCache::new(root.path().join("cache"), "git".into());

        let cached = cache
            .maintain(&[repository])
            .expect("unadvertised pinned commit");

        assert_eq!(
            git(
                root.path(),
                &[
                    "--git-dir",
                    cached[0].path.to_str().expect("cache path"),
                    "rev-parse",
                    "refs/vesc-mcp/pins/fixture",
                ],
            ),
            revision
        );
    }

    #[test]
    fn cache_rejects_an_existing_repository_with_the_wrong_origin() {
        let root = tempdir().expect("temporary root");
        let (remote, _work, revision) = remote_fixture(root.path());
        let remote_url = remote.to_str().expect("remote URL").to_owned();
        let repository = PinnedRepository {
            id: "fixture",
            remote_url: &remote_url,
            revision: &revision,
        };
        let cache_root = root.path().join("cache");
        let cache = ReleaseRepositoryCache::new(cache_root.clone(), "git".into());
        cache.maintain(&[repository]).expect("cold cache");
        let cached = cache_root.join("fixture.git");
        git(
            root.path(),
            &[
                "--git-dir",
                cached.to_str().expect("cache path"),
                "config",
                "remote.origin.url",
                "https://example.invalid/wrong.git",
            ],
        );

        assert!(matches!(
            cache.maintain(&[repository]),
            Err(ReleaseRepositoryError::RemoteMismatch { repository })
                if repository == "fixture"
        ));
    }

    #[test]
    fn refresh_prunes_deleted_remote_branches() {
        let root = tempdir().expect("temporary root");
        let (remote, work, revision) = remote_fixture(root.path());
        git(&work, &["checkout", "-qb", "stale"]);
        git(&work, &["push", "-q", "origin", "stale"]);
        let remote_url = remote.to_str().expect("remote URL").to_owned();
        let repository = PinnedRepository {
            id: "fixture",
            remote_url: &remote_url,
            revision: &revision,
        };
        let cache_root = root.path().join("cache");
        let cache = ReleaseRepositoryCache::new(cache_root.clone(), "git".into());
        cache.maintain(&[repository]).expect("cache with branch");
        git(&work, &["push", "-q", "origin", "--delete", "stale"]);

        cache.maintain(&[repository]).expect("pruned cache");
        let cached = cache_root.join("fixture.git");
        let status = Command::new("git")
            .args([
                "--git-dir",
                cached.to_str().expect("cache path"),
                "show-ref",
                "--verify",
                "refs/remotes/origin/stale",
            ])
            .status()
            .expect("inspect cached ref");
        assert!(!status.success());
    }

    #[test]
    fn missing_pinned_commit_is_an_actionable_error() {
        let root = tempdir().expect("temporary root");
        let (remote, _work, _revision) = remote_fixture(root.path());
        let remote_url = remote.to_str().expect("remote URL").to_owned();
        let missing = "a".repeat(40);
        let repository = PinnedRepository {
            id: "fixture",
            remote_url: &remote_url,
            revision: &missing,
        };
        let cache = ReleaseRepositoryCache::new(root.path().join("cache"), "git".into());

        assert!(matches!(
            cache.maintain(&[repository]),
            Err(ReleaseRepositoryError::MissingPinnedRevision { repository, revision })
                if repository == "fixture" && revision == missing
        ));
    }

    #[test]
    fn git_diagnostics_are_bounded_without_splitting_unicode() {
        let alias = format!("alias.loud=!printf '{}'", "é".repeat(2_048));
        let output = Command::new("git")
            .args(["-c", &alias, "loud"])
            .output()
            .expect("run diagnostic fixture");

        let diagnostic = bounded_diagnostic(&output);

        assert_eq!(diagnostic.chars().count(), MAX_GIT_DIAGNOSTIC_CHARS);
        assert!(diagnostic.chars().all(|character| character == 'é'));
    }
}
