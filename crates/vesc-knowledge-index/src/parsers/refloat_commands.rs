//! Parse `catalog/refloat/commands.yaml` command docs into index entries.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::{Category, IndexEntry, SourceRef};

/// Relative path from the catalog root to the refloat commands catalog document.
pub const CATALOG_REL_PATH: &str = "refloat/commands.yaml";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RefloatCommandsCatalog {
    source_repo: String,
    public_commands: Vec<CommandEntry>,
    #[serde(default)]
    internal_commands: Vec<CommandEntry>,
    #[serde(default)]
    supporting_docs: Vec<SupportingDocEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct CommandEntry {
    name: String,
    command_id: u32,
    path: String,
    summary: String,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct SupportingDocEntry {
    path: String,
    summary: String,
}

/// Errors while parsing refloat command docs into index entries.
#[derive(Debug, Error)]
pub enum RefloatCommandsParseError {
    /// Failed to read a file from disk.
    #[error("read {path}: {source}")]
    Io {
        /// Path that could not be read.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to deserialize catalog YAML.
    #[error("parse catalog YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Load refloat command docs from catalog YAML and upstream markdown.
///
/// # Errors
///
/// Returns [`RefloatCommandsParseError`] when the catalog or a referenced doc is missing.
pub fn parse_catalog(
    catalog_root: &Path,
    refloat_root: &Path,
) -> Result<Vec<IndexEntry>, RefloatCommandsParseError> {
    let catalog = load_catalog(catalog_root)?;
    entries_from_catalog(&catalog, refloat_root)
}

fn load_catalog(catalog_root: &Path) -> Result<RefloatCommandsCatalog, RefloatCommandsParseError> {
    let path = catalog_root.join(CATALOG_REL_PATH);
    let content =
        std::fs::read_to_string(&path).map_err(|source| RefloatCommandsParseError::Io {
            path: path.display().to_string(),
            source,
        })?;
    Ok(serde_yaml::from_str(&content)?)
}

fn entries_from_catalog(
    catalog: &RefloatCommandsCatalog,
    refloat_root: &Path,
) -> Result<Vec<IndexEntry>, RefloatCommandsParseError> {
    let mut entries = Vec::new();

    for command in catalog
        .public_commands
        .iter()
        .chain(catalog.internal_commands.iter())
    {
        entries.push(entry_from_command(catalog, command, refloat_root)?);
    }

    for doc in &catalog.supporting_docs {
        entries.push(entry_from_supporting_doc(catalog, doc, refloat_root)?);
    }

    Ok(entries)
}

fn entry_from_command(
    catalog: &RefloatCommandsCatalog,
    command: &CommandEntry,
    refloat_root: &Path,
) -> Result<IndexEntry, RefloatCommandsParseError> {
    let summary = doc_summary(refloat_root, &command.path, &command.summary)?;
    Ok(IndexEntry {
        id: format!("refloat_command.{}", command.name),
        name: command.name.clone(),
        category: Category::RefloatCommand,
        summary,
        source: SourceRef {
            repo: catalog.source_repo.clone(),
            path: command.path.clone(),
            line: 1,
        },
        keywords: vec![
            "refloat_command".into(),
            command.name.clone(),
            command.status.clone(),
            command.command_id.to_string(),
        ],
    })
}

fn entry_from_supporting_doc(
    catalog: &RefloatCommandsCatalog,
    doc: &SupportingDocEntry,
    refloat_root: &Path,
) -> Result<IndexEntry, RefloatCommandsParseError> {
    let name = supporting_doc_name(&doc.path);
    let summary = doc_summary(refloat_root, &doc.path, &doc.summary)?;
    Ok(IndexEntry {
        id: format!("refloat_command.{name}"),
        name,
        category: Category::RefloatCommand,
        summary,
        source: SourceRef {
            repo: catalog.source_repo.clone(),
            path: doc.path.clone(),
            line: 1,
        },
        keywords: vec!["refloat_command".into(), "supporting_doc".into()],
    })
}

fn supporting_doc_name(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(path)
        .to_string()
}

fn doc_summary(
    refloat_root: &Path,
    rel_path: &str,
    catalog_fallback: &str,
) -> Result<String, RefloatCommandsParseError> {
    let doc_path = refloat_root.join(rel_path);
    let content =
        std::fs::read_to_string(&doc_path).map_err(|source| RefloatCommandsParseError::Io {
            path: doc_path.display().to_string(),
            source,
        })?;

    let title = extract_title(&content);
    let paragraph =
        extract_first_paragraph(&content).unwrap_or_else(|| catalog_fallback.to_string());

    Ok(match title {
        Some(title) if paragraph.is_empty() => title,
        Some(title) => format!("{title}. {paragraph}"),
        None => paragraph,
    })
}

fn extract_title(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches('#').trim().to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_title_reads_first_heading() {
        let markdown = "# Command: REALTIME_DATA\n\nBody.\n";
        assert_eq!(
            extract_title(markdown).as_deref(),
            Some("Command: REALTIME_DATA")
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

    #[test]
    fn doc_summary_combines_title_and_paragraph() {
        let dir = std::env::temp_dir().join(format!("vesc-refloat-doc-{}", std::process::id()));
        let doc_path = dir.join("doc/commands/TEST.md");
        std::fs::create_dir_all(doc_path.parent().unwrap()).unwrap();
        std::fs::write(&doc_path, "# Command: TEST\n\n**ID**: 1\n\nSummary body.\n").unwrap();

        let summary = doc_summary(&dir, "doc/commands/TEST.md", "fallback").expect("summary");
        assert_eq!(summary, "Command: TEST. Summary body.");

        let _ = std::fs::remove_dir_all(dir);
    }
}
