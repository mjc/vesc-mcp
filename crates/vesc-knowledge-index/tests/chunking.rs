use vesc_knowledge_index::{
    ChunkingConfig, NormalizedDocument, RepositoryId, Revision, SourceKind,
};

fn document(content: &str) -> NormalizedDocument {
    NormalizedDocument::new(
        "Example",
        SourceKind::Markdown,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "docs/example.md",
        "text/markdown",
        content,
    )
    .expect("document")
}

#[test]
fn markdown_heading_stays_with_first_paragraph() {
    let chunks = vesc_knowledge_index::chunk_markdown(
        &document("# Heading\n\nThe first paragraph."),
        ChunkingConfig {
            target_chars: 50,
            hard_max_chars: 50,
            minimum_chars: 1,
            ..ChunkingConfig::default()
        },
    )
    .expect("chunks");

    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains("# Heading"));
    assert!(chunks[0].text.contains("The first paragraph."));
    assert_eq!(chunks[0].heading_path, vec!["Heading"]);
}

#[test]
fn fenced_code_block_is_not_split_below_hard_limit() {
    let code = "x".repeat(40);
    let chunks = vesc_knowledge_index::chunk_markdown(
        &document(&format!("```c\n{code}\n```")),
        ChunkingConfig {
            target_chars: 10,
            hard_max_chars: 100,
            minimum_chars: 1,
            ..ChunkingConfig::default()
        },
    )
    .expect("chunks");

    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains(&code));
}

#[test]
fn oversized_paragraph_splits_on_utf8_boundaries_and_links_adjacency() {
    let content = "é ".repeat(80);
    let chunks = vesc_knowledge_index::chunk_markdown(
        &document(&content),
        ChunkingConfig {
            target_chars: 20,
            hard_max_chars: 30,
            minimum_chars: 1,
            ..ChunkingConfig::default()
        },
    )
    .expect("chunks");

    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|chunk| !chunk.text.trim().is_empty()));
    assert!(chunks.iter().all(|chunk| chunk.char_count <= 20));
    for pair in chunks.windows(2) {
        assert_eq!(pair[0].next_chunk.as_ref(), Some(&pair[1].chunk_id));
        assert_eq!(pair[1].previous_chunk.as_ref(), Some(&pair[0].chunk_id));
    }
}

#[test]
fn structured_record_remains_one_semantic_chunk() {
    let document = NormalizedDocument::new(
        "Commands: public_commands[0]",
        SourceKind::CatalogYaml,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("rev"),
        "catalog/commands.yaml#public_commands[0]",
        "application/yaml",
        "name: INFO\nsummary: Versioned handshake\n",
    )
    .expect("document");
    let chunks = vesc_knowledge_index::chunk_document(
        &document,
        ChunkingConfig {
            target_chars: 10,
            hard_max_chars: 100,
            minimum_chars: 1,
            ..ChunkingConfig::default()
        },
    )
    .expect("chunks");

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading_path, vec!["public_commands[0]"]);
    assert!(chunks[0].text.contains("summary: Versioned handshake"));
}

#[test]
fn oversized_structured_record_splits_with_provenance() {
    let document = NormalizedDocument::new(
        "Schema: root",
        SourceKind::CatalogYaml,
        RepositoryId::try_from("repo").expect("repo"),
        Revision::try_from("rev").expect("rev"),
        "catalog/schema.yaml#root",
        "application/yaml",
        "first: alpha beta gamma\nsecond: delta epsilon zeta\nthird: eta theta iota\n",
    )
    .expect("document");
    let chunks = vesc_knowledge_index::chunk_document(
        &document,
        ChunkingConfig {
            target_chars: 24,
            hard_max_chars: 30,
            minimum_chars: 1,
            ..ChunkingConfig::default()
        },
    )
    .expect("chunks");

    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|chunk| chunk.char_count <= 24));
    assert!(chunks.iter().all(|chunk| chunk.source_span.is_some()));
    for pair in chunks.windows(2) {
        assert_eq!(pair[0].next_chunk.as_ref(), Some(&pair[1].chunk_id));
        assert_eq!(pair[1].previous_chunk.as_ref(), Some(&pair[0].chunk_id));
    }
}
