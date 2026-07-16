//! Detect catalog YAML changes for subscribed MCP resource notifications.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use super::abi::{MINIMAL_TEST_PACKAGE_ABI_CATALOG_REL, MINIMAL_TEST_PACKAGE_ABI_URI};
use super::catalog::{BUILD_FLOW_CATALOG_REL, REFLOAT_VESC_TOOL_URI};
use super::refloat_command::REFLOAT_COMMANDS_CATALOG_REL;

const REFLOAT_COMMAND_URI_PREFIX: &str = "vesc://catalog/commands/refloat/";

/// Tracks last-seen modification times for catalog-backed resource URIs.
#[derive(Debug, Default)]
pub struct CatalogSourceWatcher {
    last_mtime: RwLock<HashMap<String, SystemTime>>,
}

impl CatalogSourceWatcher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the current catalog source mtime without treating it as a change event.
    ///
    /// # Panics
    ///
    /// Panics if the watcher lock is poisoned.
    pub fn seed_baseline(&self, uri: &str, catalog_root: &Path) {
        let Some(path) = catalog_yaml_path(uri, catalog_root) else {
            return;
        };
        if let Ok(mtime) = file_mtime(&path) {
            self.last_mtime
                .write()
                .expect("catalog watcher lock")
                .entry(uri.to_string())
                .or_insert(mtime);
        }
    }

    /// Returns `true` when a catalog YAML backing `uri` changed since the last baseline.
    ///
    /// Updates the stored mtime when a prior baseline exists and the file changed.
    ///
    /// # Panics
    ///
    /// Panics if the watcher lock is poisoned.
    #[must_use]
    pub fn take_change_if_any(&self, uri: &str, catalog_root: &Path) -> bool {
        let Some(path) = catalog_yaml_path(uri, catalog_root) else {
            return false;
        };
        let Ok(mtime) = file_mtime(&path) else {
            return false;
        };

        let mut guard = self.last_mtime.write().expect("catalog watcher lock");
        match guard.get(uri) {
            Some(prev) if *prev == mtime => false,
            Some(_) => {
                guard.insert(uri.to_string(), mtime);
                true
            }
            None => {
                guard.insert(uri.to_string(), mtime);
                false
            }
        }
    }
}

fn file_mtime(path: &Path) -> Result<SystemTime, std::io::Error> {
    path.metadata()?.modified()
}

fn catalog_yaml_path(uri: &str, catalog_root: &Path) -> Option<PathBuf> {
    let rel = match uri {
        MINIMAL_TEST_PACKAGE_ABI_URI => MINIMAL_TEST_PACKAGE_ABI_CATALOG_REL,
        REFLOAT_VESC_TOOL_URI => BUILD_FLOW_CATALOG_REL,
        _ if uri.starts_with(REFLOAT_COMMAND_URI_PREFIX) => REFLOAT_COMMANDS_CATALOG_REL,
        _ => return None,
    };
    Some(catalog_root.join(rel))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn catalog_root() -> PathBuf {
        crate::workspace::catalog_root()
    }

    #[test]
    fn first_check_establishes_baseline_without_change() {
        let watcher = CatalogSourceWatcher::new();
        assert!(!watcher.take_change_if_any(MINIMAL_TEST_PACKAGE_ABI_URI, &catalog_root()));
    }

    #[test]
    fn seed_then_modify_reports_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog_root = temp.path();
        let abi_path = catalog_root.join(MINIMAL_TEST_PACKAGE_ABI_CATALOG_REL);
        fs::create_dir_all(abi_path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &abi_path,
            "package_id: test\nsource_repo: vesc-mcp\nsources: []\nrequirements: []\n",
        )
        .expect("write");

        let watcher = CatalogSourceWatcher::new();
        watcher.seed_baseline(MINIMAL_TEST_PACKAGE_ABI_URI, catalog_root);
        thread::sleep(Duration::from_millis(1100));
        fs::write(
            &abi_path,
            "package_id: test2\nsource_repo: vesc-mcp\nsources: []\nrequirements: []\n",
        )
        .expect("rewrite");

        assert!(watcher.take_change_if_any(MINIMAL_TEST_PACKAGE_ABI_URI, catalog_root));
    }
}
