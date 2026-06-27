//! Catalog-backed refloat command doc MCP resources (`text/markdown`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{
    ParsedResourceUri, ResourceMeta, ResourceReadError, ResourceReadHandler, ResourceRegistry,
    ResourceRegistryError,
};

/// Relative path to the refloat commands catalog document.
pub const REFLOAT_COMMANDS_CATALOG_REL: &str = "refloat/commands.yaml";

/// `vesc://catalog/commands/refloat/REALTIME_DATA`
pub const REALTIME_DATA_COMMAND_URI: &str = "vesc://catalog/commands/refloat/REALTIME_DATA";

const REFLOAT_COMMAND_URI_PREFIX: &str = "vesc://catalog/commands/refloat/";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RefloatCommandsCatalog {
    source_repo: String,
    public_commands: Vec<CommandEntry>,
    #[serde(default)]
    internal_commands: Vec<CommandEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct CommandEntry {
    name: String,
    command_id: u32,
    path: String,
    summary: String,
    status: String,
}

/// Build the resource URI for a refloat command name from the catalog index.
#[must_use]
pub fn refloat_command_uri(command: &str) -> String {
    format!("{REFLOAT_COMMAND_URI_PREFIX}{command}")
}

/// Load `catalog/refloat/commands.yaml` from a catalog root directory.
///
/// # Errors
///
/// Returns I/O or YAML parse errors, or when the file is missing.
fn load_refloat_commands_catalog(catalog_root: &Path) -> Result<RefloatCommandsCatalog, String> {
    let path = catalog_root.join(REFLOAT_COMMANDS_CATALOG_REL);
    let content =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_yaml::from_str(&content).map_err(|err| format!("parse {}: {err}", path.display()))
}

/// Register static refloat command doc resources from the catalog index.
///
/// # Errors
///
/// Returns [`ResourceRegistryError`] when a URI is invalid or already registered.
pub fn register_refloat_command_resources(
    registry: &mut ResourceRegistry,
    catalog_root: &Path,
) -> Result<(), ResourceRegistryError> {
    let doc = load_refloat_commands_catalog(catalog_root).map_err(|message| {
        ResourceRegistryError::InvalidUri {
            uri: REFLOAT_COMMANDS_CATALOG_REL.into(),
            source: super::ResourceUriError {
                uri: REFLOAT_COMMANDS_CATALOG_REL.into(),
                reason: message,
            },
        }
    })?;

    for entry in doc
        .public_commands
        .iter()
        .chain(doc.internal_commands.iter())
    {
        registry.register(ResourceMeta {
            uri: refloat_command_uri(&entry.name),
            name: format!("Refloat command {}", entry.name),
            description: Some(entry.summary.clone()),
            mime_type: "text/markdown".into(),
        })?;
    }

    Ok(())
}

/// Read a refloat command doc resource body by URI.
///
/// # Errors
///
/// Returns [`ResourceReadError`] when the URI is unknown, the command is missing from the
/// catalog, or the source doc cannot be read.
pub fn read_refloat_command(
    uri: &str,
    catalog_root: &Path,
    refloat_root: &Path,
) -> Result<String, ResourceReadError> {
    let command = parse_refloat_command_name(uri)
        .ok_or_else(|| ResourceReadError::NotFound { uri: uri.into() })?;

    let doc = load_refloat_commands_catalog(catalog_root).map_err(|message| {
        ResourceReadError::ReadFailed {
            uri: uri.into(),
            message,
        }
    })?;

    let entry = find_command_entry(&doc, command)
        .ok_or_else(|| ResourceReadError::NotFound { uri: uri.into() })?;

    let doc_path = refloat_root.join(&entry.path);
    let content =
        std::fs::read_to_string(&doc_path).map_err(|err| ResourceReadError::ReadFailed {
            uri: uri.into(),
            message: format!("read {}: {err}", doc_path.display()),
        })?;

    let first_paragraph =
        extract_first_paragraph(&content).unwrap_or_else(|| entry.summary.clone());
    Ok(render_refloat_command(
        &doc.source_repo,
        entry,
        &first_paragraph,
    ))
}

fn parse_refloat_command_name(uri: &str) -> Option<&str> {
    uri.strip_prefix(REFLOAT_COMMAND_URI_PREFIX)
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

fn find_command_entry<'a>(
    doc: &'a RefloatCommandsCatalog,
    command: &str,
) -> Option<&'a CommandEntry> {
    doc.public_commands
        .iter()
        .chain(doc.internal_commands.iter())
        .find(|entry| entry.name.eq_ignore_ascii_case(command))
}

fn extract_first_paragraph(markdown: &str) -> Option<String> {
    let mut paragraph = String::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                return Some(paragraph.trim().to_string());
            }
            continue;
        }
        if trimmed.starts_with('#')
            || trimmed.starts_with('|')
            || trimmed.starts_with("---")
            || (trimmed.starts_with("**") && trimmed.contains(':'))
        {
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    if paragraph.is_empty() {
        None
    } else {
        Some(paragraph.trim().to_string())
    }
}

fn render_refloat_command(repo: &str, entry: &CommandEntry, summary: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {name}\n\n**Command ID**: {id}\n**Status**: {status}\n\n{summary}\n",
        name = entry.name,
        id = entry.command_id,
        status = entry.status,
        summary = summary,
    );
    append_attribution(&mut out, repo, &entry.path);
    out
}

fn append_attribution(out: &mut String, repo: &str, path: &str) {
    let _ = writeln!(out, "\n---");
    let _ = writeln!(out, "Source: {repo}/{path}");
    let _ = writeln!(out, "Source: catalog/{REFLOAT_COMMANDS_CATALOG_REL}");
}

/// Handler dispatching catalog refloat command doc URIs.
#[derive(Debug, Clone)]
pub struct RefloatCommandResourceHandler {
    catalog_root: PathBuf,
    refloat_root: PathBuf,
}

impl RefloatCommandResourceHandler {
    #[must_use]
    pub fn new() -> Self {
        let catalog_root = repo_catalog_root();
        let refloat_root = crate::RepoRoots::from_env().refloat;
        Self {
            catalog_root,
            refloat_root,
        }
    }
}

impl Default for RefloatCommandResourceHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceReadHandler for RefloatCommandResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(
            uri,
            ParsedResourceUri::Catalog(catalog)
                if catalog.kind == "commands"
                    && catalog.id.starts_with("refloat/")
                    && catalog.id.len() > "refloat/".len()
        )
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        read_refloat_command(&uri.to_uri(), &self.catalog_root, &self.refloat_root)
    }
}

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_refloat_commands_catalog_parses_fixture() {
        let doc = load_refloat_commands_catalog(&repo_catalog_root()).expect("load catalog");
        assert_eq!(doc.source_repo, "refloat");
        assert!(
            doc.public_commands
                .iter()
                .any(|entry| entry.name == "REALTIME_DATA")
        );
    }

    #[test]
    fn extract_first_paragraph_skips_title_and_metadata() {
        let markdown =
            "# Command: INFO\n\n**ID**: 0\n\nFirst paragraph here.\n\nSecond paragraph.\n";
        assert_eq!(
            extract_first_paragraph(markdown).as_deref(),
            Some("First paragraph here.")
        );
    }
}
