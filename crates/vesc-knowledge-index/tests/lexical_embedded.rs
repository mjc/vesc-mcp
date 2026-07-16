use vesc_knowledge_index::{Category, search_lexical_knowledge};

#[test]
fn embedded_lexical_search_supports_category_filter() {
    let hits = search_lexical_knowledge("nvm", Some(Category::FirmwareApi), 10).expect("search");
    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|hit| hit.chunk.category == Some(Category::FirmwareApi))
    );
}
