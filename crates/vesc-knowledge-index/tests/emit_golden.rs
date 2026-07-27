//! One-shot helper to emit golden JSON (run with `cargo test -p vesc-knowledge-index emit_golden -- --ignored --nocapture`).

use std::path::PathBuf;

use vesc_knowledge_index::{LexicalFilters, lexical_index};

#[test]
#[ignore = "manual: regenerate tests/golden/*.json"]
fn emit_golden_search_fixtures() {
    let queries = [
        ("search_lbm_add_extension", "lbm_add_extension"),
        ("search_nvm_write", "NVM"),
        ("search_refloat_realtime", "REALTIME"),
        ("search_build_pkg_from_desc", "buildPkgFromDesc"),
    ];
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden");
    std::fs::create_dir_all(&out_dir).expect("create golden dir");

    for (file_stem, query) in queries {
        let hits = lexical_index()
            .search(query, &LexicalFilters::default(), 1)
            .expect("lexical search");
        let top = hits
            .first()
            .unwrap_or_else(|| panic!("no hits for {query}"));
        let chunk = &top.chunk;
        let category = match chunk.category.expect("catalog category") {
            vesc_knowledge_index::Category::FirmwareApi => "firmware_api",
            vesc_knowledge_index::Category::Lispbm => "lispbm",
            vesc_knowledge_index::Category::PackageBuild => "package_build",
            vesc_knowledge_index::Category::RefloatCommand => "refloat_command",
            vesc_knowledge_index::Category::NativeLibAbi => "native_lib_abi",
        };
        let payload = serde_json::json!({
            "query": query,
            "top": {
                "id": chunk
                    .registered_id
                    .as_deref()
                    .map_or_else(|| chunk.chunk_id.to_string(), str::to_owned),
                "name": chunk.title,
                "category": category,
                "source_repo": chunk.repository,
                "source_path": chunk.path,
            }
        });
        let path = out_dir.join(format!("{file_stem}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&payload).expect("json"))
            .expect("write golden");
        eprintln!("wrote {}", path.display());
    }
}
