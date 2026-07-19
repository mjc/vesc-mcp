//! Extract repo-relative path references from catalog YAML documents.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml::Value;

use super::CatalogRepo;

/// A repo-relative path cited by a catalog YAML file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogPathRef {
    pub repo: CatalogRepo,
    pub path: String,
    pub catalog_file: PathBuf,
}

/// Walk `catalog/` and collect path references tagged with `source_repo`.
///
/// # Errors
///
/// Returns I/O or YAML parse errors, or when a catalog file lacks `source_repo`.
pub fn collect_catalog_path_refs(catalog_root: &Path) -> Result<Vec<CatalogPathRef>, String> {
    let mut refs = BTreeSet::new();
    for entry in walk_yaml_files(catalog_root)? {
        let content =
            fs::read_to_string(&entry).map_err(|e| format!("read {}: {e}", entry.display()))?;
        let value: Value = serde_yaml::from_str(&content)
            .map_err(|e| format!("parse {}: {e}", entry.display()))?;
        let repo = source_repo_from_value(&value).ok_or_else(|| {
            format!(
                "missing source_repo in {}",
                entry.strip_prefix(catalog_root).unwrap_or(&entry).display()
            )
        })?;
        collect_paths_from_value(&value, repo, &entry, &mut refs);
    }
    Ok(refs.into_iter().collect())
}

fn walk_yaml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk_yaml_files(&path)?);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml")
        {
            if path.file_name().is_some_and(|name| name == "schema.yaml") {
                continue;
            }
            files.push(path);
        }
    }
    Ok(files)
}

fn source_repo_from_value(value: &Value) -> Option<CatalogRepo> {
    let name = value.get("source_repo")?.as_str()?;
    match name {
        "refloat" => Some(CatalogRepo::Refloat),
        "vesc" => Some(CatalogRepo::Vesc),
        "vesc-rust-poc" => Some(CatalogRepo::Poc),
        "vesc_tool" => Some(CatalogRepo::VescTool),
        "vesc-mcp" => Some(CatalogRepo::VescMcp),
        _ => None,
    }
}

fn collect_paths_from_value(
    value: &Value,
    repo: CatalogRepo,
    catalog_file: &Path,
    out: &mut BTreeSet<CatalogPathRef>,
) {
    match value {
        Value::Mapping(map) => {
            for (key, val) in map {
                if key.as_str() == Some("path") {
                    if let Some(path_str) = val.as_str() {
                        if is_repo_relative_path(path_str) {
                            out.insert(CatalogPathRef {
                                repo,
                                path: path_str.to_string(),
                                catalog_file: catalog_file.to_path_buf(),
                            });
                        }
                    }
                }
                collect_paths_from_value(val, repo, catalog_file, out);
            }
        }
        Value::Sequence(seq) => {
            for item in seq {
                collect_paths_from_value(item, repo, catalog_file, out);
            }
        }
        _ => {}
    }
}

fn is_repo_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with("catalog/")
        && !path.starts_with("http")
        && path.contains('/')
}

/// Collect unique catalog document ids; returns duplicates if any.
///
/// # Errors
///
/// Returns I/O or YAML parse errors while scanning catalog files.
pub fn find_duplicate_catalog_ids(catalog_root: &Path) -> Result<Vec<String>, String> {
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    let mut duplicates = Vec::new();
    for entry in walk_yaml_files(catalog_root)? {
        let content =
            fs::read_to_string(&entry).map_err(|e| format!("read {}: {e}", entry.display()))?;
        let value: Value = serde_yaml::from_str(&content)
            .map_err(|e| format!("parse {}: {e}", entry.display()))?;
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            if let Some(first) = seen.insert(id.to_string(), entry.clone()) {
                duplicates.push(format!("{id}: {} and {}", first.display(), entry.display()));
            }
        }
    }
    duplicates.sort();
    Ok(duplicates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_repo_relative_path_rejects_catalog_and_urls() {
        assert!(!is_repo_relative_path("catalog/refloat/build-flow.yaml"));
        assert!(!is_repo_relative_path("https://example.com/x"));
        assert!(is_repo_relative_path("lispBM/c_libs/vesc_c_if.h"));
    }
}
