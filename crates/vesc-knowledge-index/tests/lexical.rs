use vesc_knowledge_index::corpus::{Chunk, NormalizedDocument, RepositoryId, Revision, SourceKind};
use vesc_knowledge_index::lexical::{LexicalFilters, LexicalIndex};

fn chunk(repository: &str, text: &str, category: vesc_knowledge_index::Category) -> Chunk {
    let mut document = NormalizedDocument::new(
        "Package API",
        SourceKind::Markdown,
        RepositoryId::try_from(repository).expect("repo"),
        Revision::try_from("rev").expect("revision"),
        "docs/api.md",
        "text/markdown",
        text,
    )
    .expect("document");
    document.category = Some(category);
    Chunk::from_document(&document, 0, text.into(), Vec::new(), None).expect("chunk")
}

#[test]
fn lexical_filter_is_conjunctive() {
    let index = LexicalIndex::build(&[
        chunk(
            "repo-a",
            "native extension registration",
            vesc_knowledge_index::Category::FirmwareApi,
        ),
        chunk(
            "repo-b",
            "native extension registration",
            vesc_knowledge_index::Category::PackageBuild,
        ),
    ])
    .expect("index");
    let hits = index
        .search(
            "native extension",
            &LexicalFilters {
                category: Some(vesc_knowledge_index::Category::FirmwareApi),
                ..LexicalFilters::default()
            },
            10,
        )
        .expect("search");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk.repository.as_str(), "repo-a");
}

#[test]
fn empty_or_punctuation_only_query_is_rejected() {
    let index = LexicalIndex::build(&[chunk(
        "repo",
        "native extension",
        vesc_knowledge_index::Category::FirmwareApi,
    )])
    .expect("index");
    assert!(index.search("---", &LexicalFilters::default(), 10).is_err());
}

#[test]
fn source_kind_and_tag_filters_are_conjunctive() {
    let mut matching = chunk(
        "repo",
        "native extension registration",
        vesc_knowledge_index::Category::FirmwareApi,
    );
    matching.tags.insert("firmware".into());
    let index = LexicalIndex::build(&[matching]).expect("index");
    let hits = index
        .search(
            "native extension",
            &LexicalFilters {
                source_kind: Some(SourceKind::Markdown),
                tags: vec!["firmware".into()],
                ..LexicalFilters::default()
            },
            10,
        )
        .expect("search");
    assert_eq!(hits.len(), 1);
}
