use std::collections::BTreeSet;

use vesc_knowledge_index::{
    Category, Chunk, ContentDigest, CorpusManifest, CorpusVersion, IndexEntry, LicenseStatus,
    NormalizedDocument, RepositoryId, ResourceUri, Revision, SchemaVersion, SourceInventory,
    SourceKind, SourceRef, SourceSpan, TrustTier, validate_chunk_adjacency,
};

#[test]
fn legacy_entry_migration_preserves_exact_name_and_id() {
    let entry = IndexEntry {
        id: "vesc_c_if.lbm_add_extension".into(),
        name: "lbm_add_extension".into(),
        category: Category::FirmwareApi,
        summary: "Register a native extension".into(),
        source: SourceRef {
            repo: "vesc".into(),
            path: "lispBM/c_libs/vesc_c_if.h".into(),
            line: 42,
        },
        keywords: vec!["extension".into()],
    };

    let document = NormalizedDocument::from_legacy(&entry).expect("migration");
    let chunk = document.legacy_chunk().expect("legacy chunk");

    assert_eq!(document.schema, SchemaVersion { major: 1, minor: 0 });
    assert_eq!(chunk.schema, SchemaVersion { major: 1, minor: 0 });
    assert_eq!(document.legacy_ids, vec![entry.id]);
    assert_eq!(document.category, Some(Category::FirmwareApi));
    assert_eq!(chunk.text, entry.summary);
    assert_eq!(
        chunk.source_span,
        Some(SourceSpan::new(42, 42, None, None).expect("span"))
    );
}

#[test]
fn manifest_json_is_byte_stable() {
    let first = CorpusManifest::new(
        CorpusVersion::try_from("v1").expect("version"),
        vec![
            "doc-b".try_into().expect("doc"),
            "doc-a".try_into().expect("doc"),
        ],
        vec![
            "chunk-b".try_into().expect("chunk"),
            "chunk-a".try_into().expect("chunk"),
        ],
    );
    let second = CorpusManifest::new(
        CorpusVersion::try_from("v1").expect("version"),
        vec![
            "doc-a".try_into().expect("doc"),
            "doc-b".try_into().expect("doc"),
        ],
        vec![
            "chunk-a".try_into().expect("chunk"),
            "chunk-b".try_into().expect("chunk"),
        ],
    );

    assert_eq!(first.validate(), Ok(()));
    assert_eq!(second.validate(), Ok(()));
    assert_eq!(
        first.canonical_json().expect("json"),
        second.canonical_json().expect("json")
    );
}

#[test]
fn compact_manifest_serializes_counts_without_id_inventories() {
    let manifest = CorpusManifest::from_inventory(
        CorpusVersion::try_from("git-full-history-v1").expect("version"),
        123,
        456,
        ContentDigest::of(b"inventory"),
    );
    let json: serde_json::Value =
        serde_json::from_slice(&manifest.canonical_json().expect("json")).expect("value");

    assert_eq!(manifest.document_count(), 123);
    assert_eq!(manifest.chunk_count(), 456);
    assert_eq!(manifest.schema, SchemaVersion { major: 1, minor: 1 });
    assert!(manifest.validate().is_ok());
    assert!(json.get("documents").is_none());
    assert!(json.get("chunks").is_none());
    assert_eq!(json["document_count"], 123);
    assert_eq!(json["chunk_count"], 456);
}

#[test]
fn manifest_rejects_duplicate_chunk_ids() {
    let manifest = CorpusManifest::new(
        CorpusVersion::try_from("v1").expect("version"),
        vec!["doc-a".try_into().expect("doc")],
        vec![
            "chunk-a".try_into().expect("chunk"),
            "chunk-a".try_into().expect("chunk"),
        ],
    );
    assert!(manifest.validate().is_err());
}

#[test]
fn artifact_manifest_rejects_unsafe_source_inventory_path() {
    let output = tempfile::tempdir().expect("tempdir");
    let mut manifest = vesc_knowledge_index::build_embedded_artifacts(output.path())
        .expect("build")
        .manifest;
    manifest.sources.push(SourceInventory {
        relative_path: "../secret.md".into(),
        title: "secret".into(),
        repository: RepositoryId::try_from("repo").expect("repo"),
        revision: Revision::try_from("rev").expect("revision"),
        media_type: "text/markdown".into(),
        source_kind: SourceKind::Markdown,
        trust_tier: TrustTier::FirstParty,
        license: LicenseStatus::InRepo,
        required: true,
        byte_count: Some(1),
        content_digest: Some(ContentDigest::of(b"x")),
        document_count: 1,
        rejection: None,
    });
    assert!(manifest.validate().is_err());
}

#[test]
fn ids_change_with_identity_and_content() {
    let base = NormalizedDocument::new(
        "Title",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "docs/title.md",
        "text/markdown",
        "one",
    )
    .expect("document");
    let changed = NormalizedDocument::new(
        "Title",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "docs/title.md",
        "text/markdown",
        "two",
    )
    .expect("document");
    assert_ne!(base.document_id, changed.document_id);
    assert_ne!(
        Chunk::from_document(&base, 0, "one".into(), Vec::new(), None)
            .expect("chunk")
            .chunk_id,
        Chunk::from_document(&base, 0, "two".into(), Vec::new(), None)
            .expect("chunk")
            .chunk_id
    );
}

#[test]
fn schema_and_digest_validation_reject_unknown_values() {
    let error = serde_json::from_str::<SchemaVersion>(r#"{"major":2,"minor":0}"#)
        .expect("schema parses before compatibility check")
        .ensure_major(SchemaVersion { major: 1, minor: 0 }, "corpus")
        .expect_err("unknown major");
    assert!(
        error
            .to_string()
            .contains("unsupported corpus schema major 2")
    );
    assert!(serde_json::from_str::<vesc_knowledge_index::ContentDigest>("\"sha256:bad\"").is_err());
}

#[test]
fn document_rejects_absolute_and_parent_paths() {
    let absolute = NormalizedDocument::new(
        "Title",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "/tmp/title.md",
        "text/markdown",
        "content",
    );
    let parent = NormalizedDocument::new(
        "Title",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "docs/../title.md",
        "text/markdown",
        "content",
    );
    assert!(absolute.is_err());
    assert!(parent.is_err());
}

#[test]
fn chunk_validation_rejects_digest_and_adjacency_corruption() {
    let document = NormalizedDocument::new(
        "Title",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("rev"),
        "docs/title.md",
        "text/markdown",
        "content",
    )
    .expect("document");
    let mut chunk =
        Chunk::from_document(&document, 0, "content".into(), Vec::new(), None).expect("chunk");
    chunk.text = "tampered".into();
    assert!(chunk.validate().is_err());

    let mut chunk =
        Chunk::from_document(&document, 0, "content".into(), Vec::new(), None).expect("chunk");
    chunk.next_chunk = Some(chunk.chunk_id.clone());
    assert!(validate_chunk_adjacency(&[chunk]).is_err());
}

#[test]
fn corpus_contracts_roundtrip_and_validate_uri() {
    let uri = ResourceUri::try_from("vesc://knowledge/chunk/chunk-a").expect("uri");
    assert_eq!(uri.as_str(), "vesc://knowledge/chunk/chunk-a");
    assert!(ResourceUri::try_from("not a uri").is_err());

    let document = NormalizedDocument::new(
        "Title",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "docs/title.md",
        "text/markdown",
        "A passage",
    )
    .expect("document");
    let mut chunk =
        Chunk::from_document(&document, 0, "A passage".into(), vec!["Title".into()], None)
            .expect("chunk");
    chunk.resource_uri = Some(uri);

    let json = serde_json::to_string(&chunk).expect("serialize");
    let roundtrip: Chunk = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(roundtrip, chunk);

    let _ = (
        BTreeSet::<String>::new(),
        LicenseStatus::InRepo,
        SchemaVersion { major: 1, minor: 0 },
        TrustTier::FirstParty,
    );
}
