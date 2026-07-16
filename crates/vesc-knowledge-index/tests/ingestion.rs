use std::fs;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::ingest::ingest_allowlisted;
use vesc_knowledge_index::{
    IngestionError, IngestionReport, LicenseStatus, RepositoryId, Revision, SourceKind, SourceSpec,
    TrustTier,
};

fn spec(path: &str, required: bool) -> SourceSpec {
    SourceSpec {
        relative_path: path.into(),
        title: "Example".into(),
        media_type: "text/markdown".into(),
        source_kind: SourceKind::Markdown,
        trust_tier: TrustTier::FirstParty,
        license: LicenseStatus::InRepo,
        required,
        max_bytes: 1024,
        source_repository: None,
        source_revision: None,
    }
}

#[test]
fn ingestion_normalizes_crlf_and_records_repo_relative_provenance() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("README.md"), "# Title\r\n\r\nBody\r\n").expect("write");

    let report: IngestionReport = ingest_allowlisted(
        root.path(),
        &RepositoryId::try_from("vesc-mcp").expect("repo"),
        &Revision::try_from("rev").expect("revision"),
        &[spec("README.md", true)],
    )
    .expect("ingest");

    assert_eq!(report.documents.len(), 1);
    assert_eq!(report.documents[0].path, "README.md");
    assert_eq!(report.documents[0].content, "# Title\n\nBody\n");
    assert!(report.documents[0].canonical_uri.is_some());
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].document_count, 1);
    assert!(report.sources[0].content_digest.is_some());
    assert!(report.sources[0].rejection.is_none());
}

#[test]
fn ingestion_rejects_parent_traversal_and_symlink_escape() {
    let root = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside");
    fs::write(outside.path().join("secret.md"), "secret").expect("write");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        root.path().join("link.md"),
    )
    .expect("symlink");

    let parent = ingest_allowlisted(
        root.path(),
        &RepositoryId::try_from("repo").expect("repo"),
        &Revision::try_from("rev").expect("revision"),
        &[spec("../secret.md", true)],
    )
    .expect_err("parent traversal");
    assert!(matches!(
        parent,
        IngestionError::RequiredSourcesRejected { .. }
    ));

    #[cfg(unix)]
    let symlink = ingest_allowlisted(
        root.path(),
        &RepositoryId::try_from("repo").expect("repo"),
        &Revision::try_from("rev").expect("revision"),
        &[spec("link.md", true)],
    )
    .expect_err("symlink escape");
    #[cfg(unix)]
    assert!(matches!(
        symlink,
        IngestionError::RequiredSourcesRejected { .. }
    ));
}

#[test]
fn optional_source_rejection_is_reported_without_failing_build() {
    let root = tempdir().expect("tempdir");
    let report = ingest_allowlisted(
        root.path(),
        &RepositoryId::try_from("repo").expect("repo"),
        &Revision::try_from("rev").expect("revision"),
        &[spec("missing.md", false)],
    )
    .expect("optional source");

    assert!(report.documents.is_empty());
    assert_eq!(report.rejected.len(), 1);
    assert!(!report.rejected[0].required);
}

#[test]
fn oversized_source_is_rejected_before_reading_content() {
    let root = tempdir().expect("tempdir");
    fs::write(root.path().join("large.md"), "0123456789").expect("write");
    let mut source = spec("large.md", true);
    source.max_bytes = 4;

    let error = ingest_allowlisted(
        root.path(),
        &RepositoryId::try_from("repo").expect("repo"),
        &Revision::try_from("rev").expect("revision"),
        &[source],
    )
    .expect_err("oversized source");
    assert!(matches!(
        error,
        IngestionError::RequiredSourcesRejected { .. }
    ));
}

#[test]
fn ingestion_catalog_records_preserve_field_anchors() {
    let root = tempdir().expect("tempdir");
    fs::write(
        root.path().join("commands.yaml"),
        "public_commands:\n  - name: INFO\n    command_id: 0\n  - name: REMOTE\n    command_id: 15\n",
    )
    .expect("write");
    let mut source = spec("commands.yaml", true);
    source.title = "Commands".into();
    source.media_type = "application/yaml".into();
    source.source_kind = SourceKind::CatalogYaml;

    let report = ingest_allowlisted(
        root.path(),
        &RepositoryId::try_from("repo").expect("repo"),
        &Revision::try_from("rev").expect("revision"),
        &[source],
    )
    .expect("catalog ingestion");

    assert_eq!(report.documents.len(), 2);
    assert_eq!(report.documents[0].path, "commands.yaml#public_commands[0]");
    assert!(report.documents[0].identifiers.contains("INFO"));
    assert!(report.documents[0].source_span.is_some());
}
