//! Explicit-root, allowlisted source ingestion.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;

use super::{
    ContentDigest, LicenseStatus, NormalizedDocument, RepositoryId, Revision, SourceKind, TrustTier,
};
use crate::corpus::CorpusError;

/// One explicitly approved repository-relative source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    pub relative_path: PathBuf,
    pub title: String,
    pub media_type: String,
    pub source_kind: SourceKind,
    pub trust_tier: TrustTier,
    pub license: LicenseStatus,
    pub required: bool,
    pub max_bytes: u64,
    #[serde(default)]
    pub source_repository: Option<RepositoryId>,
    #[serde(default)]
    pub source_revision: Option<Revision>,
}

/// The reproducible observation recorded for one allowlisted source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInventory {
    pub relative_path: PathBuf,
    pub title: String,
    pub repository: RepositoryId,
    pub revision: Revision,
    pub media_type: String,
    pub source_kind: SourceKind,
    pub trust_tier: TrustTier,
    pub license: LicenseStatus,
    pub required: bool,
    pub byte_count: Option<u64>,
    pub content_digest: Option<ContentDigest>,
    pub document_count: usize,
    pub rejection: Option<SourceRejection>,
}

/// A bounded, artifact-safe source rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRejection {
    pub source: String,
    pub code: String,
    pub message: String,
    pub required: bool,
}

/// Ingestion results in deterministic source-spec order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestionReport {
    pub documents: Vec<NormalizedDocument>,
    pub rejected: Vec<SourceRejection>,
    pub sources: Vec<SourceInventory>,
    /// Number of source entries examined, including entries later rejected by policy.
    #[serde(default)]
    pub visited_files: usize,
    #[cfg(feature = "git-corpus")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Non-identity profiling data; excluded from deterministic report equality.
    pub git_observations: Option<super::git::GitIngestionObservations>,
}

impl PartialEq for IngestionReport {
    fn eq(&self, other: &Self) -> bool {
        self.documents == other.documents
            && self.rejected == other.rejected
            && self.sources == other.sources
            && self.visited_files == other.visited_files
    }
}

impl Eq for IngestionReport {}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IngestionError {
    #[error("source root is not a directory")]
    InvalidRoot,
    #[error("required sources were rejected")]
    RequiredSourcesRejected { rejected: Vec<SourceRejection> },
    #[error(transparent)]
    Contract(#[from] CorpusError),
}

/// The reviewed, repository-relative v1 source inventory for this workspace.
///
/// This is deliberately an explicit list: adding a source requires changing
/// this function and reviewing its trust/license metadata.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn vesc_mcp_source_specs() -> Vec<SourceSpec> {
    let mut specs = Vec::new();
    for path in [
        "docs/architecture.md",
        "docs/configuration.md",
        "docs/rag-threat-model.md",
        "docs/safety.md",
        "docs/testing.md",
        "docs/troubleshooting.md",
        "docs/vesc-pkg-lib-abi.md",
        "docs/vescpackage-reference.md",
        "docs/vescpkg-wire-format.md",
        "docs/examples/build-native-lib-package-session.md",
        "docs/examples/inspect-refloat-session.md",
        "catalog/gap-analysis.md",
        "tests/fixtures/README.md",
        "tests/fixtures/refloat-minimal/package_README-gen.md",
        "tests/fixtures/native-lib-minimal/package/README.md",
    ] {
        specs.push(SourceSpec {
            relative_path: path.into(),
            title: path.to_owned(),
            media_type: "text/markdown".into(),
            source_kind: SourceKind::Markdown,
            trust_tier: TrustTier::FirstParty,
            license: LicenseStatus::InRepo,
            required: true,
            max_bytes: 4 * 1024 * 1024,
            source_repository: None,
            source_revision: None,
        });
    }
    for path in [
        "catalog/abi/minimal-test-package-abi.yaml",
        "catalog/bldc/native-lib-macros.yaml",
        "catalog/bldc/nvm.yaml",
        "catalog/bldc/vesc_c_if.yaml",
        "catalog/priorities.json",
        "catalog/refloat/build-flow.yaml",
        "catalog/refloat/commands.yaml",
        "catalog/refloat/lisp-loader.yaml",
        "catalog/refloat/native-lib.yaml",
        "catalog/schema.yaml",
    ] {
        let is_json = Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        specs.push(SourceSpec {
            relative_path: path.into(),
            title: path.to_owned(),
            media_type: if is_json {
                "application/json".into()
            } else {
                "application/yaml".into()
            },
            source_kind: if is_json {
                SourceKind::CatalogJson
            } else {
                SourceKind::CatalogYaml
            },
            trust_tier: TrustTier::FirstParty,
            license: LicenseStatus::InRepo,
            required: true,
            max_bytes: 4 * 1024 * 1024,
            source_repository: None,
            source_revision: None,
        });
    }
    for (path, title, media_type, repository) in [
        (
            "vendor/bldc/lispBM/c_libs/vesc_c_if.h",
            "bldc vesc_c_if.h",
            "text/x-c",
            "bldc",
        ),
        (
            "vendor/refloat/vesc_pkg_lib/README.md",
            "refloat native package ABI README",
            "text/markdown",
            "refloat",
        ),
        (
            "vendor/refloat/doc/commands/REALTIME_DATA.md",
            "refloat REALTIME_DATA command documentation",
            "text/markdown",
            "refloat",
        ),
    ] {
        specs.push(SourceSpec {
            relative_path: path.into(),
            title: title.into(),
            media_type: media_type.into(),
            source_kind: SourceKind::VendorFile,
            trust_tier: TrustTier::CuratedUpstream,
            license: LicenseStatus::Redistributable {
                spdx: "GPL-3.0-only".into(),
            },
            required: false,
            max_bytes: 4 * 1024 * 1024,
            source_repository: RepositoryId::try_from(repository).ok(),
            source_revision: None,
        });
    }
    specs
}

/// Reads only the supplied files beneath `root` and normalizes their text.
///
/// # Errors
///
/// Returns [`IngestionError::RequiredSourcesRejected`] when any required source
/// is missing, escapes the root, exceeds its bound, or is not valid UTF-8.
pub fn ingest_allowlisted(
    root: &Path,
    repository: &RepositoryId,
    revision: &Revision,
    specs: &[SourceSpec],
) -> Result<IngestionReport, IngestionError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|_| IngestionError::InvalidRoot)?;
    if !canonical_root.is_dir() {
        return Err(IngestionError::InvalidRoot);
    }

    let mut report = IngestionReport {
        documents: Vec::new(),
        rejected: Vec::new(),
        sources: Vec::new(),
        visited_files: specs.len(),
        #[cfg(feature = "git-corpus")]
        git_observations: None,
    };
    for spec in specs {
        match ingest_one(&canonical_root, repository, revision, spec) {
            Ok(source) => {
                report.documents.extend(source.documents.clone());
                report.sources.push(SourceInventory {
                    relative_path: spec.relative_path.clone(),
                    title: spec.title.clone(),
                    repository: source.repository.clone(),
                    revision: source.revision.clone(),
                    media_type: spec.media_type.clone(),
                    source_kind: spec.source_kind,
                    trust_tier: spec.trust_tier,
                    license: spec.license.clone(),
                    required: spec.required,
                    byte_count: Some(source.byte_count),
                    content_digest: Some(source.content_digest),
                    document_count: source.documents.len(),
                    rejection: None,
                });
            }
            Err(rejection) => {
                report.sources.push(SourceInventory {
                    relative_path: spec.relative_path.clone(),
                    title: spec.title.clone(),
                    repository: spec
                        .source_repository
                        .clone()
                        .unwrap_or_else(|| repository.clone()),
                    revision: spec
                        .source_revision
                        .clone()
                        .unwrap_or_else(|| revision.clone()),
                    media_type: spec.media_type.clone(),
                    source_kind: spec.source_kind,
                    trust_tier: spec.trust_tier,
                    license: spec.license.clone(),
                    required: spec.required,
                    byte_count: None,
                    content_digest: None,
                    document_count: 0,
                    rejection: Some(rejection.clone()),
                });
                report.rejected.push(rejection);
            }
        }
    }

    if report.rejected.iter().any(|rejection| rejection.required) {
        return Err(IngestionError::RequiredSourcesRejected {
            rejected: report.rejected,
        });
    }
    Ok(report)
}

fn ingest_one(
    root: &Path,
    repository: &RepositoryId,
    revision: &Revision,
    spec: &SourceSpec,
) -> Result<IngestedSource, SourceRejection> {
    let source = source_label(&spec.relative_path);
    let content = read_source(root, spec, &source)?;
    let content_digest = ContentDigest::of(content.as_bytes());
    let byte_count = content.len() as u64;
    let (source_repository, source_revision) = source_identity(root, repository, revision, spec);
    let documents = if is_structured_source(spec) {
        structured_documents(
            &source_repository,
            &source_revision,
            spec,
            &source,
            &content,
        )?
    } else {
        vec![build_document(
            &source_repository,
            &source_revision,
            spec,
            &source,
            content,
            None,
            None,
        )?]
    };
    Ok(IngestedSource {
        documents,
        byte_count,
        content_digest,
        repository: source_repository,
        revision: source_revision,
    })
}

struct IngestedSource {
    documents: Vec<NormalizedDocument>,
    byte_count: u64,
    content_digest: ContentDigest,
    repository: RepositoryId,
    revision: Revision,
}

fn source_identity(
    root: &Path,
    repository: &RepositoryId,
    revision: &Revision,
    spec: &SourceSpec,
) -> (RepositoryId, Revision) {
    let source_repository = spec
        .source_repository
        .clone()
        .unwrap_or_else(|| repository.clone());
    let source_revision = spec.source_revision.clone().or_else(|| {
        let source_root = root.join("vendor").join(source_repository.as_str());
        git_revision(&source_root)
    });
    (
        source_repository,
        source_revision.unwrap_or_else(|| revision.clone()),
    )
}

fn git_revision(root: &Path) -> Option<Revision> {
    let output = Command::new("git")
        .args(["-C", root.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?;
    Revision::try_from(revision.trim()).ok()
}

fn read_source(root: &Path, spec: &SourceSpec, source: &str) -> Result<String, SourceRejection> {
    if spec.relative_path.is_absolute()
        || spec
            .relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(rejection(
            spec,
            source,
            "path_escape",
            "source path is not repository-relative",
        ));
    }
    let candidate = root.join(&spec.relative_path);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| rejection(spec, source, "missing", "source does not exist"))?;
    if !canonical.starts_with(root) {
        return Err(rejection(
            spec,
            source,
            "path_escape",
            "source escapes approved root",
        ));
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|_| rejection(spec, source, "metadata", "source metadata is unavailable"))?;
    if !metadata.is_file() {
        return Err(rejection(
            spec,
            source,
            "not_file",
            "source is not a regular file",
        ));
    }
    if metadata.len() > spec.max_bytes {
        return Err(rejection(
            spec,
            source,
            "oversized",
            "source exceeds its byte bound",
        ));
    }
    let bytes = fs::read(&canonical)
        .map_err(|_| rejection(spec, source, "read", "source could not be read"))?;
    normalize_text_ref(&bytes)
        .map_err(|_| rejection(spec, source, "encoding", "source is not UTF-8"))
}

pub(super) fn normalize_text_ref(bytes: &[u8]) -> Result<String, std::str::Utf8Error> {
    std::str::from_utf8(bytes).map(|content| content.replace("\r\n", "\n"))
}

fn build_document(
    repository: &RepositoryId,
    revision: &Revision,
    spec: &SourceSpec,
    source: &str,
    content: String,
    anchor: Option<&str>,
    source_span: Option<super::SourceSpan>,
) -> Result<NormalizedDocument, SourceRejection> {
    let path = anchor.map_or_else(|| source.to_owned(), |anchor| format!("{source}#{anchor}"));
    let title = anchor.map_or_else(
        || spec.title.clone(),
        |anchor| format!("{}: {anchor}", spec.title),
    );
    let mut document = NormalizedDocument::new(
        title,
        spec.source_kind,
        repository.clone(),
        revision.clone(),
        path,
        spec.media_type.clone(),
        content,
    )
    .map_err(|error| rejection(spec, source, "contract", &error.to_string()))?;
    document.trust_tier = spec.trust_tier;
    document.license = spec.license.clone();
    document.source_span = source_span;
    document.canonical_uri = Some(
        format!("vesc://knowledge/document/{}", document.document_id)
            .try_into()
            .map_err(|error: CorpusError| {
                rejection(spec, source, "contract", &error.to_string())
            })?,
    );
    Ok(document)
}

fn is_structured_source(spec: &SourceSpec) -> bool {
    spec.media_type.ends_with("yaml") || spec.media_type.ends_with("json")
}

fn structured_documents(
    repository: &RepositoryId,
    revision: &Revision,
    spec: &SourceSpec,
    source: &str,
    content: &str,
) -> Result<Vec<NormalizedDocument>, SourceRejection> {
    let value = if spec.media_type.ends_with("json") {
        let json: serde_json::Value = serde_json::from_str(content)
            .map_err(|error| rejection(spec, source, "malformed_catalog", &error.to_string()))?;
        serde_yaml::to_value(json)
            .map_err(|error| rejection(spec, source, "malformed_catalog", &error.to_string()))?
    } else {
        serde_yaml::from_str(content)
            .map_err(|error| rejection(spec, source, "malformed_catalog", &error.to_string()))?
    };
    let records = structured_records(&value);
    records
        .into_iter()
        .map(|(anchor, record)| {
            let record_text = serde_yaml::to_string(&record).map_err(|error| {
                rejection(spec, source, "malformed_catalog", &error.to_string())
            })?;
            let line = content
                .find(anchor.split('[').next().unwrap_or(&anchor))
                .map_or(1, |offset| {
                    content[..offset]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1
                });
            let line = u32::try_from(line).unwrap_or(u32::MAX);
            let span = super::SourceSpan::new(line, line, None, None).ok();
            let mut document = build_document(
                repository,
                revision,
                spec,
                source,
                record_text,
                Some(&anchor),
                span,
            )?;
            document.identifiers.insert(anchor);
            if let YamlValue::Mapping(map) = &record {
                for key in ["id", "name", "command"] {
                    if let Some(YamlValue::String(value)) = map.get(YamlValue::String(key.into())) {
                        document.identifiers.insert(value.clone());
                    }
                }
            }
            Ok(document)
        })
        .collect()
}

fn structured_records(value: &YamlValue) -> Vec<(String, YamlValue)> {
    let YamlValue::Mapping(map) = value else {
        return vec![("root".into(), value.clone())];
    };
    let mut sequence_records = Vec::new();
    for (key, value) in map {
        let Some(key) = key.as_str() else { continue };
        if let YamlValue::Sequence(items) = value {
            for (index, item) in items.iter().enumerate() {
                sequence_records.push((format!("{key}[{index}]"), item.clone()));
            }
        }
    }
    if sequence_records.is_empty() {
        vec![("root".into(), value.clone())]
    } else {
        sequence_records.sort_by(|left, right| left.0.cmp(&right.0));
        sequence_records
    }
}

fn rejection(spec: &SourceSpec, source: &str, code: &str, message: &str) -> SourceRejection {
    SourceRejection {
        source: source.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        required: spec.required,
    }
}

fn source_label(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
