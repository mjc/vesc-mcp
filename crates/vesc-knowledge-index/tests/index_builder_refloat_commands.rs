//! Index builder tests for catalog-backed refloat command doc parsing.

use std::path::PathBuf;

use vesc_knowledge_index::{Category, IndexBuilder};

fn repo_catalog_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

fn refloat_root() -> PathBuf {
    std::env::var("VESC_REFLOAT_ROOT").map_or_else(
        |_| PathBuf::from(std::env::var("HOME").expect("HOME")).join("projects/refloat"),
        PathBuf::from,
    )
}

#[test]
fn index_builder_parses_refloat_commands() {
    let root = refloat_root();
    let realtime_doc = root.join("doc/commands/REALTIME_DATA.md");
    if !realtime_doc.is_file() {
        eprintln!("skip: refloat command docs missing at {}", root.display());
        return;
    }

    let entries = IndexBuilder::parse_refloat_commands(&repo_catalog_root(), &root)
        .expect("parse refloat commands catalog");

    assert_eq!(
        entries.len(),
        12,
        "expected 7 public + 2 internal + 3 supporting doc entries"
    );

    for entry in &entries {
        assert_eq!(entry.category, Category::RefloatCommand);
        assert!(
            entry.id.starts_with("refloat_command."),
            "unexpected id prefix: {}",
            entry.id
        );
        assert_eq!(entry.source.repo, "refloat");
        assert!(
            entry.source.path.starts_with("doc/commands/"),
            "expected doc/commands path, got {}",
            entry.source.path
        );
        assert!(!entry.summary.is_empty(), "{} missing summary", entry.id);
    }

    let realtime = entries
        .iter()
        .find(|entry| entry.name == "REALTIME_DATA")
        .expect("REALTIME_DATA entry");
    assert_eq!(realtime.id, "refloat_command.REALTIME_DATA");
    assert_eq!(realtime.source.path, "doc/commands/REALTIME_DATA.md");
    assert!(
        realtime.summary.contains("selectable realtime data")
            || realtime.summary.contains("bitmask"),
        "expected doc first paragraph in summary: {}",
        realtime.summary
    );
    assert!(realtime.keywords.iter().any(|kw| kw == "REALTIME_DATA"));
    assert!(realtime.keywords.iter().any(|kw| kw == "stable"));
    assert!(realtime.keywords.iter().any(|kw| kw == "33"));
}
