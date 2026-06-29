//! Golden-file search result tests against the embedded knowledge index.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vesc_knowledge_index::{Category, search_knowledge};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GoldenSearchTop {
    id: String,
    name: String,
    category: String,
    score: u32,
    source_repo: String,
    source_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GoldenSearchExpectation {
    query: String,
    top: GoldenSearchTop,
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden")
}

fn load_expectation(name: &str) -> GoldenSearchExpectation {
    let path = golden_dir().join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read golden file {}: {err}", path.display());
    });
    serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse golden file {}: {err}", path.display());
    })
}

const fn category_name(category: Category) -> &'static str {
    match category {
        Category::FirmwareApi => "firmware_api",
        Category::Lispbm => "lispbm",
        Category::PackageBuild => "package_build",
        Category::RefloatCommand => "refloat_command",
        Category::NativeLibAbi => "native_lib_abi",
    }
}

fn assert_golden_search(name: &str) {
    let expected = load_expectation(name);
    let hits = search_knowledge(&expected.query, None, 1);
    assert!(
        !hits.is_empty(),
        "query {:?} returned no hits",
        expected.query
    );
    let top = &hits[0];
    let actual = GoldenSearchTop {
        id: top.id.clone(),
        name: top.name.clone(),
        category: category_name(top.category).into(),
        score: top.score,
        source_repo: top.source.repo.clone(),
        source_path: top.source.path.clone(),
    };
    assert_eq!(actual, expected.top, "query {:?}", expected.query);
}

#[test]
fn golden_search_lbm_add_extension() {
    assert_golden_search("search_lbm_add_extension");
}

#[test]
fn golden_search_nvm_write() {
    assert_golden_search("search_nvm_write");
}

#[test]
fn golden_search_refloat_realtime() {
    assert_golden_search("search_refloat_realtime");
}

#[test]
fn golden_search_build_pkg_from_desc() {
    assert_golden_search("search_build_pkg_from_desc");
}
