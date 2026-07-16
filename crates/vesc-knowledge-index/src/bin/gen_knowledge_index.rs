//! Regenerate `generated/knowledge_index.json` from catalog YAML.

#[cfg(feature = "semantic-fastembed")]
use std::collections::BTreeMap;
use std::env;
#[cfg(feature = "semantic-fastembed-online")]
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "semantic-fastembed-online")]
use sha2::{Digest, Sha256};
use vesc_knowledge_index::benchmark::{BenchmarkReport, benchmark_lexical};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::benchmark::{
    SemanticBenchmarkMatrixReport, SemanticBenchmarkReport, benchmark_semantic,
};
use vesc_knowledge_index::evaluation::{
    EvaluationMode, EvaluationQuery, EvaluationReport, QualityThresholds, evaluate_quality_gate,
    evaluate_suite_with_mode,
};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::{
    Chunk, EmbeddingProfile, EmbeddingProvider, FastEmbedProvider, FusionConfig, VectorArtifact,
    build_allowlisted_artifacts_with_provider, build_embedded_artifacts_with_provider,
    embedding_text, fuse_candidates,
};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::{ContentDigest, NormalizedDocument, embedded_entries};
use vesc_knowledge_index::{
    IndexBuilder, LexicalFilters, LexicalIndex, RepositoryId, Revision, active_manifest_path,
    build_allowlisted_artifacts, build_embedded_artifacts, inspect_manifest, search_knowledge,
    search_lexical_knowledge, vesc_mcp_source_specs,
};
#[cfg(feature = "git-corpus")]
use vesc_knowledge_index::{
    LicenseStatus, TrustTier, build_git_artifacts, build_git_artifacts_with_provider,
    corpus::git::{GitCorpusPolicy, GitCorpusSource},
};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "provision-model") {
        run_provision_model(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "evaluate") {
        run_evaluation(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "benchmark") {
        run_benchmark(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "build") {
        run_build(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "build-default") {
        run_build_default(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "inspect") {
        run_inspect(&args[1..]);
        return;
    }

    generate_index();
}

#[cfg(feature = "git-corpus")]
#[allow(clippy::option_if_let_else, clippy::too_many_lines)]
fn run_build_default(args: &[String]) {
    let generated = argument_value(args, "--generated-dir").map_or_else(
        || PathBuf::from("crates/vesc-knowledge-index/generated"),
        PathBuf::from,
    );
    let staging = argument_value(args, "--staging-dir").map_or_else(
        || PathBuf::from("target/default-knowledge-artifacts"),
        PathBuf::from,
    );
    let source = |name: &str| {
        let path = argument_value(args, &format!("--{name}-path"))
            .unwrap_or_else(|| panic!("--{name}-path is required"));
        let revision = argument_value(args, &format!("--{name}-revision"))
            .unwrap_or_else(|| panic!("--{name}-revision is required"));
        let mut policy = GitCorpusPolicy::default();
        policy.extensions.remove("md");
        policy.max_file_bytes = 512 * 1024;
        GitCorpusSource {
            repository_path: PathBuf::from(path),
            repository_id: RepositoryId::try_from(name).expect("valid repository identifier"),
            revision: Revision::try_from(revision).expect("valid immutable revision"),
            trust_tier: TrustTier::CuratedUpstream,
            license: LicenseStatus::ReferenceOnly,
            policy,
        }
    };
    let sources = [source("vesc"), source("vesc-tool"), source("refloat")];
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .unwrap_or_else(|error| panic!("remove staging {}: {error}", staging.display()));
    }
    let model_dir = argument_value(args, "--semantic-model-dir");
    let summary = if let Some(model_dir) = model_dir {
        let model_id = argument_value(args, "--semantic-model-id")
            .unwrap_or_else(|| panic!("--semantic-model-id is required with --semantic-model-dir"));
        let model_revision = argument_value(args, "--semantic-model-revision")
            .unwrap_or_else(|| {
                panic!("--semantic-model-revision is required with --semantic-model-dir")
            });
        #[cfg(feature = "semantic-fastembed")]
        {
            let batch_size = argument_value(args, "--semantic-batch-size").map(|value| {
                value
                    .parse::<usize>()
                    .expect("--semantic-batch-size must be an integer")
            });
            let mut provider = FastEmbedProvider::from_model_dir_with_profile(
                &PathBuf::from(model_dir),
                batch_size,
                embedding_profile(&model_id),
            )
            .unwrap_or_else(|error| panic!("load semantic model: {error}"));
            build_git_artifacts_with_provider(
                &staging,
                &sources,
                Some((&mut provider, &model_id, &model_revision)),
            )
        }
        #[cfg(not(feature = "semantic-fastembed"))]
        {
            let _ = (model_dir, model_id, model_revision);
            panic!(
                "semantic model builds require the semantic-fastembed feature; rerun with --features semantic-fastembed"
            );
        }
    } else {
        build_git_artifacts(&staging, &sources)
    }
    .unwrap_or_else(|error| panic!("build default corpus: {error}"));
    let generation = staging.join("generations").join(&summary.generation);
    let generated_generation = generated.join("generations").join(&summary.generation);
    if generated.exists() {
        fs::remove_dir_all(&generated)
            .unwrap_or_else(|error| panic!("remove {}: {error}", generated.display()));
    }
    fs::create_dir_all(&generated_generation)
        .unwrap_or_else(|error| panic!("create {}: {error}", generated_generation.display()));
    fs::copy(
        generation.join("lexical.json"),
        generated_generation.join("lexical.json"),
    )
    .unwrap_or_else(|error| panic!("copy default lexical artifact: {error}"));
    if summary.vector_bytes.is_some() {
        fs::copy(
            generation.join("vectors.bin"),
            generated_generation.join("vectors.bin"),
        )
        .unwrap_or_else(|error| panic!("copy default vector artifact: {error}"));
    }
    fs::copy(staging.join("active.json"), generated.join("active.json"))
        .unwrap_or_else(|error| panic!("copy default manifest: {error}"));
    println!("documents: {}", summary.document_count);
    println!("chunks: {}", summary.chunk_count);
    println!("build-duration-us: {}", summary.build_duration_us);
    for (phase, duration) in &summary.observations.phases_us {
        println!("phase-{phase:?}-us: {duration}");
    }
    println!("sources: {}", summary.manifest.sources.len());
    println!("diagnostics: {}", summary.manifest.diagnostics.len());
    println!(
        "embedding-input-bytes: {}",
        summary.observations.embedding_input_bytes
    );
    println!("visited-files: {}", summary.observations.visited_files);
    println!(
        "provenance-bytes: {}",
        summary.observations.provenance_bytes()
    );
    println!("corpus-bytes: {}", summary.observations.corpus_bytes);
    println!(
        "generation-manifest-bytes: {}",
        summary.observations.manifest_bytes
    );
    println!(
        "active-manifest-bytes: {}",
        summary.observations.active_manifest_bytes
    );
    if let Some(batch) = summary.observations.resolved_batch_size {
        println!("semantic-batch-size: {batch}");
    }
    println!("generated-dir: {}", generated.display());
}

#[cfg(not(feature = "git-corpus"))]
fn run_build_default(_args: &[String]) {
    panic!("default corpus generation requires --features git-corpus");
}

#[cfg(feature = "semantic-fastembed-online")]
fn run_provision_model(args: &[String]) {
    let out = argument_value(args, "--out").map_or_else(
        || PathBuf::from("target/models/bge-small-en-v1.5"),
        PathBuf::from,
    );
    let cache = argument_value(args, "--cache-dir")
        .map_or_else(|| PathBuf::from("target/fastembed-cache"), PathBuf::from);
    let model_id = "Xenova/bge-small-en-v1.5";
    let mut model = fastembed::TextEmbedding::try_new(
        fastembed::TextInitOptions::new(fastembed::EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache.clone())
            .with_show_download_progress(true),
    )
    .unwrap_or_else(|error| panic!("provision {model_id}: {error}"));
    model
        .embed([String::from("model provisioning check")], Some(1))
        .unwrap_or_else(|error| panic!("validate provisioned {model_id}: {error}"));

    let snapshot = model_snapshot(&cache, model_id)
        .unwrap_or_else(|error| panic!("locate provisioned {model_id}: {error}"));
    let files = [
        ("model.onnx", snapshot.join("onnx/model.onnx")),
        ("tokenizer.json", snapshot.join("tokenizer.json")),
        ("config.json", snapshot.join("config.json")),
        (
            "special_tokens_map.json",
            snapshot.join("special_tokens_map.json"),
        ),
        (
            "tokenizer_config.json",
            snapshot.join("tokenizer_config.json"),
        ),
    ];
    fs::create_dir_all(&out)
        .unwrap_or_else(|error| panic!("create model directory {}: {error}", out.display()));
    let mut manifest_files = serde_json::Map::new();
    for (name, source) in files {
        let bytes = fs::read(&source)
            .unwrap_or_else(|error| panic!("read model file {}: {error}", source.display()));
        fs::write(out.join(name), &bytes).unwrap_or_else(|error| {
            panic!("write model file {}: {error}", out.join(name).display())
        });
        manifest_files.insert(
            name.into(),
            serde_json::json!({
                "bytes": bytes.len(),
                "sha256": sha256_hex(&bytes),
            }),
        );
    }
    let revision = snapshot
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let manifest = serde_json::json!({
        "schema": 1,
        "model_id": model_id,
        "model_revision": revision,
        "license": "Apache-2.0",
        "files": manifest_files,
    });
    fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize model manifest"),
    )
    .unwrap_or_else(|error| panic!("write model manifest {}: {error}", out.display()));
    println!("model-id: {model_id}");
    println!("model-revision: {revision}");
    println!("model-dir: {}", out.display());
    println!("manifest: {}", out.join("manifest.json").display());
}

#[cfg(not(feature = "semantic-fastembed-online"))]
fn run_provision_model(_args: &[String]) {
    panic!(
        "model provisioning requires the semantic-fastembed-online feature; rerun with --features semantic-fastembed-online"
    );
}

#[cfg(feature = "semantic-fastembed-online")]
fn model_snapshot(cache: &Path, model_id: &str) -> Result<PathBuf, String> {
    let cache_name = format!("models--{}", model_id.replace('/', "--"));
    let model_root = cache.join(cache_name);
    let revision = fs::read_to_string(model_root.join("refs/main"))
        .map_err(|error| format!("read cache revision: {error}"))?;
    let revision = revision.trim();
    if revision.is_empty() {
        return Err("cache revision is empty".into());
    }
    let snapshot = model_root.join("snapshots").join(revision);
    if !snapshot.join("onnx/model.onnx").is_file() {
        return Err(format!("missing model file under {}", snapshot.display()));
    }
    Ok(snapshot)
}

#[cfg(feature = "semantic-fastembed-online")]
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(all(test, feature = "semantic-fastembed-online"))]
mod provisioning_tests {
    use super::*;

    #[test]
    fn model_snapshot_follows_the_pinned_main_ref() {
        let temp = tempfile::tempdir().expect("temporary cache");
        let root = temp.path().join("models--Xenova--bge-small-en-v1.5");
        let revision = "0123456789abcdef";
        fs::create_dir_all(root.join("refs")).expect("refs");
        fs::create_dir_all(root.join("snapshots").join(revision).join("onnx")).expect("snapshot");
        fs::write(root.join("refs/main"), revision).expect("main ref");
        fs::write(
            root.join("snapshots")
                .join(revision)
                .join("onnx/model.onnx"),
            b"model",
        )
        .expect("model");

        let snapshot =
            model_snapshot(temp.path(), "Xenova/bge-small-en-v1.5").expect("pinned snapshot");
        assert_eq!(snapshot, root.join("snapshots").join(revision));
        assert_eq!(
            sha256_hex(b"model"),
            "9372c470eeadd5ecd9c3c74c2b3cb633f8e2f2fad799250a0f70d652b6b825e4"
        );
    }
}

#[allow(clippy::option_if_let_else)]
#[allow(clippy::too_many_lines)]
fn run_build(args: &[String]) {
    let out = argument_value(args, "--out").map_or_else(
        || PathBuf::from("target/knowledge-artifacts"),
        PathBuf::from,
    );
    let source_root = argument_value(args, "--source-root").map(PathBuf::from);
    let repository = RepositoryId::try_from(
        argument_value(args, "--repository").unwrap_or_else(|| "vesc-mcp".into()),
    )
    .expect("valid repository identifier");
    let revision = Revision::try_from(
        argument_value(args, "--revision").unwrap_or_else(|| "working-tree".into()),
    )
    .expect("valid source revision");
    let specs = vesc_mcp_source_specs();
    let model_dir = argument_value(args, "--semantic-model-dir");
    let semantic_batch_size = argument_value(args, "--semantic-batch-size").map(|value| {
        value
            .parse::<usize>()
            .expect("--semantic-batch-size must be an integer")
    });
    let summary = if let Some(model_dir) = model_dir {
        let model_id = argument_value(args, "--semantic-model-id")
            .unwrap_or_else(|| panic!("--semantic-model-id is required with --semantic-model-dir"));
        let model_revision = argument_value(args, "--semantic-model-revision")
            .unwrap_or_else(|| {
                panic!("--semantic-model-revision is required with --semantic-model-dir")
            });
        #[cfg(feature = "semantic-fastembed")]
        {
            let mut provider = FastEmbedProvider::from_model_dir_with_profile(
                &PathBuf::from(model_dir),
                semantic_batch_size,
                embedding_profile(&model_id),
            )
            .unwrap_or_else(|error| panic!("load semantic model: {error}"));
            match source_root.as_deref() {
                Some(source_root) => build_allowlisted_artifacts_with_provider(
                    &out,
                    source_root,
                    &repository,
                    &revision,
                    &specs,
                    Some((&mut provider, &model_id, &model_revision)),
                ),
                None => build_embedded_artifacts_with_provider(
                    &out,
                    &mut provider,
                    &model_id,
                    &model_revision,
                ),
            }
        }
        #[cfg(not(feature = "semantic-fastembed"))]
        {
            let _ = (model_dir, model_id, model_revision, semantic_batch_size);
            panic!(
                "semantic model builds require the semantic-fastembed feature; rerun with --features semantic-fastembed"
            );
        }
    } else {
        match source_root.as_deref() {
            Some(source_root) => {
                build_allowlisted_artifacts(&out, source_root, &repository, &revision, &specs)
            }
            None => build_embedded_artifacts(&out),
        }
    }
    .unwrap_or_else(|error| {
        panic!("build knowledge artifacts under {}: {error}", out.display());
    });
    println!("generation: {}", summary.generation);
    println!("documents: {}", summary.document_count);
    println!("chunks: {}", summary.chunk_count);
    println!("lexical-bytes: {}", summary.lexical_bytes);
    println!("build-duration-us: {}", summary.build_duration_us);
    for (phase, duration) in &summary.observations.phases_us {
        println!("phase-{phase:?}-us: {duration}");
    }
    println!("artifact-bytes: {}", summary.observations.artifact_bytes);
    println!(
        "embedding-input-bytes: {}",
        summary.observations.embedding_input_bytes
    );
    println!("visited-files: {}", summary.observations.visited_files);
    println!(
        "provenance-bytes: {}",
        summary.observations.provenance_bytes()
    );
    println!("corpus-bytes: {}", summary.observations.corpus_bytes);
    println!(
        "generation-manifest-bytes: {}",
        summary.observations.manifest_bytes
    );
    println!(
        "active-manifest-bytes: {}",
        summary.observations.active_manifest_bytes
    );
    if let Some(batch) = summary.observations.resolved_batch_size {
        println!("semantic-batch-size: {batch}");
    }
    println!("sources: {}", summary.manifest.sources.len());
    if let Some(vector_bytes) = summary.vector_bytes {
        println!("vector-bytes: {vector_bytes}");
    }
    println!("diagnostics: {}", summary.manifest.diagnostics.len());
    println!("active-manifest: {}", active_manifest_path(&out).display());
}

fn run_inspect(args: &[String]) {
    let path = argument_value(args, "--path").map_or_else(
        || PathBuf::from("target/knowledge-artifacts/active.json"),
        PathBuf::from,
    );
    let path = if path.is_dir() {
        active_manifest_path(&path)
    } else {
        path
    };
    let manifest = inspect_manifest(&path).unwrap_or_else(|error| {
        panic!("inspect knowledge manifest {}: {error}", path.display());
    });
    println!(
        "schema: {}.{}",
        manifest.schema.major, manifest.schema.minor
    );
    println!("corpus: {}", manifest.corpus.corpus_version);
    println!("documents: {}", manifest.corpus.documents.len());
    println!("chunks: {}", manifest.corpus.chunks.len());
    println!("corpus-digest: {}", manifest.corpus.content_digest);
    println!("lexical-checksum: {:?}", manifest.lexical_checksum);
    println!("vector-checksum: {:?}", manifest.vector_checksum);
    println!("sources: {}", manifest.sources.len());
    println!("diagnostics: {}", manifest.diagnostics.len());
    println!("chunking: {:?}", manifest.chunking);
    println!("component-versions: {:?}", manifest.component_versions);
}

fn generate_index() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let catalog_root = manifest_dir.join("../../catalog");
    let refloat_root = resolve_refloat_root(&manifest_dir);
    let out_path = manifest_dir.join("generated/knowledge_index.json");

    let entries = IndexBuilder::build_embedded_index(&catalog_root, &refloat_root)
        .expect("build knowledge index from catalog");
    let json = serde_json::to_string_pretty(&entries).expect("serialize knowledge index");
    fs::write(&out_path, json).expect("write generated/knowledge_index.json");
    eprintln!("wrote {}", out_path.display());
}

fn run_evaluation(args: &[String]) {
    let suite_path = argument_value(args, "--suite")
        .map(PathBuf::from)
        .map_or_else(
            || PathBuf::from("tests/evaluation/v1/queries.json"),
            PathBuf::from,
        );
    let format = argument_value(args, "--format").unwrap_or_else(|| "text".into());
    let gate_requested = args.iter().any(|arg| arg == "--gate");
    let artifact = argument_value(args, "--artifact").map(PathBuf::from);
    let semantic_model_dir = argument_value(args, "--semantic-model-dir").map(PathBuf::from);
    let semantic_model_id = argument_value(args, "--semantic-model-id");
    let semantic_model_revision = argument_value(args, "--semantic-model-revision");
    let semantic_min_similarity = argument_value(args, "--semantic-min-similarity").map(|value| {
        value
            .parse::<f32>()
            .expect("--semantic-min-similarity must be a number")
    });
    let mode_name = argument_value(args, "--mode").unwrap_or_else(|| "legacy".into());
    let modes = match mode_name.as_str() {
        "legacy" => vec![EvaluationMode::Legacy],
        "lexical" => vec![EvaluationMode::Lexical],
        "semantic" => vec![EvaluationMode::Semantic],
        "hybrid" => vec![EvaluationMode::Hybrid],
        "all" => vec![
            EvaluationMode::Legacy,
            EvaluationMode::Lexical,
            EvaluationMode::Semantic,
            EvaluationMode::Hybrid,
        ],
        other => {
            panic!(
                "unsupported evaluation mode {other:?}; use legacy, lexical, semantic, hybrid, or all"
            )
        }
    };
    let raw = fs::read_to_string(&suite_path).unwrap_or_else(|err| {
        panic!("read evaluation suite {}: {err}", suite_path.display());
    });
    let queries: Vec<EvaluationQuery> = serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse evaluation suite {}: {err}", suite_path.display());
    });
    let reports: Vec<_> = modes
        .iter()
        .copied()
        .map(|mode| {
            evaluate_mode(
                &queries,
                mode,
                artifact.as_deref(),
                semantic_model_dir.as_deref(),
                semantic_model_id.as_deref(),
                semantic_model_revision.as_deref(),
                semantic_min_similarity,
            )
        })
        .collect();
    assert!(
        !(gate_requested && reports.len() > 1),
        "--gate requires one evaluation mode; use --mode lexical"
    );
    let gate =
        gate_requested.then(|| evaluate_quality_gate(&reports[0], QualityThresholds::default()));
    match format.as_str() {
        "json" if reports.len() == 1 && gate_requested => println!(
            "{}",
            serde_json::json!({ "report": &reports[0], "gate": &gate })
        ),
        "json" if reports.len() == 1 => println!(
            "{}",
            serde_json::to_string_pretty(&reports[0]).expect("serialize evaluation report")
        ),
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "reports": reports }))
                .expect("serialize evaluation reports")
        ),
        "text" => {
            for (index, report) in reports.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                print_text_report(report);
            }
            if let Some(gate) = &gate {
                print_quality_gate(gate);
            }
        }
        other => panic!("unsupported evaluation format {other:?}; use json or text"),
    }
    if gate.as_ref().is_some_and(|gate| !gate.passed) {
        std::process::exit(2);
    }
}

fn evaluate_mode(
    queries: &[EvaluationQuery],
    mode: EvaluationMode,
    artifact: Option<&Path>,
    semantic_model_dir: Option<&Path>,
    semantic_model_id: Option<&str>,
    semantic_model_revision: Option<&str>,
    semantic_min_similarity: Option<f32>,
) -> EvaluationReport {
    #[cfg(not(feature = "semantic-fastembed"))]
    let _ = (
        semantic_model_dir,
        semantic_model_id,
        semantic_model_revision,
        semantic_min_similarity,
    );
    match mode {
        EvaluationMode::Legacy => evaluate_suite_with_mode(queries, mode, Vec::new(), |query| {
            search_knowledge(query, None, 50)
                .into_iter()
                .map(|hit| hit.id)
                .collect()
        }),
        EvaluationMode::Lexical => evaluate_suite_with_mode(queries, mode, Vec::new(), |query| {
            lexical_result_ids(query, artifact)
        }),
        EvaluationMode::Semantic | EvaluationMode::Hybrid => {
            #[cfg(feature = "semantic-fastembed")]
            if let (Some(artifact), Some(model_dir), Some(model_id), Some(model_revision)) = (
                artifact,
                semantic_model_dir,
                semantic_model_id,
                semantic_model_revision,
            ) {
                return evaluate_semantic(
                    queries,
                    mode,
                    artifact,
                    model_dir,
                    model_id,
                    model_revision,
                    semantic_min_similarity,
                );
            }
            evaluate_suite_with_mode(
                queries,
                mode,
                vec!["semantic capability is unavailable; lexical results used".into()],
                |query| lexical_result_ids(query, artifact),
            )
        }
    }
}

#[cfg(feature = "semantic-fastembed")]
fn evaluate_semantic(
    queries: &[EvaluationQuery],
    mode: EvaluationMode,
    artifact: &Path,
    model_dir: &Path,
    model_id: &str,
    model_revision: &str,
    semantic_min_similarity: Option<f32>,
) -> EvaluationReport {
    let manifest = inspect_manifest(&active_manifest_path(artifact))
        .unwrap_or_else(|error| panic!("inspect semantic artifact: {error}"));
    let generation = artifact
        .join("generations")
        .join(manifest.corpus.content_digest.to_string());
    let lexical_path = generation.join("lexical.json");
    let vector_path = generation.join("vectors.bin");
    let lexical = LexicalIndex::open_artifact(&lexical_path)
        .unwrap_or_else(|error| panic!("open semantic lexical artifact: {error}"));
    let vector = VectorArtifact::open_artifact(&vector_path)
        .unwrap_or_else(|error| panic!("open semantic vector artifact: {error}"));
    let raw = fs::read_to_string(&lexical_path)
        .unwrap_or_else(|error| panic!("read semantic lexical artifact: {error}"));
    let artifact: LexicalSourceArtifact = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("parse semantic lexical artifact: {error}"));
    let chunks: BTreeMap<_, _> = artifact
        .chunks
        .into_iter()
        .map(|chunk| (chunk.chunk_id.clone(), chunk))
        .collect();
    let chunk_ids = chunks.keys().cloned().collect();
    vector
        .validate_for_corpus(
            &manifest.corpus.content_digest,
            &chunk_ids,
            model_id,
            model_revision,
        )
        .unwrap_or_else(|error| panic!("validate semantic evaluation artifact: {error}"));
    let mut provider = FastEmbedProvider::from_model_dir_with_profile(
        model_dir,
        Some(8),
        embedding_profile(model_id),
    )
    .unwrap_or_else(|error| panic!("load semantic evaluation model: {error}"));

    evaluate_suite_with_mode(queries, mode, Vec::new(), |query| {
        let lexical_hits = if mode == EvaluationMode::Hybrid {
            lexical
                .search(query, &LexicalFilters::default(), 50)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let query_vector = provider
            .embed_query(&vesc_knowledge_index::semantic_query_text(query))
            .unwrap_or_else(|error| panic!("embed evaluation query: {error}"));
        let semantic_hits = vector
            .search(&query_vector, 50)
            .unwrap_or_else(|error| panic!("search semantic evaluation vectors: {error}"))
            .into_iter()
            .filter(|hit| semantic_min_similarity.is_none_or(|minimum| hit.similarity >= minimum))
            .collect::<Vec<_>>();
        if mode == EvaluationMode::Semantic {
            semantic_hits
                .into_iter()
                .filter_map(|hit| chunks.get(&hit.chunk_id))
                .flat_map(chunk_result_ids)
                .collect()
        } else {
            fuse_candidates(
                &lexical_hits,
                &semantic_hits,
                &chunks,
                FusionConfig {
                    limit: 50,
                    lexical_floor: true,
                    ..FusionConfig::default()
                },
            )
            .into_iter()
            .flat_map(|hit| chunk_result_ids(&hit.chunk))
            .collect()
        }
    })
}

#[cfg(feature = "semantic-fastembed")]
fn chunk_result_ids(chunk: &Chunk) -> Vec<String> {
    if chunk.legacy_ids.is_empty() {
        vec![chunk.chunk_id.to_string()]
    } else {
        chunk.legacy_ids.clone()
    }
}

#[cfg(feature = "semantic-fastembed")]
#[derive(Debug, serde::Deserialize)]
struct LexicalSourceArtifact {
    #[serde(rename = "schema")]
    _schema: u16,
    chunks: Vec<Chunk>,
}

fn run_benchmark(args: &[String]) {
    let suite_path = argument_value(args, "--suite")
        .map(PathBuf::from)
        .map_or_else(
            || PathBuf::from("tests/evaluation/v1/queries.json"),
            PathBuf::from,
        );
    let format = argument_value(args, "--format").unwrap_or_else(|| "text".into());
    let warmup = argument_value(args, "--warmup")
        .as_deref()
        .unwrap_or("3")
        .parse::<usize>()
        .expect("--warmup must be an integer");
    let repetitions = argument_value(args, "--repetitions")
        .as_deref()
        .unwrap_or("10")
        .parse::<usize>()
        .expect("--repetitions must be an integer");
    let artifact = argument_value(args, "--artifact").map(PathBuf::from);
    if argument_value(args, "--mode").as_deref() == Some("semantic") {
        run_semantic_benchmark(
            args,
            artifact.as_deref(),
            &suite_path,
            &format,
            warmup,
            repetitions,
        );
        return;
    }
    let raw = fs::read_to_string(&suite_path).unwrap_or_else(|error| {
        panic!("read benchmark suite {}: {error}", suite_path.display());
    });
    let queries: Vec<EvaluationQuery> = serde_json::from_str(&raw).unwrap_or_else(|error| {
        panic!("parse benchmark suite {}: {error}", suite_path.display());
    });
    let texts: Vec<_> = queries.into_iter().map(|query| query.text).collect();
    let report = benchmark_lexical(artifact.as_deref(), &texts, warmup, repetitions)
        .unwrap_or_else(|error| panic!("run lexical benchmark: {error}"));
    match format.as_str() {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize benchmark report")
        ),
        "text" => print_benchmark_report(&report),
        other => panic!("unsupported benchmark format {other:?}; use json, text, or markdown"),
    }
}

#[cfg(feature = "semantic-fastembed")]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_semantic_benchmark(
    args: &[String],
    artifact_root: Option<&Path>,
    suite_path: &Path,
    format: &str,
    warmup: usize,
    repetitions: usize,
) {
    let model_dir = argument_value(args, "--semantic-model-dir").map_or_else(
        || panic!("--semantic-model-dir is required for semantic benchmarks"),
        PathBuf::from,
    );
    let model_id = argument_value(args, "--semantic-model-id")
        .unwrap_or_else(|| panic!("--semantic-model-id is required for semantic benchmarks"));
    let model_revision = argument_value(args, "--semantic-model-revision")
        .unwrap_or_else(|| panic!("--semantic-model-revision is required for semantic benchmarks"));
    let batch_size = argument_value(args, "--semantic-batch-size").map(|value| {
        value
            .parse::<usize>()
            .expect("--semantic-batch-size must be an integer")
    });
    let batch_sizes_argument = argument_value(args, "--semantic-batch-sizes");
    let batch_sizes = batch_sizes_argument.as_deref().map_or_else(
        || vec![batch_size.unwrap_or(8)],
        |value| {
            value
                .split(',')
                .map(|value| {
                    value
                        .parse::<usize>()
                        .expect("--semantic-batch-sizes must be comma-separated integers")
                })
                .collect::<Vec<_>>()
        },
    );
    assert!(
        !batch_sizes.is_empty(),
        "--semantic-batch-sizes must contain at least one value"
    );
    assert!(
        batch_size.is_none() || batch_sizes_argument.is_none(),
        "use --semantic-batch-size or --semantic-batch-sizes, not both"
    );
    let intra_threads = argument_value(args, "--semantic-intra-threads").map(|value| {
        value
            .parse::<usize>()
            .expect("--semantic-intra-threads must be a positive integer")
    });
    assert!(
        intra_threads.is_none_or(|threads| threads > 0),
        "--semantic-intra-threads must be a positive integer"
    );
    let length_bucketed = argument_value(args, "--semantic-length-bucketed")
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    let sample_size = argument_value(args, "--semantic-sample-chunks").map(|value| {
        let size = value
            .parse::<usize>()
            .expect("--semantic-sample-chunks must be a positive integer");
        assert!(
            size > 0,
            "--semantic-sample-chunks must be a positive integer"
        );
        size
    });
    let limits = argument_value(args, "--limits")
        .unwrap_or_else(|| "5,10,20,50".into())
        .split(',')
        .map(|value| {
            value
                .parse::<usize>()
                .expect("--limits must be comma-separated integers")
        })
        .collect::<Vec<_>>();
    let raw = fs::read_to_string(suite_path).unwrap_or_else(|error| {
        panic!("read benchmark suite {}: {error}", suite_path.display());
    });
    let queries: Vec<EvaluationQuery> = serde_json::from_str(&raw).unwrap_or_else(|error| {
        panic!("parse benchmark suite {}: {error}", suite_path.display());
    });
    let texts = queries
        .into_iter()
        .map(|query| query.text)
        .collect::<Vec<_>>();
    let (chunks, corpus_digest) = semantic_benchmark_chunks(artifact_root);
    let chunks = if let Some(size) = sample_size {
        representative_chunk_sample(&chunks, size)
    } else {
        chunks
    };
    let mut embedding_texts = chunks.iter().map(embedding_text).collect::<Vec<_>>();
    let initialization_started = std::time::Instant::now();
    let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads(
        &model_dir,
        Some(batch_sizes[0]),
        embedding_profile(&model_id),
        intra_threads,
    )
    .unwrap_or_else(|error| panic!("load semantic model: {error}"));
    let chunks = if length_bucketed {
        let lengths = provider
            .token_lengths(&embedding_texts)
            .unwrap_or_else(|error| panic!("measure token lengths: {error}"));
        let mut indexed = chunks.into_iter().zip(lengths).collect::<Vec<_>>();
        indexed.sort_unstable_by(|(left_chunk, left_length), (right_chunk, right_length)| {
            left_length
                .cmp(right_length)
                .then_with(|| left_chunk.path.cmp(&right_chunk.path))
                .then_with(|| left_chunk.ordinal.cmp(&right_chunk.ordinal))
                .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
        });
        let chunks = indexed
            .into_iter()
            .map(|(chunk, _)| chunk)
            .collect::<Vec<_>>();
        embedding_texts = chunks.iter().map(embedding_text).collect();
        chunks
    } else {
        chunks
    };
    let initialization = vesc_knowledge_index::benchmark::TimingDistribution::single(
        u64::try_from(initialization_started.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
    let mut reports = Vec::with_capacity(batch_sizes.len());
    for batch_size in batch_sizes {
        provider
            .set_batch_size(batch_size)
            .unwrap_or_else(|error| panic!("set semantic batch size: {error}"));
        let mut report = benchmark_semantic(
            &mut provider,
            &chunks,
            &texts,
            &model_id,
            &model_revision,
            &corpus_digest,
            &limits,
            warmup,
            repetitions,
        )
        .unwrap_or_else(|error| panic!("run semantic benchmark: {error}"));
        report.cold_initialization = Some(initialization.clone());
        report.intra_threads = intra_threads;
        report.length_bucketed = length_bucketed;
        report.token_statistics = Some(
            provider
                .token_statistics(&embedding_texts)
                .unwrap_or_else(|error| panic!("measure semantic token statistics: {error}")),
        );
        reports.push(report);
    }
    if reports.len() == 1 {
        let report = reports.pop().expect("one semantic benchmark report");
        match format {
            "json" => println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize semantic benchmark report")
            ),
            "text" => print_semantic_benchmark_report(&report),
            "markdown" => print!("{}", report.to_markdown()),
            other => panic!("unsupported benchmark format {other:?}; use json, text, or markdown"),
        }
    } else {
        let report = SemanticBenchmarkMatrixReport {
            schema: 1,
            runs: reports,
        };
        match format {
            "json" => println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize semantic benchmark matrix")
            ),
            "text" => {
                for run in &report.runs {
                    println!("batch-size: {}", run.outer_batch_size);
                    print_semantic_benchmark_report(run);
                }
            }
            "markdown" => print!("{}", report.to_markdown()),
            other => panic!("unsupported benchmark format {other:?}; use json, text, or markdown"),
        }
    }
}

#[cfg(not(feature = "semantic-fastembed"))]
fn run_semantic_benchmark(
    _args: &[String],
    _artifact_root: Option<&Path>,
    _suite_path: &Path,
    _format: &str,
    _warmup: usize,
    _repetitions: usize,
) {
    panic!(
        "semantic benchmarks require the semantic-fastembed feature; rerun with --features semantic-fastembed"
    );
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_benchmark_chunks(artifact_root: Option<&Path>) -> (Vec<Chunk>, ContentDigest) {
    let (path, corpus_digest) = match artifact_root {
        Some(root) if root.is_file() => (root.to_owned(), ContentDigest::of(b"benchmark-artifact")),
        Some(root) => {
            let manifest = inspect_manifest(&active_manifest_path(root))
                .unwrap_or_else(|error| panic!("inspect benchmark artifact: {error}"));
            (
                root.join("generations")
                    .join(manifest.corpus.content_digest.to_string())
                    .join("lexical.json"),
                manifest.corpus.content_digest,
            )
        }
        None => {
            let chunks = embedded_entries()
                .iter()
                .filter_map(|entry| {
                    NormalizedDocument::from_legacy(entry)
                        .ok()
                        .and_then(|document| document.legacy_chunk().ok())
                })
                .collect();
            return (chunks, ContentDigest::of(b"embedded-benchmark"));
        }
    };
    let index = LexicalIndex::open_artifact(&path)
        .unwrap_or_else(|error| panic!("open benchmark lexical artifact: {error}"));
    let mut chunks = index.chunks().values().cloned().collect::<Vec<_>>();
    chunks.sort_unstable_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    (chunks, corpus_digest)
}

#[cfg(feature = "semantic-fastembed")]
fn representative_chunk_sample(chunks: &[Chunk], limit: usize) -> Vec<Chunk> {
    if limit >= chunks.len() {
        return chunks.to_vec();
    }

    let mut buckets = BTreeMap::<String, Vec<usize>>::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let language: String = chunk
            .path
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .map_or_else(
                || "other".into(),
                |extension| match extension.as_str() {
                    "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => "c".into(),
                    "rs" => "rust".into(),
                    "md" | "markdown" | "mdown" => "markdown".into(),
                    "qml" => "qml".into(),
                    _ => "other".into(),
                },
            );
        let length = match chunk.byte_count {
            0..=512 => "short",
            4096.. => "long",
            _ => "medium",
        };
        buckets
            .entry(format!("{language}:{length}"))
            .or_default()
            .push(index);
    }

    let keys = buckets.keys().cloned().collect::<Vec<_>>();
    let mut cursors = BTreeMap::<String, usize>::new();
    let mut selected = Vec::with_capacity(limit);
    while selected.len() < limit {
        let mut added = false;
        for key in &keys {
            let cursor = cursors.entry(key.clone()).or_default();
            let Some(index) = buckets[key].get(*cursor).copied() else {
                continue;
            };
            *cursor += 1;
            selected.push(chunks[index].clone());
            added = true;
            if selected.len() == limit {
                break;
            }
        }
        if !added {
            break;
        }
    }
    selected
}

fn lexical_result_ids(query: &str, artifact: Option<&Path>) -> Vec<String> {
    let hits = artifact.map_or_else(
        || search_lexical_knowledge(query, None, 50).unwrap_or_default(),
        |root| {
            let path = if root.is_file() {
                root.to_owned()
            } else {
                let Ok(manifest) = inspect_manifest(&active_manifest_path(root)) else {
                    return Vec::new();
                };
                root.join("generations")
                    .join(manifest.corpus.content_digest.to_string())
                    .join("lexical.json")
            };
            LexicalIndex::open_artifact(&path)
                .ok()
                .and_then(|index| index.search(query, &LexicalFilters::default(), 50).ok())
                .unwrap_or_default()
        },
    );
    hits.into_iter()
        .flat_map(|hit| {
            if hit.chunk.legacy_ids.is_empty() {
                vec![hit.chunk.chunk_id.to_string()]
            } else {
                hit.chunk.legacy_ids
            }
        })
        .collect()
}

fn argument_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

#[cfg(feature = "semantic-fastembed")]
fn embedding_profile(model_id: &str) -> EmbeddingProfile {
    EmbeddingProfile::for_model_id(model_id)
        .unwrap_or_else(|| panic!("no embedding profile is registered for {model_id}"))
}

fn print_text_report(report: &EvaluationReport) {
    println!("mode: {:?}", report.mode);
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    println!("queries: {}", report.query_count);
    println!("recall@5: {:.4}", report.recall_at_5);
    println!("recall@10: {:.4}", report.recall_at_10);
    println!("mrr@10: {:.4}", report.mrr_at_10);
    println!("ndcg@10: {:.4}", report.ndcg_at_10);
    println!("zero-result-rate: {:.4}", report.zero_result_rate);
    println!("duplicate-rate@5: {:.4}", report.duplicate_rate_at_5);
    println!("diversity@5: {:.4}", report.diversity_at_5);
    println!("identifier-top1: {:.4}", report.exact_identifier_top_one);
    for (intent, metrics) in &report.by_intent {
        println!(
            "intent-{intent:?}: queries={} recall@5={:.4} mrr@10={:.4} ndcg@10={:.4} zero-result={:.4}",
            metrics.query_count,
            metrics.recall_at_5,
            metrics.mrr_at_10,
            metrics.ndcg_at_10,
            metrics.zero_result_rate,
        );
    }
    for (category, metrics) in &report.by_category {
        println!(
            "category-{category}: queries={} recall@5={:.4} mrr@10={:.4} ndcg@10={:.4} zero-result={:.4} duplicate-rate@5={:.4} diversity@5={:.4}",
            metrics.query_count,
            metrics.recall_at_5,
            metrics.mrr_at_10,
            metrics.ndcg_at_10,
            metrics.zero_result_rate,
            metrics.duplicate_rate_at_5,
            metrics.diversity_at_5,
        );
    }
    for (source, metrics) in &report.by_source {
        println!(
            "source-{source}: queries={} recall@5={:.4} mrr@10={:.4} ndcg@10={:.4} zero-result={:.4} duplicate-rate@5={:.4} diversity@5={:.4}",
            metrics.query_count,
            metrics.recall_at_5,
            metrics.mrr_at_10,
            metrics.ndcg_at_10,
            metrics.zero_result_rate,
            metrics.duplicate_rate_at_5,
            metrics.diversity_at_5,
        );
    }
    for (category, metrics) in &report.by_failure_category {
        println!(
            "failure-{category}: queries={} recall@5={:.4} recall@10={:.4} mrr@10={:.4}",
            metrics.query_count, metrics.recall_at_5, metrics.recall_at_10, metrics.mrr_at_10
        );
    }
}

fn print_quality_gate(gate: &vesc_knowledge_index::evaluation::QualityGateReport) {
    println!(
        "quality-gate: {}",
        if gate.passed { "PASS" } else { "FAIL" }
    );
    for failure in &gate.failures {
        println!(
            "quality-failure: {} actual={:.4} required={:.4}",
            failure.metric, failure.actual, failure.required
        );
    }
    for query in &gate.regression_queries {
        println!(
            "regression-query: {} intent={:?} returned={:?}",
            query.id, query.intent, query.returned
        );
    }
}

fn print_benchmark_report(report: &BenchmarkReport) {
    println!("mode: {:?}", report.mode);
    println!(
        "machine: {} {} target={}",
        report.machine.os, report.machine.arch, report.machine.rust_target
    );
    println!(
        "corpus: documents={} chunks={} artifact-bytes={:?}",
        report.corpus_documents, report.corpus_chunks, report.artifact_bytes
    );
    println!(
        "iterations: warmup={} repetitions={} queries={}",
        report.warmup_iterations, report.repetitions, report.query_count
    );
    print_timing("build", &report.build);
    print_timing("load", &report.load);
    print_timing("query", &report.query);
    print_timing("fusion", &report.fusion);
    println!(
        "response-bytes: samples={} min={} p50={} p95={} max={}",
        report.response_bytes.samples,
        report.response_bytes.min_bytes,
        report.response_bytes.p50_bytes,
        report.response_bytes.p95_bytes,
        report.response_bytes.max_bytes
    );
    println!(
        "rss-bytes: before={:?} after={:?} delta={:?}",
        report.rss_before_queries_bytes,
        report.rss_after_queries_bytes,
        report.rss_retained_delta_bytes
    );
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

#[cfg(feature = "semantic-fastembed")]
fn print_semantic_benchmark_report(report: &SemanticBenchmarkReport) {
    println!("mode: {:?}", report.mode);
    println!(
        "identity: model={} revision={} corpus={} build={}",
        report.model_id, report.model_revision, report.corpus_digest, report.build_identity
    );
    println!(
        "corpus: chunks={} vectors={} dimension={} artifact-bytes={} outer-batch={} intra-threads={:?}",
        report.corpus_chunks,
        report.vector_count,
        report.vector_dimension,
        report.artifact_bytes,
        report.outer_batch_size,
        report.intra_threads
    );
    if let Some(initialization) = &report.cold_initialization {
        print_timing("cold-initialization", initialization);
    }
    print_timing("first-query-after-build", &report.first_query_after_build);
    print_timing("build", &report.build);
    print_timing("embedding-input", &report.embedding_input);
    print_timing("provider-inference", &report.provider_inference);
    print_timing("vector-finalization", &report.vector_finalization);
    println!("embedding-input-bytes: {}", report.embedding_input_bytes);
    if let Some(statistics) = &report.token_statistics {
        println!(
            "tokens: real={} padded={} untruncated={} min={} median={} p95={} max={} truncated-chunks={} padding-ppm={}",
            statistics.total_real_tokens,
            statistics.total_padded_tokens,
            statistics.total_untruncated_tokens,
            statistics.min_tokens,
            statistics.median_tokens,
            statistics.p95_tokens,
            statistics.maximum_tokens,
            statistics.truncated_chunks,
            statistics.padding_ratio_ppm,
        );
    }
    print_timing("embedding", &report.embedding);
    for (limit, timing) in &report.exact_search {
        print_timing(&format!("exact-search-{limit}"), timing);
    }
    if let (Some(before), Some(after)) = (
        report.rss_before_queries_bytes,
        report.rss_after_queries_bytes,
    ) {
        println!(
            "rss: before={before} after={after} delta={:?}",
            report.rss_retained_delta_bytes
        );
    }
}

fn print_timing(name: &str, timing: &vesc_knowledge_index::benchmark::TimingDistribution) {
    println!(
        "{name}: samples={} min-us={} p50-us={} p95-us={} max-us={}",
        timing.samples, timing.min_us, timing.p50_us, timing.p95_us, timing.max_us
    );
}

fn resolve_refloat_root(manifest_dir: &Path) -> PathBuf {
    if let Ok(path) = env::var("VESC_REFLOAT_ROOT") {
        return PathBuf::from(path);
    }

    let workspace = manifest_dir.join("../..");
    let vendor = workspace.join("vendor/refloat");
    if vendor.is_dir() {
        return vendor;
    }

    PathBuf::from(env::var("HOME").unwrap_or_else(|_| "/".into())).join("projects/refloat")
}
