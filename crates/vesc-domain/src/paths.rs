//! Repository and package root newtypes (stub).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot(pub PathBuf);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot(pub PathBuf);
