//! Catalog-backed MCP resources (build recipes, doc topics).
//!
//! Loads YAML from `catalog/` and renders markdown bodies with source attribution.

use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;

use super::{ResourceMeta, ResourceReadError, ResourceRegistry, ResourceRegistryError};

/// Relative path to the Refloat build-flow catalog document.
pub const BUILD_FLOW_CATALOG_REL: &str = "refloat/build-flow.yaml";

/// `vesc://catalog/build-recipe/refloat-vesc-tool`
pub const REFLOAT_VESC_TOOL_URI: &str = "vesc://catalog/build-recipe/refloat-vesc-tool";

/// `vesc://catalog/build-recipe/poc-rust-packer`
pub const POC_RUST_PACKER_URI: &str = "vesc://catalog/build-recipe/poc-rust-packer";

/// Parsed build-flow catalog document used to render build-recipe resources.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BuildFlowDoc {
    pub id: String,
    pub source_repo: String,
    pub makefile: MakefileSection,
    pub targets: Vec<TargetEntry>,
    pub poc_equivalent: Option<PocEquivalent>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PocEquivalent {
    pub repo: String,
    pub doc: String,
    pub makefile_target: String,
    pub packer: String,
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
    })?;
    registry.register(ResourceMeta {
        uri: POC_RUST_PACKER_URI.into(),
        name: "POC Rust packer build recipe".into(),
        description: Some(
            "Build packages with vesc-rust-poc make package and Rust vesc-pkg-build".into(),
        ),
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
        POC_RUST_PACKER_URI => {
            let poc = doc
                .poc_equivalent
                .as_ref()
                .ok_or_else(|| ResourceReadError::ReadFailed {
                    uri: uri.into(),
                    message: format!("missing poc_equivalent in catalog/{BUILD_FLOW_CATALOG_REL}"),
                })?;
            Ok(render_poc_rust_packer(&doc, poc))
        }
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

    append_attribution(&mut out, &doc.source_repo, &doc.makefile.path, None);
    out
}

fn render_poc_rust_packer(doc: &BuildFlowDoc, poc: &PocEquivalent) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# POC Rust packer build flow\n\nEquivalent to Refloat `{id}` using `{repo}`.\n",
        id = doc.id,
        repo = poc.repo,
    );
    let _ = writeln!(
        out,
        "## Build\n\n```makefile\nmake {target}\n```\n",
        target = poc.makefile_target,
    );
    let _ = writeln!(
        out,
        "Packer: {packer}\n\nReference: `{repo}/{doc}`\n",
        packer = poc.packer,
        repo = poc.repo,
        doc = poc.doc,
    );

    append_attribution(&mut out, &poc.repo, &poc.doc, None);
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

fn append_attribution(out: &mut String, repo: &str, path: &str, line: Option<u64>) {
    let _ = writeln!(out, "\n---");
    match line {
        Some(line_no) => {
            let _ = writeln!(out, "Source: {repo}/{path}#L{line_no}");
        }
        None => {
            let _ = writeln!(out, "Source: {repo}/{path}");
        }
    }
    let _ = writeln!(out, "Source: catalog/{BUILD_FLOW_CATALOG_REL}");
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
        assert!(doc.poc_equivalent.is_some());
    }

    #[test]
    fn render_refloat_includes_pkgdesc_command() {
        let doc = load_build_flow(&default_catalog_root()).expect("load");
        let body = render_refloat_vesc_tool(&doc);
        assert!(body.contains("--buildPkgFromDesc"));
    }
}
