//! Static doc topic MCP resources (`include_str!` snippets + catalog-backed topics).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::attribution::{SourceRef, append_source_footer};
use super::{
    ParsedResourceUri, ResourceMeta, ResourceReadError, ResourceReadHandler, ResourceRegistry,
    ResourceRegistryError,
};

/// `vesc://catalog/doc/topic/pkgdesc_dialects`
pub const PKGDESC_DIALECTS_URI: &str = "vesc://catalog/doc/topic/pkgdesc_dialects";

/// `vesc://catalog/doc/topic/vesc_c_if`
pub const VESC_C_IF_URI: &str = "vesc://catalog/doc/topic/vesc_c_if";

/// `vesc://catalog/doc/topic/lisp_imports`
pub const LISP_IMPORTS_URI: &str = "vesc://catalog/doc/topic/lisp_imports";

/// `vesc://catalog/doc/topic/vescpackage_reference`
pub const VESCPACKAGE_REFERENCE_URI: &str = "vesc://catalog/doc/topic/vescpackage_reference";

const VESC_C_IF_CATALOG_REL: &str = "bldc/vesc_c_if.yaml";

const PKGDESC_DIALECTS_BODY: &str = include_str!("snippets/pkgdesc_dialects.md");
const LISP_IMPORTS_BODY: &str = include_str!("snippets/lisp_imports.md");
const VESCPACKAGE_REFERENCE_BODY: &str = include_str!("snippets/vescpackage_reference.md");

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct VescCIfCatalog {
    source_repo: String,
    header: VescCIfHeader,
    compatibility: VescCIfCompatibility,
    function_groups: Vec<FunctionGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct VescCIfHeader {
    path: String,
    #[serde(rename = "struct")]
    struct_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct VescCIfCompatibility {
    rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FunctionGroup {
    id: String,
    #[serde(default)]
    lines: Option<[u64; 2]>,
    symbols: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
}

/// Register static doc topic resource metadata in the registry.
///
/// # Errors
///
/// Returns [`ResourceRegistryError`] when a URI is invalid or already registered.
pub fn register_doc_topic_resources(
    registry: &mut ResourceRegistry,
) -> Result<(), ResourceRegistryError> {
    registry.register(ResourceMeta {
        uri: PKGDESC_DIALECTS_URI.into(),
        name: "pkgdesc dialect guide".into(),
        description: Some("vesc_tool vs legacy POC pkgdesc.qml property schemas".into()),
        mime_type: "text/markdown".into(),
    })?;
    registry.register(ResourceMeta {
        uri: VESC_C_IF_URI.into(),
        name: "vesc_c_if LBM core overview".into(),
        description: Some("LispBM extension surface from bldc vesc_c_if.h".into()),
        mime_type: "text/markdown".into(),
    })?;
    registry.register(ResourceMeta {
        uri: LISP_IMPORTS_URI.into(),
        name: "lispData import table".into(),
        description: Some("Wire format for embedded native payloads in lispData".into()),
        mime_type: "text/markdown".into(),
    })?;
    registry.register(ResourceMeta {
        uri: VESCPACKAGE_REFERENCE_URI.into(),
        name: "VESC package lifecycle reference".into(),
        description: Some(
            "End-to-end pkgdesc → wire → native ABI index with sharp edges and MCP integration"
                .into(),
        ),
        mime_type: "text/markdown".into(),
    })
}

/// Read a doc topic resource body by URI.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the URI is unknown or catalog load fails.
pub fn read_doc_topic(uri: &str, catalog_root: &Path) -> Result<String, ResourceReadError> {
    match uri {
        PKGDESC_DIALECTS_URI => Ok(render_pkgdesc_dialects()),
        VESC_C_IF_URI => {
            render_vesc_c_if(catalog_root).map_err(|message| ResourceReadError::ReadFailed {
                uri: uri.into(),
                message,
            })
        }
        LISP_IMPORTS_URI => Ok(render_lisp_imports()),
        VESCPACKAGE_REFERENCE_URI => Ok(render_vescpackage_reference()),
        other => Err(ResourceReadError::NotFound { uri: other.into() }),
    }
}

fn render_pkgdesc_dialects() -> String {
    let mut out = PKGDESC_DIALECTS_BODY.to_owned();
    append_source_footer(
        &mut out,
        &[
            SourceRef::new("vesc-mcp", "catalog/gap-analysis.md").with_line(16),
            SourceRef::literal("catalog/gap-analysis.md"),
        ],
    );
    out
}

fn render_lisp_imports() -> String {
    let mut out = LISP_IMPORTS_BODY.to_owned();
    append_source_footer(
        &mut out,
        &[
            SourceRef::new("vesc_tool", "codeloader.cpp").with_line(173),
            SourceRef::literal("crates/vesc-domain/src/wire/mod.rs"),
        ],
    );
    out
}

fn render_vescpackage_reference() -> String {
    let mut out = VESCPACKAGE_REFERENCE_BODY.to_owned();
    append_source_footer(
        &mut out,
        &[
            SourceRef::literal("docs/vescpackage-reference.md"),
            SourceRef::literal("docs/vescpkg-wire-format.md"),
            SourceRef::literal("docs/vesc-pkg-lib-abi.md"),
        ],
    );
    out
}

fn render_vesc_c_if(catalog_root: &Path) -> Result<String, String> {
    let doc = load_vesc_c_if_catalog(catalog_root)?;
    let group = doc
        .function_groups
        .iter()
        .find(|group| group.id == "lbm_core")
        .ok_or_else(|| {
            format!("missing function_groups.lbm_core in catalog/{VESC_C_IF_CATALOG_REL}")
        })?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# vesc_c_if — LispBM core API\n\nNative VESC packages extend firmware via the `{struct_name}` function-pointer table in `{path}`.\n",
        struct_name = doc.header.struct_name,
        path = doc.header.path,
    );
    let _ = writeln!(out, "## Compatibility\n\n{}\n", doc.compatibility.rule);

    if let Some(notes) = &group.notes {
        let _ = writeln!(out, "{notes}\n");
    }

    let _ = writeln!(out, "## `lbm_core` symbols\n");
    for symbol in &group.symbols {
        let _ = writeln!(out, "- `{symbol}`");
    }
    let _ = writeln!(out);

    let line = group.lines.map(|lines| lines[0]);
    let sources = vec![
        if let Some(line_no) = line {
            SourceRef::new(&doc.source_repo, &doc.header.path).with_line(line_no)
        } else {
            SourceRef::new(&doc.source_repo, &doc.header.path)
        },
        SourceRef::literal(format!("catalog/{VESC_C_IF_CATALOG_REL}")),
    ];
    append_source_footer(&mut out, &sources);
    Ok(out)
}

fn load_vesc_c_if_catalog(catalog_root: &Path) -> Result<VescCIfCatalog, String> {
    let path = catalog_root.join(VESC_C_IF_CATALOG_REL);
    let content =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|err| format!("parse {}: {err}", path.display()))
}

/// Handler dispatching catalog doc topic URIs.
#[derive(Debug, Clone)]
pub struct DocTopicResourceHandler {
    catalog_root: PathBuf,
}

impl DocTopicResourceHandler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            catalog_root: repo_catalog_root(),
        }
    }
}

impl Default for DocTopicResourceHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceReadHandler for DocTopicResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::Catalog(catalog) if catalog.kind == "doc"
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        read_doc_topic(&uri.to_uri(), &self.catalog_root)
    }
}

fn repo_catalog_root() -> PathBuf {
    crate::workspace::catalog_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_vesc_c_if_catalog_parses_fixture() {
        let doc = load_vesc_c_if_catalog(&repo_catalog_root()).expect("load catalog");
        assert_eq!(doc.source_repo, "bldc");
        assert!(
            doc.function_groups
                .iter()
                .any(|group| group.id == "lbm_core")
        );
    }
}
