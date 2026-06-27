//! Repository and package root newtypes (stub).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot(pub PathBuf);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot(pub PathBuf);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_and_package_root_equality() {
        let path = PathBuf::from("/tmp/root");
        assert_eq!(RepoRoot(path.clone()), RepoRoot(path.clone()));
        assert_eq!(PackageRoot(path), PackageRoot(PathBuf::from("/tmp/root")));
    }
}
