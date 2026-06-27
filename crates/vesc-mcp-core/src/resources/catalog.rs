//! Catalog-backed MCP resources (build recipes, doc topics).
//!
//! Loads YAML from `catalog/` and renders markdown bodies with source attribution.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::attribution::{SourceRef, append_source_footer};
use super::{
    ParsedResourceUri, ResourceMeta, ResourceReadError, ResourceReadHandler, ResourceRegistry,
    ResourceRegistryError,
};

/// Relative path to the Refloat build-flow catalog document.
pub const BUILD_FLOW_CATALOG_REL: &str = "refloat/build-flow.yaml";

/// `vesc://catalog/build-recipe/refloat-vesc-tool`
pub const REFLOAT_VESC_TOOL_URI: &str = "vesc://catalog/build-recipe/refloat-vesc-tool";

/// Parsed build-flow catalog document used to render build-recipe resources.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildFlowDoc {
    pub id: String,
    pub source_repo: String,
    pub makefile: MakefileSection,
    pub targets: Vec<TargetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MakefileSection {
    pub path: String,
    pub default_target: String,
    pub variables: Vec<MakefileVariable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MakefileVariable {
    pub name: String,
    pub default: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TargetEntry {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub lines: Option<[u64; 2]>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub modes: Vec<BuildMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildMode {
    pub id: String,
    #[serde(default)]
    pub condition: Option<String>,
    pub command: String,
    pub description: String,
}

/// Load `catalog/refloat/build-flow.yaml` from a catalog root directory.
///
/// # Errors
///
/// Returns I/O or YAML parse errors, or when the file is missing.
pub fn load_build_flow(catalog_root: &Path) -> Result<BuildFlowDoc, String> {
    let path = catalog_root.join(BUILD_FLOW_CATALOG_REL);
    let content =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|err| format!("parse {}: {err}", path.display()))
}

/// Register static build-recipe resource metadata in the registry.
///
/// # Errors
///
/// Returns [`ResourceRegistryError`] when a URI is invalid or already registered.
pub fn register_build_recipe_resources(
    registry: &mut ResourceRegistry,
) -> Result<(), ResourceRegistryError> {
    registry.register(ResourceMeta {
        uri: REFLOAT_VESC_TOOL_URI.into(),
        name: "Refloat vesc_tool build recipe".into(),
        description: Some("Build Refloat .vescpkg packages via Makefile and vesc_tool".into()),
        mime_type: "text/markdown".into(),
    })
}

/// Read a build-recipe resource body by URI.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the URI is unknown or catalog load fails.
pub fn read_build_recipe(uri: &str, catalog_root: &Path) -> Result<String, ResourceReadError> {
    let doc = load_build_flow(catalog_root).map_err(|message| ResourceReadError::ReadFailed {
        uri: uri.into(),
        message,
    })?;

    match uri {
        REFLOAT_VESC_TOOL_URI => Ok(render_refloat_vesc_tool(&doc)),
        other => Err(ResourceReadError::NotFound { uri: other.into() }),
    }
}

fn render_refloat_vesc_tool(doc: &BuildFlowDoc) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Refloat package build (vesc_tool)\n\nBuild `{repo}` packages using `{makefile}` (default target `{default}`).\n",
        repo = doc.source_repo,
        makefile = doc.makefile.path,
        default = doc.makefile.default_target,
    );

    if !doc.makefile.variables.is_empty() {
        let _ = writeln!(out, "## Makefile variables\n");
        for var in &doc.makefile.variables {
            let _ = writeln!(
                out,
                "- `{name}` (default `{default}`): {desc}",
                name = var.name,
                default = var.default,
                desc = var.description,
            );
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "## Makefile targets\n");
    for target in &doc.targets {
        render_target(&mut out, doc, target);
    }

    append_source_footer(
        &mut out,
        &[
            SourceRef::new(&doc.source_repo, &doc.makefile.path),
            SourceRef::literal(format!("catalog/{BUILD_FLOW_CATALOG_REL}")),
        ],
    );
    out
}

fn render_target(out: &mut String, doc: &BuildFlowDoc, target: &TargetEntry) {
    let _ = writeln!(out, "### `{name}`", name = target.name);
    if let Some(desc) = &target.description {
        let _ = writeln!(out, "\n{desc}\n");
    }

    if !target.modes.is_empty() {
        for mode in &target.modes {
            if let Some(condition) = &mode.condition {
                let _ = writeln!(
                    out,
                    "**{id}** (`{condition}`): {desc}",
                    id = mode.id,
                    condition = condition,
                    desc = mode.description
                );
            } else {
                let _ = writeln!(
                    out,
                    "**{id}**: {desc}",
                    id = mode.id,
                    desc = mode.description
                );
            }
            let _ = writeln!(out, "\n```makefile\n{cmd}\n```\n", cmd = mode.command);
        }
    } else if let Some(command) = &target.command {
        let _ = writeln!(out, "\n```makefile\n{command}\n```\n");
    }

    if let (Some(path), Some([start, end, ..])) = (&target.path, target.lines) {
        let _ = writeln!(
            out,
            "_Defined in `{repo}/{path}` lines {start}–{end}_\n",
            repo = doc.source_repo,
            path = path,
            start = start,
            end = end,
        );
    }
}

/// Handler dispatching catalog build-recipe URIs.
#[derive(Debug, Clone)]
pub struct BuildRecipeResourceHandler {
    catalog_root: PathBuf,
}

impl BuildRecipeResourceHandler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalog_root: repo_catalog_root(),
        }
    }
}

impl Default for BuildRecipeResourceHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceReadHandler for BuildRecipeResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::Catalog(catalog) if catalog.kind == "build-recipe"
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        read_build_recipe(&uri.to_uri(), &self.catalog_root)
    }
}

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[cfg(test)]
fn default_catalog_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_build_flow_parses_catalog_fixture() {
        let doc = load_build_flow(&default_catalog_root()).expect("load build-flow");
        assert_eq!(doc.id, "refloat-build-flow");
    }

    #[test]
    fn render_refloat_includes_pkgdesc_command() {
        let doc = load_build_flow(&default_catalog_root()).expect("load");
        let body = render_refloat_vesc_tool(&doc);
        assert!(body.contains("--buildPkgFromDesc"));
    }
}
