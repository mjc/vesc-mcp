//! vescpkg manifest MCP resources — fixture and sandboxed dynamic reads.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use vesc_domain::{ParsedPkgDesc, parse_pkgdesc_qml};

use crate::config::{allowed_package_roots, validate_sandbox_file};
use crate::tools::inspect::{ParsedPkgdescJson, pkgdesc_to_json};

use super::attribution::{SourceRef, append_source_footer};
use super::{
    ParsedResourceUri, ResourceMeta, ResourceReadError, ResourceReadHandler, ResourceRegistry,
    ResourceRegistryError, parse_resource_uri,
};

/// `vescpkg://fixture/refloat-minimal/manifest`
pub const REFLOAT_MINIMAL_MANIFEST_URI: &str = "vescpkg://fixture/refloat-minimal/manifest";

/// `vescpkg://fixture/native-lib-minimal/manifest`
pub const NATIVE_LIB_MINIMAL_MANIFEST_URI: &str = "vescpkg://fixture/native-lib-minimal/manifest";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ManifestResourceBody {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dialect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parsed: Option<ParsedPkgdescJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_qml: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Register static fixture manifest resource metadata.
///
/// # Errors
///
/// Returns [`ResourceRegistryError`] when a URI is invalid or already registered.
pub fn register_manifest_resources(
    registry: &mut ResourceRegistry,
) -> Result<(), ResourceRegistryError> {
    registry.register(ResourceMeta {
        uri: REFLOAT_MINIMAL_MANIFEST_URI.into(),
        name: "refloat-minimal fixture manifest".into(),
        description: Some("Parsed pkgdesc for the refloat-minimal test fixture".into()),
        mime_type: "application/json".into(),
    })?;
    registry.register(ResourceMeta {
        uri: NATIVE_LIB_MINIMAL_MANIFEST_URI.into(),
        name: "native-lib-minimal fixture manifest".into(),
        description: Some("Parsed pkgdesc for the native-lib-minimal test fixture".into()),
        mime_type: "application/json".into(),
    })
}

/// Read a manifest resource body by URI.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the URI is unknown, sandbox validation fails,
/// or pkgdesc parsing fails.
pub fn read_manifest(uri: &str, allowed_roots: &[PathBuf]) -> Result<String, ResourceReadError> {
    let parsed_uri = parse_resource_uri(uri).map_err(|err| ResourceReadError::NotFound {
        uri: format!("{uri}: {err}"),
    })?;

    let path = match &parsed_uri {
        ParsedResourceUri::FixtureManifest(fixture) => resolve_fixture_pkgdesc_path(&fixture.name)
            .ok_or_else(|| ResourceReadError::NotFound { uri: uri.into() })?,
        ParsedResourceUri::DynamicManifest(manifest) => {
            resolve_dynamic_manifest_path(&manifest.path, allowed_roots).map_err(|message| {
                ResourceReadError::ReadFailed {
                    uri: uri.into(),
                    message,
                }
            })?
        }
        ParsedResourceUri::Catalog(_)
        | ParsedResourceUri::KnowledgeChunk(_)
        | ParsedResourceUri::KnowledgeDocument(_)
        | ParsedResourceUri::RefloatCommand(_) => {
            return Err(ResourceReadError::NotFound { uri: uri.into() });
        }
    };

    read_manifest_at_path(&path, uri)
}

/// Handler dispatching fixture and dynamic manifest URIs.
#[derive(Debug, Clone)]
pub struct ManifestResourceHandler {
    allowed_roots: Vec<PathBuf>,
}

impl ManifestResourceHandler {
    #[must_use]
    pub const fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self { allowed_roots }
    }

    #[must_use]
    pub fn from_config() -> Self {
        Self::new(allowed_package_roots(None))
    }
}

impl ResourceReadHandler for ManifestResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::FixtureManifest(_) | ParsedResourceUri::DynamicManifest(_)
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        read_manifest(&uri.to_uri(), &self.allowed_roots)
    }
}

fn read_manifest_at_path(path: &Path, uri: &str) -> Result<String, ResourceReadError> {
    let raw_qml = fs::read_to_string(path).map_err(|err| ResourceReadError::ReadFailed {
        uri: uri.into(),
        message: format!("read {}: {err}", path.display()),
    })?;

    let body = match parse_pkgdesc_qml(&raw_qml, path) {
        Ok(parsed) => parsed_to_body(&parsed, raw_qml),
        Err(err) => ManifestResourceBody {
            ok: false,
            dialect: None,
            parsed: None,
            raw_qml: None,
            error: Some(err.to_string()),
        },
    };

    let json = serde_json::to_string(&body).map_err(|err| ResourceReadError::ReadFailed {
        uri: uri.into(),
        message: format!("serialize manifest JSON: {err}"),
    })?;

    let mut out = json;
    let source_path = manifest_source_path(path);
    append_source_footer(&mut out, &[SourceRef::new("vesc-mcp", source_path)]);
    Ok(out)
}

fn parsed_to_body(parsed: &ParsedPkgDesc, raw_qml: String) -> ManifestResourceBody {
    let (dialect, parsed_json) = pkgdesc_to_json(parsed);

    ManifestResourceBody {
        ok: true,
        dialect: Some(dialect),
        parsed: Some(parsed_json),
        raw_qml: Some(raw_qml),
        error: None,
    }
}

fn resolve_fixture_pkgdesc_path(name: &str) -> Option<PathBuf> {
    let root = fixtures_root();
    [
        root.join(name).join("pkgdesc.qml"),
        root.join(name).join("package/pkgdesc.qml"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn resolve_dynamic_manifest_path(path: &str, allowed_roots: &[PathBuf]) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return validate_sandbox_file(path, allowed_roots);
    }

    for root in allowed_roots {
        let candidate = root.join(path);
        if candidate.is_file() {
            return validate_sandbox_file(&candidate, allowed_roots);
        }
    }

    let candidate = workspace_root().join(path);
    if candidate.is_file() {
        return validate_sandbox_file(&candidate, allowed_roots);
    }

    validate_sandbox_file(&candidate, allowed_roots)
}

fn fixtures_root() -> PathBuf {
    crate::workspace::fixtures_root()
}

fn workspace_root() -> PathBuf {
    crate::workspace::workspace_root()
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn manifest_source_path(path: &Path) -> String {
    path.strip_prefix(workspace_root()).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |relative| relative.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_sandbox_roots;

    #[test]
    fn resolve_fixture_paths_for_known_fixtures() {
        assert!(resolve_fixture_pkgdesc_path("refloat-minimal").is_some());
        assert!(resolve_fixture_pkgdesc_path("native-lib-minimal").is_some());
        assert!(resolve_fixture_pkgdesc_path("missing-fixture").is_none());
    }

    #[test]
    fn read_fixture_manifest_returns_json() {
        let body = read_manifest(REFLOAT_MINIMAL_MANIFEST_URI, &fixture_sandbox_roots())
            .expect("read refloat fixture");
        assert!(body.contains("\"raw_qml\""));
        assert!(body.contains("Refloat Minimal"));
    }
}
