//! One-shot helper to emit golden JSON (run with `cargo test -p vesc-knowledge-index emit_golden -- --ignored --nocapture`).

use std::path::PathBuf;

use vesc_knowledge_index::search_knowledge;

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
        let hits = search_knowledge(query, None, 1);
        let top = hits
            .first()
            .unwrap_or_else(|| panic!("no hits for {query}"));
        let category = match top.category {
            vesc_knowledge_index::Category::FirmwareApi => "firmware_api",
            vesc_knowledge_index::Category::Lispbm => "lispbm",
            vesc_knowledge_index::Category::PackageBuild => "package_build",
            vesc_knowledge_index::Category::RefloatCommand => "refloat_command",
            vesc_knowledge_index::Category::PocAbi => "poc_abi",
        };
        let payload = serde_json::json!({
            "query": query,
            "top": {
                "id": top.id,
                "name": top.name,
                "category": category,
                "score": top.score,
                "source_repo": top.source.repo,
                "source_path": top.source.path,
            }
        });
        let path = out_dir.join(format!("{file_stem}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&payload).expect("json"))
            .expect("write golden");
        eprintln!("wrote {}", path.display());
    }
}
