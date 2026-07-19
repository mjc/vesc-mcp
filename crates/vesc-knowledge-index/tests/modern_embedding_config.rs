use serde::Deserialize;
use vesc_knowledge_index::{EmbeddingProfile, benchmark::BakeoffCandidateSpec};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: u16,
    corpus_digest: String,
    corpus_documents: usize,
    corpus_chunks: usize,
    candidates: Vec<BakeoffCandidateSpec>,
}

#[test]
fn modern_matrix_is_staged_pinned_and_production_licensed() {
    let config: Config = serde_json::from_str(include_str!(
        "../../../tests/benchmark/modern-embedding-models.json"
    ))
    .expect("modern embedding matrix");

    assert_eq!(config.schema, 1);
    assert!(config.corpus_digest.starts_with("sha256:"));
    assert_eq!(
        (config.corpus_documents, config.corpus_chunks),
        (2875, 16_586)
    );
    assert_eq!(config.candidates.len(), 3);
    assert!(config.candidates.iter().all(|candidate| {
        candidate.model_revision.len() == 40
            && candidate.onnx_sha256.len() == 64
            && candidate.onnx_bytes > 0
            && candidate.production_eligible
            && matches!(candidate.license.as_str(), "MIT" | "Apache-2.0")
            && EmbeddingProfile::for_model_id(&candidate.model_id).is_some()
    }));
    assert_eq!(
        config.candidates[1].onnx_sha256,
        "a6022dd8220ea6f6595562a1328ee216f4a94faa55362f2f4747c80f1e78772e"
    );
    assert_eq!(
        config.candidates[2].onnx_sha256,
        "f1fdd44e7e1ac51f12ab7957c7bd092e064d596c288513bf9d326842f669edee"
    );
}
