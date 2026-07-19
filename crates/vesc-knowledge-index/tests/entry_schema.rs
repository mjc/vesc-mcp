//! Entry schema serialization tests for the knowledge index.

use vesc_knowledge_index::{Category, IndexEntry, SourceRef};

#[test]
fn index_entry_serializes_to_json() {
    let entry = IndexEntry {
        id: "vesc_c_if.lbm_add_extension".into(),
        name: "lbm_add_extension".into(),
        category: Category::FirmwareApi,
        summary: "Register a native extension with lispBM".into(),
        source: SourceRef {
            repo: "vesc".into(),
            path: "lispBM/c_libs/vesc_c_if.h".into(),
            line: 42,
        },
        keywords: vec!["extension".into(), "native".into()],
    };

    let json = serde_json::to_string(&entry).expect("serialize entry");
    let value: serde_json::Value = serde_json::from_str(&json).expect("parse json");

    assert_eq!(value["id"], "vesc_c_if.lbm_add_extension");
    assert_eq!(value["name"], "lbm_add_extension");
    assert_eq!(value["category"], "firmware_api");
    assert_eq!(value["summary"], "Register a native extension with lispBM");
    assert_eq!(value["source"]["repo"], "vesc");
    assert_eq!(value["source"]["path"], "lispBM/c_libs/vesc_c_if.h");
    assert_eq!(value["source"]["line"], 42);
    assert_eq!(
        value["keywords"],
        serde_json::json!(["extension", "native"])
    );

    let roundtrip: IndexEntry = serde_json::from_str(&json).expect("deserialize entry");
    assert_eq!(roundtrip, entry);
}

#[test]
fn category_roundtrip() {
    let cases = [
        (Category::FirmwareApi, "firmware_api"),
        (Category::Lispbm, "lispbm"),
        (Category::PackageBuild, "package_build"),
        (Category::RefloatCommand, "refloat_command"),
        (Category::NativeLibAbi, "native_lib_abi"),
    ];

    for (category, expected) in cases {
        let json = serde_json::to_value(category).expect("serialize category");
        assert_eq!(json, expected);

        let back: Category = serde_json::from_value(json).expect("deserialize category");
        assert_eq!(back, category);
    }
}
