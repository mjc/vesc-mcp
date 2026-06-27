//! Catalog-backed ABI inventory MCP resources (`application/json`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    ParsedResourceUri, ResourceMeta, ResourceReadError, ResourceReadHandler, ResourceRegistry,
    ResourceRegistryError,
};

/// Relative path to the minimal test package ABI catalog document.
pub const MINIMAL_TEST_PACKAGE_ABI_CATALOG_REL: &str = "poc/minimal-test-package-abi.yaml";

/// `vesc://catalog/abi/minimal-test-package`
pub const MINIMAL_TEST_PACKAGE_ABI_URI: &str = "vesc://catalog/abi/minimal-test-package";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct MinimalTestPackageAbiCatalog {
    package_id: String,
    sources: Vec<AbiSourceCatalog>,
    requirements: Vec<AbiRequirementCatalog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AbiSourceCatalog {
    path: String,
    #[serde(default)]
    lines: Option<[u64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct AbiRequirementCatalog {
    name: String,
    kind: String,
    caller: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MinimalTestPackageAbiResource {
    pub id: String,
    pub source_repo: String,
    pub sources: Vec<AbiSourceJson>,
    pub requirements: Vec<AbiRequirementJson>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AbiSourceJson {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<[u64; 2]>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AbiRequirementJson {
    pub name: String,
    pub kind: String,
    pub caller: String,
}

/// Register static ABI resource metadata in the registry.
///
/// # Errors
///
/// Returns [`ResourceRegistryError`] when a URI is invalid or already registered.
pub fn register_abi_resources(
    registry: &mut ResourceRegistry,
) -> Result<(), ResourceRegistryError> {
    registry.register(ResourceMeta {
        uri: MINIMAL_TEST_PACKAGE_ABI_URI.into(),
        name: "minimal test package ABI".into(),
        description: Some("JSON ABI inventory for the POC minimal native-lib test package".into()),
        mime_type: "application/json".into(),
    })
}

/// Read an ABI resource body by URI.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the URI is unknown or catalog load fails.
pub fn read_abi_resource(uri: &str, catalog_root: &Path) -> Result<String, ResourceReadError> {
    match uri {
        MINIMAL_TEST_PACKAGE_ABI_URI => {
            let body = load_minimal_test_package_abi(catalog_root).map_err(|message| {
                ResourceReadError::ReadFailed {
                    uri: uri.into(),
                    message,
                }
            })?;
            serde_json::to_string_pretty(&body).map_err(|err| ResourceReadError::ReadFailed {
                uri: uri.into(),
                message: err.to_string(),
            })
        }
        other => Err(ResourceReadError::NotFound { uri: other.into() }),
    }
}

fn load_minimal_test_package_abi(
    catalog_root: &Path,
) -> Result<MinimalTestPackageAbiResource, String> {
    let doc = load_minimal_test_package_abi_catalog(catalog_root)?;
    Ok(MinimalTestPackageAbiResource {
        id: doc.package_id,
        source_repo: "vesc-rust-poc".into(),
        sources: doc
            .sources
            .into_iter()
            .map(|source| AbiSourceJson {
                path: source.path,
                lines: source.lines,
            })
            .collect(),
        requirements: doc
            .requirements
            .into_iter()
            .map(|item| AbiRequirementJson {
                name: item.name,
                kind: item.kind,
                caller: item.caller,
            })
            .collect(),
    })
}

fn load_minimal_test_package_abi_catalog(
    catalog_root: &Path,
) -> Result<MinimalTestPackageAbiCatalog, String> {
    let path = catalog_root.join(MINIMAL_TEST_PACKAGE_ABI_CATALOG_REL);
    let content =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|err| format!("parse {}: {err}", path.display()))
}

/// Handler dispatching catalog ABI URIs.
#[derive(Debug, Clone)]
pub struct AbiResourceHandler {
    catalog_root: PathBuf,
}

impl AbiResourceHandler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalog_root: repo_catalog_root(),
        }
    }
}

impl Default for AbiResourceHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceReadHandler for AbiResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::Catalog(catalog) if catalog.kind == "abi"
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        read_abi_resource(&uri.to_uri(), &self.catalog_root)
    }
}

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_minimal_test_package_abi_catalog_parses_fixture() {
        let doc =
            load_minimal_test_package_abi_catalog(&repo_catalog_root()).expect("load catalog");
        assert_eq!(doc.package_id, "minimal-test-package");
        assert_eq!(doc.requirements.len(), 12);
    }
}
