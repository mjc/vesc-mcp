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
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::benchmark::BakeoffCandidateSpec;
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::benchmark::{
    BakeoffCandidateReport, BakeoffReport, SemanticBenchmarkMatrixReport, SemanticBenchmarkReport,
    benchmark_semantic, benchmark_semantic_queries, benchmark_semantic_with_artifact,
};
use vesc_knowledge_index::benchmark::{BenchmarkReport, benchmark_lexical};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::build_git_artifacts_with_provider;
use vesc_knowledge_index::evaluation::{
    EvaluationMode, EvaluationQuery, EvaluationReport, EvaluationSuite, QualityThresholds,
    evaluate_quality_gate, evaluate_suite_with_mode,
};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::{
    Chunk, ChunkId, DocumentWindowVectors, EmbeddingProfile, EmbeddingProvider, FastEmbedProvider,
    FusionConfig, SemanticExecutionProvider, VectorArtifact, WindowAggregation,
    aggregate_window_vectors, build_allowlisted_artifacts_with_provider,
    build_embedded_artifacts_with_provider, configure_ort_verbose_logging, embedding_text,
    fuse_candidates, semantic_runtime_diagnostics, sequence_length_census_iter,
};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::{ContentDigest, NormalizedDocument, embedded_entries};
use vesc_knowledge_index::{DEFAULT_SEMANTIC_BATCH_SIZE, default_semantic_intra_threads};
use vesc_knowledge_index::{
    IndexBuilder, LexicalFilters, LexicalIndex, RepositoryId, Revision, active_generation_path,
    active_manifest_path, build_allowlisted_artifacts, build_embedded_artifacts, inspect_manifest,
    search_knowledge, search_lexical_knowledge, vesc_mcp_source_specs,
};
use vesc_knowledge_index::{
    LicenseStatus, TrustTier, build_git_artifacts,
    corpus::git::{GitCorpusPolicy, GitCorpusSource, GitIngestionObservations},
    release_repositories::{PINNED_RELEASE_REPOSITORIES, ReleaseRepositoryCache},
};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "sequence-census") {
        run_sequence_census(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "window-quality") {
        run_window_quality(&args[1..]);
        return;
    }
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
    if args.first().is_some_and(|arg| arg == "bakeoff") {
        run_bakeoff(&args[1..]);
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

#[cfg(feature = "semantic-fastembed")]
fn run_sequence_census(args: &[String]) {
    let model_dir = argument_value(args, "--semantic-model-dir")
        .map_or_else(|| panic!("--semantic-model-dir is required"), PathBuf::from);
    let max_lengths = argument_value(args, "--sequence-lengths")
        .unwrap_or_else(|| "64,128,256,512".into())
        .split(',')
        .map(|value| {
            value
                .parse::<usize>()
                .expect("--sequence-lengths must be comma-separated positive integers")
        })
        .collect::<Vec<_>>();
    let token_budget = argument_value(args, "--semantic-token-budget")
        .as_deref()
        .unwrap_or("4096")
        .parse::<usize>()
        .expect("--semantic-token-budget must be a positive integer");
    let artifact = argument_value(args, "--artifact").map(PathBuf::from);
    let (chunks, _) = semantic_benchmark_chunks(artifact.as_deref());
    let census = sequence_length_census_iter(
        &model_dir.join("tokenizer.json"),
        chunks.iter().map(embedding_text),
        &max_lengths,
        token_budget,
    )
    .unwrap_or_else(|error| panic!("measure sequence lengths: {error}"));
    match argument_value(args, "--format")
        .as_deref()
        .unwrap_or("json")
    {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&census).expect("serialize sequence census")
        ),
        "text" => println!("{census:#?}"),
        other => panic!("unsupported census format {other:?}; use json or text"),
    }
}

#[cfg(not(feature = "semantic-fastembed"))]
fn run_sequence_census(_args: &[String]) {
    panic!("sequence census requires the semantic-fastembed feature")
}

#[cfg(feature = "semantic-fastembed")]
fn run_window_quality(args: &[String]) {
    let artifact = argument_value(args, "--artifact")
        .map_or_else(|| panic!("--artifact is required"), PathBuf::from);
    let suite_path = argument_value(args, "--suite").map_or_else(
        || PathBuf::from("tests/evaluation/v2/queries.json"),
        PathBuf::from,
    );
    let suite: EvaluationSuite = serde_json::from_slice(
        &fs::read(&suite_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", suite_path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", suite_path.display()));
    let (chunks, corpus_digest) = semantic_benchmark_chunks(Some(&artifact));
    let manifest = inspect_manifest(&active_manifest_path(&artifact))
        .unwrap_or_else(|error| panic!("inspect quality artifact: {error}"));
    let chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    suite
        .validate_for_corpus(
            corpus_digest.as_ref(),
            manifest.corpus.document_count(),
            chunks.len(),
            &chunk_ids,
        )
        .unwrap_or_else(|error| panic!("validate quality suite: {error}"));
    let sample_size = argument_value(args, "--semantic-quality-chunks")
        .as_deref()
        .unwrap_or("128")
        .parse::<usize>()
        .expect("--semantic-quality-chunks must be a positive integer");
    let chunks = judged_chunk_sample(&chunks, &suite.queries, sample_size);
    let model_dir = argument_value(args, "--semantic-model-dir")
        .map_or_else(|| panic!("--semantic-model-dir is required"), PathBuf::from);
    let model_id = argument_value(args, "--semantic-model-id")
        .unwrap_or_else(|| panic!("--semantic-model-id is required"));
    let batch_size = argument_value(args, "--semantic-batch-size")
        .as_deref()
        .unwrap_or("8")
        .parse::<usize>()
        .expect("--semantic-batch-size must be positive");
    let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads_and_provider_and_graph_optimization(
        &model_dir,
        Some(batch_size),
        semantic_profile_with_args(&model_id, args),
        argument_value(args, "--semantic-intra-threads").map(|value| {
            value.parse::<usize>().expect("--semantic-intra-threads must be positive")
        }),
        semantic_execution_provider(args),
        semantic_graph_optimization_level(args),
    )
    .unwrap_or_else(|error| panic!("load quality model: {error}"));
    provider.set_lossless_windowing(args.iter().any(|arg| arg == "--semantic-lossless-windows"));
    let texts = chunks.iter().map(embedding_text).collect::<Vec<_>>();
    let windows = provider
        .embed_document_windows(&texts)
        .unwrap_or_else(|error| panic!("embed quality windows: {error}"));
    let mean = aggregate_window_rows(&windows, chunks.len(), WindowAggregation::Mean);
    let weighted =
        aggregate_window_rows(&windows, chunks.len(), WindowAggregation::TokenWeightedMean);
    let mean_report = evaluate_window_vectors(&suite.queries, &chunks, &mean, &mut provider);
    let weighted_report =
        evaluate_window_vectors(&suite.queries, &chunks, &weighted, &mut provider);
    let max_report = evaluate_max_window_vectors(&suite.queries, &chunks, &windows, &mut provider);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": 1,
            "corpus_digest": corpus_digest,
            "sample_chunks": chunks.len(),
            "windows": windows.vectors.len(),
            "mean": mean_report,
            "token_weighted_mean": weighted_report,
            "max_similarity": max_report,
        }))
        .expect("serialize window quality")
    );
}

#[cfg(not(feature = "semantic-fastembed"))]
fn run_window_quality(_args: &[String]) {
    panic!("window quality requires the semantic-fastembed feature")
}
#[allow(clippy::option_if_let_else, clippy::too_many_lines)]
fn run_build_default(args: &[String]) {
    let detected_args = detected_rx5700xt_8600g_build_args(args);
    let args = detected_args.as_deref().unwrap_or(args);
    let generated = argument_value(args, "--generated-dir").map_or_else(
        || PathBuf::from("crates/vesc-knowledge-index/generated"),
        PathBuf::from,
    );
    let staging = argument_value(args, "--staging-dir").map_or_else(
        || PathBuf::from("target/default-knowledge-artifacts"),
        PathBuf::from,
    );
    let sources = default_corpus_sources(args);
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
            let batch_size = argument_value(args, "--semantic-batch-size")
                .map_or(DEFAULT_SEMANTIC_BATCH_SIZE, |value| {
                    value
                        .parse::<usize>()
                        .expect("--semantic-batch-size must be an integer")
                });
            let intra_threads = argument_value(args, "--semantic-intra-threads")
                .map_or_else(default_semantic_intra_threads, |value| {
                    let threads = value
                        .parse::<usize>()
                        .expect("--semantic-intra-threads must be a positive integer");
                    assert!(
                        threads > 0,
                        "--semantic-intra-threads must be a positive integer"
                    );
                    threads
                });
            let length_bucketed = argument_value(args, "--semantic-length-bucketed")
                .is_none_or(|value| matches!(value.as_str(), "1" | "true" | "yes"));
            let lossless_windowing = args.iter().any(|arg| arg == "--semantic-lossless-windows");
            let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads_and_provider(
                &PathBuf::from(model_dir),
                Some(batch_size),
                semantic_profile_with_args(&model_id, args),
                Some(intra_threads),
                semantic_execution_provider(args),
            )
            .unwrap_or_else(|error| panic!("load semantic model: {error}"));
            provider.set_length_bucketed(length_bucketed);
            provider.set_lossless_windowing(lossless_windowing);
            provider.set_window_aggregation(semantic_window_aggregation(args));
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
        generation.join("manifest.json"),
        generated_generation.join("manifest.json"),
    )
    .unwrap_or_else(|error| panic!("copy default generation manifest: {error}"));
    fs::copy(
        generation.join("lexical.json"),
        generated_generation.join("lexical.json"),
    )
    .unwrap_or_else(|error| panic!("copy default lexical artifact: {error}"));
    let lexical_index = generation.join("lexical.tantivy");
    let generated_lexical_index = generated_generation.join("lexical.tantivy");
    fs::create_dir_all(&generated_lexical_index)
        .unwrap_or_else(|error| panic!("create default lexical index: {error}"));
    for entry in fs::read_dir(&lexical_index)
        .unwrap_or_else(|error| panic!("read default lexical index: {error}"))
    {
        let entry =
            entry.unwrap_or_else(|error| panic!("read default lexical index entry: {error}"));
        fs::copy(
            entry.path(),
            generated_lexical_index.join(entry.file_name()),
        )
        .unwrap_or_else(|error| panic!("copy default lexical index entry: {error}"));
    }
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
    print_git_observations(summary.observations.git_ingestion.as_ref());
    println!(
        "provenance-bytes: {}",
        summary.observations.provenance_bytes()
    );
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

fn default_corpus_sources(args: &[String]) -> Vec<GitCorpusSource> {
    const IDS: [&str; 4] = ["vesc", "vesc-tool", "refloat", "vesc-pkg"];
    let explicit = IDS.iter().any(|id| {
        argument_value(args, &format!("--{id}-path")).is_some()
            || argument_value(args, &format!("--{id}-revision")).is_some()
    });
    if explicit {
        return IDS
            .into_iter()
            .map(|id| {
                let path = argument_value(args, &format!("--{id}-path"))
                    .unwrap_or_else(|| panic!("--{id}-path is required in explicit source mode"));
                let revision =
                    argument_value(args, &format!("--{id}-revision")).unwrap_or_else(|| {
                        panic!("--{id}-revision is required in explicit source mode")
                    });
                corpus_source(
                    RepositoryId::try_from(id).expect("valid repository identifier"),
                    PathBuf::from(path),
                    Revision::try_from(revision).expect("valid immutable revision"),
                )
            })
            .collect();
    }

    let root = repository_cache_root(args).unwrap_or_else(|message| panic!("{message}"));
    let git = argument_value(args, "--git-bin")
        .map(PathBuf::from)
        .or_else(|| env::var_os("VESC_GIT_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("git"));
    ReleaseRepositoryCache::new(root.clone(), git)
        .maintain(&PINNED_RELEASE_REPOSITORIES)
        .unwrap_or_else(|error| {
            panic!(
                "prepare pinned release repositories in {}: {error}",
                root.display()
            )
        })
        .into_iter()
        .map(|repository| corpus_source(repository.id, repository.path, repository.revision))
        .collect()
}

fn corpus_source(
    repository_id: RepositoryId,
    repository_path: PathBuf,
    revision: Revision,
) -> GitCorpusSource {
    let mut policy = GitCorpusPolicy::default();
    policy.extensions.remove("md");
    GitCorpusSource {
        repository_path,
        repository_id,
        revision,
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy,
    }
}

fn repository_cache_root(args: &[String]) -> Result<PathBuf, &'static str> {
    select_repository_cache_root(
        argument_value(args, "--repository-cache").map(PathBuf::from),
        env::var_os("VESC_BENCHMARK_REPOSITORY_CACHE").map(PathBuf::from),
        env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        env::var_os("HOME").map(PathBuf::from),
    )
}

fn select_repository_cache_root(
    cli: Option<PathBuf>,
    configured: Option<PathBuf>,
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf, &'static str> {
    cli.or(configured)
        .or_else(|| xdg.map(|root| root.join("vesc-mcp/release-repositories")))
        .or_else(|| home.map(|root| root.join(".cache/vesc-mcp/release-repositories")))
        .ok_or(
            "release repository cache root is unknown; pass --repository-cache or set VESC_BENCHMARK_REPOSITORY_CACHE",
        )
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
    let detected_args = detected_rx5700xt_8600g_build_args(args);
    let args = detected_args.as_deref().unwrap_or(args);
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
    let semantic_batch_size = argument_value(args, "--semantic-batch-size").map_or(
        DEFAULT_SEMANTIC_BATCH_SIZE,
        |value| {
            value
                .parse::<usize>()
                .expect("--semantic-batch-size must be an integer")
        },
    );
    let semantic_intra_threads = argument_value(args, "--semantic-intra-threads").map_or_else(
        default_semantic_intra_threads,
        |value| {
            let threads = value
                .parse::<usize>()
                .expect("--semantic-intra-threads must be a positive integer");
            assert!(
                threads > 0,
                "--semantic-intra-threads must be a positive integer"
            );
            threads
        },
    );
    let semantic_length_bucketed = argument_value(args, "--semantic-length-bucketed")
        .is_none_or(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    #[cfg(feature = "semantic-fastembed")]
    let semantic_lossless_windowing = args.iter().any(|arg| arg == "--semantic-lossless-windows");
    let summary = if let Some(model_dir) = model_dir {
        let model_id = argument_value(args, "--semantic-model-id")
            .unwrap_or_else(|| panic!("--semantic-model-id is required with --semantic-model-dir"));
        let model_revision = argument_value(args, "--semantic-model-revision")
            .unwrap_or_else(|| {
                panic!("--semantic-model-revision is required with --semantic-model-dir")
            });
        #[cfg(feature = "semantic-fastembed")]
        {
            let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads_and_provider(
                &PathBuf::from(model_dir),
                Some(semantic_batch_size),
                semantic_profile_with_args(&model_id, args),
                Some(semantic_intra_threads),
                semantic_execution_provider(args),
            )
            .unwrap_or_else(|error| panic!("load semantic model: {error}"));
            provider.set_length_bucketed(semantic_length_bucketed);
            provider.set_lossless_windowing(semantic_lossless_windowing);
            provider.set_window_aggregation(semantic_window_aggregation(args));
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
            let _ = (
                model_dir,
                model_id,
                model_revision,
                semantic_batch_size,
                semantic_intra_threads,
                semantic_length_bucketed,
            );
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
    print_git_observations(summary.observations.git_ingestion.as_ref());
    println!(
        "provenance-bytes: {}",
        summary.observations.provenance_bytes()
    );
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

fn print_git_observations(observations: Option<&GitIngestionObservations>) {
    let Some(observations) = observations else {
        return;
    };
    println!("git-tree-walk-us: {}", observations.tree_walk_us);
    println!("git-candidate-sort-us: {}", observations.candidate_sort_us);
    println!("git-blob-load-us: {}", observations.blob_load_us);
    println!("git-binary-scan-us: {}", observations.binary_scan_us);
    println!(
        "git-utf8-normalization-us: {}",
        observations.utf8_normalization_us
    );
    println!(
        "git-document-metadata-us: {}",
        observations.document_metadata_us
    );
    println!("git-candidate-count: {}", observations.candidate_count);
    println!("git-blob-bytes-loaded: {}", observations.blob_bytes_loaded);
    println!("git-blob-cache-hits: {}", observations.blob_cache_hits);
    println!(
        "git-binary-rejections: {}",
        observations.binary_rejection_count
    );
    println!(
        "git-encoding-rejections: {}",
        observations.encoding_rejection_count
    );
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
    println!("documents: {}", manifest.corpus.document_count());
    println!("chunks: {}", manifest.corpus.chunk_count());
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
    let queries = read_evaluation_queries(&suite_path);
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
    let generation = active_generation_path(artifact)
        .unwrap_or_else(|error| panic!("resolve semantic artifact: {error}"));
    let lexical_path = generation.join("lexical.json");
    let vector_path = generation.join("vectors.bin");
    let lexical = LexicalIndex::open_search_artifact(&lexical_path)
        .unwrap_or_else(|error| panic!("open semantic lexical artifact: {error}"));
    let vector = VectorArtifact::open_artifact(&vector_path)
        .unwrap_or_else(|error| panic!("open semantic vector artifact: {error}"));
    let chunks: BTreeMap<_, _> = LexicalIndex::read_artifact_chunks(&lexical_path)
        .unwrap_or_else(|error| panic!("read semantic lexical artifact: {error}"))
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
#[serde(deny_unknown_fields)]
struct BakeoffConfig {
    schema: u16,
    corpus_digest: String,
    corpus_documents: usize,
    corpus_chunks: usize,
    candidates: Vec<BakeoffCandidateSpec>,
}

fn run_bakeoff(args: &[String]) {
    #[cfg(feature = "semantic-fastembed")]
    run_bakeoff_with_fastembed(args);
    #[cfg(not(feature = "semantic-fastembed"))]
    {
        let _ = args;
        panic!("bakeoff requires the semantic-fastembed feature");
    }
}

#[cfg(feature = "semantic-fastembed")]
#[allow(clippy::too_many_lines)]
fn run_bakeoff_with_fastembed(args: &[String]) {
    let artifact_root = argument_value(args, "--artifact").map_or_else(
        || panic!("--artifact is required for a bake-off"),
        PathBuf::from,
    );
    assert!(
        artifact_root.is_dir(),
        "--artifact must name a full artifact directory"
    );
    let suite_path = argument_value(args, "--suite").map_or_else(
        || PathBuf::from("tests/evaluation/v2/queries.json"),
        PathBuf::from,
    );
    let config_path = argument_value(args, "--config").map_or_else(
        || PathBuf::from("tests/benchmark/bakeoff-models.json"),
        PathBuf::from,
    );
    let model_root = argument_value(args, "--model-root")
        .map_or_else(|| PathBuf::from("target/models"), PathBuf::from);
    let format = argument_value(args, "--format").unwrap_or_else(|| "json".into());
    let warmup = argument_value(args, "--warmup")
        .as_deref()
        .unwrap_or("0")
        .parse::<usize>()
        .expect("--warmup must be an integer");
    let repetitions = argument_value(args, "--repetitions")
        .as_deref()
        .unwrap_or("1")
        .parse::<usize>()
        .expect("--repetitions must be an integer");
    assert!(repetitions > 0, "--repetitions must be positive");
    let batch_size = argument_value(args, "--semantic-batch-size")
        .as_deref()
        .unwrap_or("8")
        .parse::<usize>()
        .expect("--semantic-batch-size must be an integer");
    let intra_threads = argument_value(args, "--semantic-intra-threads").map(|value| {
        let threads = value
            .parse::<usize>()
            .expect("--semantic-intra-threads must be a positive integer");
        assert!(threads > 0, "--semantic-intra-threads must be positive");
        threads
    });
    let length_bucketed = argument_value(args, "--semantic-length-bucketed")
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    let max_length = argument_value(args, "--semantic-max-length").map(|value| {
        let length = value
            .parse::<usize>()
            .expect("--semantic-max-length must be a positive integer");
        assert!(length > 0, "--semantic-max-length must be positive");
        length
    });
    let lossless_windowing = args.iter().any(|arg| arg == "--semantic-lossless-windows");
    let execution_provider = semantic_execution_provider(args);
    let graph_optimization_level = semantic_graph_optimization_level(args);
    let verbose_ort = args.iter().any(|arg| arg == "--semantic-verbose-ort");
    configure_ort_verbose_logging(verbose_ort)
        .unwrap_or_else(|error| panic!("configure verbose ONNX Runtime logging: {error}"));
    let diagnostics = semantic_runtime_diagnostics(execution_provider)
        .unwrap_or_else(|error| panic!("inspect semantic runtime: {error}"));
    eprintln!(
        "semantic-runtime: {}",
        serde_json::to_string(&diagnostics).expect("serialize semantic runtime diagnostics")
    );

    let suite: EvaluationSuite =
        serde_json::from_str(&fs::read_to_string(&suite_path).unwrap_or_else(|error| {
            panic!("read bake-off suite {}: {error}", suite_path.display())
        }))
        .unwrap_or_else(|error| panic!("parse bake-off suite {}: {error}", suite_path.display()));
    let config: BakeoffConfig =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap_or_else(|error| {
            panic!("read bake-off config {}: {error}", config_path.display())
        }))
        .unwrap_or_else(|error| panic!("parse bake-off config {}: {error}", config_path.display()));
    assert_eq!(config.schema, 1, "unsupported bake-off config schema");
    let candidate_filter = argument_value(args, "--candidate");
    let candidates_to_run = config
        .candidates
        .into_iter()
        .filter(|candidate| {
            candidate_filter
                .as_deref()
                .is_none_or(|name| name == candidate.name)
        })
        .collect::<Vec<_>>();
    if candidate_filter.is_some() {
        assert_eq!(
            candidates_to_run.len(),
            1,
            "--candidate must select one candidate"
        );
    } else {
        assert!(!candidates_to_run.is_empty(), "bake-off has no candidates");
    }
    let (mut chunks, corpus_digest) = semantic_benchmark_chunks(Some(&artifact_root));
    let manifest = inspect_manifest(&active_manifest_path(&artifact_root))
        .unwrap_or_else(|error| panic!("inspect bake-off artifact: {error}"));
    let corpus_documents = manifest.corpus.document_count();
    let chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    suite
        .validate_for_corpus(
            corpus_digest.as_ref(),
            corpus_documents,
            chunks.len(),
            &chunk_ids,
        )
        .unwrap_or_else(|error| panic!("validate bake-off suite: {error}"));
    assert_eq!(config.corpus_digest, corpus_digest.to_string());
    assert_eq!(config.corpus_documents, corpus_documents);
    assert_eq!(config.corpus_chunks, chunks.len());
    let corpus_chunks = chunks.len();
    let bounded_quality_chunks = argument_value(args, "--semantic-quality-chunks").map(|value| {
        let size = value
            .parse::<usize>()
            .expect("--semantic-quality-chunks must be a positive integer");
        assert!(size > 0, "--semantic-quality-chunks must be positive");
        chunks = judged_chunk_sample(&chunks, &suite.queries, size);
        size
    });

    let generation = artifact_root
        .join("generations")
        .join(corpus_digest.to_string());
    let lexical = LexicalIndex::open_search_artifact(&generation.join("lexical.json"))
        .unwrap_or_else(|error| panic!("open bake-off lexical artifact: {error}"));
    let chunks_by_id = chunks
        .iter()
        .map(|chunk| (chunk.chunk_id.clone(), chunk))
        .collect::<BTreeMap<_, _>>();
    let lexical_report = evaluate_suite_with_mode(
        &suite.queries,
        EvaluationMode::Lexical,
        Vec::new(),
        |query| {
            lexical
                .search(query, &LexicalFilters::default(), 50)
                .unwrap_or_else(|error| panic!("lexical bake-off search: {error}"))
                .into_iter()
                .flat_map(|hit| stable_chunk_result_ids(&hit.chunk))
                .collect()
        },
    );
    let peak_rss = parse_peak_rss(args);
    let mut warnings = Vec::new();
    if let Some(size) = bounded_quality_chunks {
        warnings.push(format!(
            "bounded quality gate: all judged-relevant chunks plus representative decoys, requested {size}, selected {}",
            chunks.len()
        ));
    }
    if peak_rss.is_empty() {
        warnings.push(
            "peak RSS is externally measured; pass --peak-rss-bytes name=bytes,... to attach it"
                .into(),
        );
    }
    let query_texts = suite
        .queries
        .iter()
        .map(|query| query.text.clone())
        .collect::<Vec<_>>();
    let mut candidates = Vec::with_capacity(candidates_to_run.len());
    for candidate in candidates_to_run {
        let model_dir = model_root.join(&candidate.directory);
        let model_path = model_dir.join("model.onnx");
        let model_bytes = fs::metadata(&model_path)
            .unwrap_or_else(|error| panic!("inspect {}: {error}", model_path.display()))
            .len();
        assert_eq!(
            model_bytes, candidate.onnx_bytes,
            "ONNX byte count mismatch for {}",
            candidate.name
        );
        let actual_digest = vesc_knowledge_index::hardware::sha256_file(&model_path)
            .unwrap_or_else(|error| panic!("hash {}: {error}", model_path.display()));
        assert_eq!(
            actual_digest, candidate.onnx_sha256,
            "ONNX SHA-256 mismatch for {}",
            candidate.name
        );
        let initialization_started = std::time::Instant::now();
        let mut profile = semantic_profile(&candidate.model_id);
        if let Some(max_length) = max_length {
            assert!(
                max_length <= profile.max_length,
                "--semantic-max-length cannot exceed the model profile maximum"
            );
            profile.max_length = max_length;
        }
        let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads_and_provider_and_graph_optimization(
            &model_dir,
            Some(batch_size),
            profile,
            intra_threads,
            execution_provider,
            graph_optimization_level,
        )
        .unwrap_or_else(|error| panic!("load bake-off candidate {}: {error}", candidate.name));
        provider.set_length_bucketed(length_bucketed);
        provider.set_lossless_windowing(lossless_windowing);
        provider.set_window_aggregation(semantic_window_aggregation(args));
        let initialization = vesc_knowledge_index::benchmark::TimingDistribution::single(
            u64::try_from(initialization_started.elapsed().as_micros()).unwrap_or(u64::MAX),
        );
        let (mut benchmark, vector) = benchmark_semantic_with_artifact(
            &mut provider,
            &chunks,
            &query_texts,
            &candidate.model_id,
            &candidate.model_revision,
            &corpus_digest,
            &[5, 10, 20, 50],
            warmup,
            repetitions,
        )
        .unwrap_or_else(|error| panic!("benchmark bake-off candidate {}: {error}", candidate.name));
        benchmark.cold_initialization = Some(initialization);
        benchmark.intra_threads = intra_threads;
        benchmark.length_bucketed = length_bucketed;
        benchmark.effective_max_length = Some(provider.max_length());
        benchmark.token_statistics = Some(
            provider
                .token_statistics_iter(chunks.iter().map(embedding_text))
                .unwrap_or_else(|error| {
                    panic!("measure token statistics for {}: {error}", candidate.name)
                }),
        );
        benchmark.peak_rss_bytes = peak_rss.get(&candidate.name).copied();
        let semantic = evaluate_provider(
            &suite.queries,
            EvaluationMode::Semantic,
            &lexical,
            &chunks_by_id,
            &vector,
            &mut provider,
        );
        let hybrid = evaluate_provider(
            &suite.queries,
            EvaluationMode::Hybrid,
            &lexical,
            &chunks_by_id,
            &vector,
            &mut provider,
        );
        candidates.push(BakeoffCandidateReport {
            candidate,
            benchmark,
            semantic,
            hybrid,
        });
    }
    let machine = candidates
        .first()
        .map(|candidate| candidate.benchmark.machine.clone())
        .expect("bake-off candidates are nonempty");
    let report = BakeoffReport {
        schema: 1,
        suite_id: suite.suite_id,
        corpus_digest,
        corpus_documents,
        corpus_chunks,
        evaluated_chunks: chunks.len(),
        lexical: lexical_report,
        candidates,
        machine,
        warnings,
    };
    let json = serde_json::to_string_pretty(&report).expect("serialize bake-off report");
    let markdown = report.to_markdown();
    if let Some(path) = argument_value(args, "--json-out") {
        fs::write(&path, &json)
            .unwrap_or_else(|error| panic!("write bake-off JSON {path}: {error}"));
    }
    if let Some(path) = argument_value(args, "--markdown-out") {
        fs::write(&path, &markdown)
            .unwrap_or_else(|error| panic!("write bake-off Markdown {path}: {error}"));
    }
    match format.as_str() {
        "json" => println!("{json}"),
        "markdown" => print!("{markdown}"),
        other => panic!("unsupported bake-off format {other:?}; use json or markdown"),
    }
}

#[cfg(feature = "semantic-fastembed")]
fn judged_chunk_sample(chunks: &[Chunk], queries: &[EvaluationQuery], size: usize) -> Vec<Chunk> {
    let relevant = queries
        .iter()
        .flat_map(|query| {
            query
                .relevant
                .iter()
                .filter_map(|(id, &score)| (score > 0).then_some(id.as_str()))
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected = chunks
        .iter()
        .enumerate()
        .filter_map(|(index, chunk)| {
            stable_chunk_result_ids(chunk)
                .iter()
                .any(|id| relevant.contains(id.as_str()))
                .then_some(index)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let target = size.max(selected.len()).min(chunks.len());
    if selected.len() < target {
        let remaining = chunks.len().saturating_sub(selected.len());
        let step = remaining.div_ceil(target - selected.len()).max(1);
        for index in (0..chunks.len()).step_by(step) {
            selected.insert(index);
            if selected.len() == target {
                break;
            }
        }
    }
    selected
        .into_iter()
        .map(|index| chunks[index].clone())
        .collect()
}

#[cfg(feature = "semantic-fastembed")]
fn aggregate_window_rows(
    windows: &DocumentWindowVectors,
    documents: usize,
    aggregation: WindowAggregation,
) -> Vec<Vec<f32>> {
    (0..documents)
        .map(|owner| {
            let start = windows
                .owners
                .partition_point(|&candidate| candidate < owner);
            let end = windows
                .owners
                .partition_point(|&candidate| candidate <= owner);
            aggregate_window_vectors(
                &windows.vectors[start..end],
                &windows.token_counts[start..end],
                aggregation,
            )
            .unwrap_or_else(|error| panic!("aggregate document windows: {error}"))
        })
        .collect()
}

#[cfg(feature = "semantic-fastembed")]
fn evaluate_window_vectors(
    queries: &[EvaluationQuery],
    chunks: &[Chunk],
    vectors: &[Vec<f32>],
    provider: &mut FastEmbedProvider,
) -> EvaluationReport {
    evaluate_suite_with_mode(queries, EvaluationMode::Semantic, Vec::new(), |query| {
        let vector = provider
            .embed_query(&vesc_knowledge_index::semantic_query_text(query))
            .unwrap_or_else(|error| panic!("embed window-quality query: {error}"));
        let scores = vectors
            .iter()
            .map(|candidate| dot_product(candidate, &vector))
            .collect::<Vec<_>>();
        rank_chunk_scores(chunks, &scores)
    })
}

#[cfg(feature = "semantic-fastembed")]
fn evaluate_max_window_vectors(
    queries: &[EvaluationQuery],
    chunks: &[Chunk],
    windows: &DocumentWindowVectors,
    provider: &mut FastEmbedProvider,
) -> EvaluationReport {
    evaluate_suite_with_mode(queries, EvaluationMode::Semantic, Vec::new(), |query| {
        let vector = provider
            .embed_query(&vesc_knowledge_index::semantic_query_text(query))
            .unwrap_or_else(|error| panic!("embed max-window query: {error}"));
        let mut scores = vec![f32::NEG_INFINITY; chunks.len()];
        for (candidate, &owner) in windows.vectors.iter().zip(&windows.owners) {
            scores[owner] = scores[owner].max(dot_product(candidate, &vector));
        }
        rank_chunk_scores(chunks, &scores)
    })
}

#[cfg(feature = "semantic-fastembed")]
fn dot_product(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

#[cfg(feature = "semantic-fastembed")]
fn rank_chunk_scores(chunks: &[Chunk], scores: &[f32]) -> Vec<String> {
    let mut order = (0..chunks.len()).collect::<Vec<_>>();
    order.sort_unstable_by(|&left, &right| {
        scores[right]
            .total_cmp(&scores[left])
            .then_with(|| chunks[left].chunk_id.cmp(&chunks[right].chunk_id))
    });
    order
        .into_iter()
        .take(50)
        .flat_map(|index| stable_chunk_result_ids(&chunks[index]))
        .collect()
}

#[cfg(feature = "semantic-fastembed")]
fn evaluate_provider(
    queries: &[EvaluationQuery],
    mode: EvaluationMode,
    lexical: &LexicalIndex,
    chunks: &BTreeMap<ChunkId, &Chunk>,
    vector: &VectorArtifact,
    provider: &mut FastEmbedProvider,
) -> EvaluationReport {
    evaluate_suite_with_mode(queries, mode, Vec::new(), |query| {
        let lexical_hits = if mode == EvaluationMode::Hybrid {
            lexical
                .search(query, &LexicalFilters::default(), 50)
                .unwrap_or_else(|error| panic!("lexical provider search: {error}"))
        } else {
            Vec::new()
        };
        let query_vector = provider
            .embed_query(&vesc_knowledge_index::semantic_query_text(query))
            .unwrap_or_else(|error| panic!("embed evaluation query: {error}"));
        let semantic_hits = vector
            .search(&query_vector, 50)
            .unwrap_or_else(|error| panic!("search evaluation vectors: {error}"));
        if mode == EvaluationMode::Semantic {
            semantic_hits
                .into_iter()
                .filter_map(|hit| chunks.get(&hit.chunk_id))
                .copied()
                .flat_map(stable_chunk_result_ids)
                .collect()
        } else {
            fuse_candidates(
                &lexical_hits,
                &semantic_hits,
                chunks,
                FusionConfig {
                    limit: 50,
                    ..FusionConfig::default()
                },
            )
            .into_iter()
            .flat_map(|hit| stable_chunk_result_ids(&hit.chunk))
            .collect()
        }
    })
}

#[cfg(feature = "semantic-fastembed")]
fn stable_chunk_result_ids(chunk: &Chunk) -> Vec<String> {
    vec![chunk.chunk_id.to_string()]
}

#[cfg(feature = "semantic-fastembed")]
fn parse_peak_rss(args: &[String]) -> BTreeMap<String, u64> {
    argument_value(args, "--peak-rss-bytes")
        .map(|value| {
            value
                .split(',')
                .map(|entry| {
                    let (name, bytes) = entry
                        .split_once('=')
                        .unwrap_or_else(|| panic!("--peak-rss-bytes entries must be name=bytes"));
                    (
                        name.to_owned(),
                        bytes.parse::<u64>().unwrap_or_else(|error| {
                            panic!("invalid peak RSS bytes {bytes}: {error}")
                        }),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
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
        if args.iter().any(|arg| arg == "--semantic-query-only") {
            run_semantic_query_benchmark(
                args,
                artifact.as_deref(),
                &suite_path,
                &format,
                warmup,
                repetitions,
            );
            return;
        }
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
    let queries = read_evaluation_queries(&suite_path);
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
fn run_semantic_query_benchmark(
    args: &[String],
    artifact_root: Option<&Path>,
    suite_path: &Path,
    format: &str,
    warmup: usize,
    repetitions: usize,
) {
    let artifact_root =
        artifact_root.unwrap_or_else(|| panic!("--artifact is required for --semantic-query-only"));
    let model_dir = argument_value(args, "--semantic-model-dir")
        .map_or_else(|| panic!("--semantic-model-dir is required"), PathBuf::from);
    let model_id = argument_value(args, "--semantic-model-id")
        .unwrap_or_else(|| panic!("--semantic-model-id is required"));
    let batch_size = argument_value(args, "--semantic-batch-size")
        .as_deref()
        .unwrap_or("8")
        .parse::<usize>()
        .expect("--semantic-batch-size must be a positive integer");
    let intra_threads = argument_value(args, "--semantic-intra-threads").map(|value| {
        value
            .parse::<usize>()
            .expect("--semantic-intra-threads must be a positive integer")
    });
    let limits = argument_value(args, "--limits")
        .unwrap_or_else(|| "5,10,20,50".into())
        .split(',')
        .map(|value| value.parse::<usize>().expect("invalid exact-search limit"))
        .collect::<Vec<_>>();
    let queries = read_evaluation_queries(suite_path)
        .into_iter()
        .map(|query| query.text)
        .collect::<Vec<_>>();
    let vector_path = argument_value(args, "--semantic-vector-artifact").map_or_else(
        || {
            active_generation_path(artifact_root)
                .unwrap_or_else(|error| panic!("inspect semantic query artifact: {error}"))
                .join("vectors.bin")
        },
        PathBuf::from,
    );
    let vector = VectorArtifact::open_artifact(&vector_path)
        .unwrap_or_else(|error| panic!("open semantic query artifact: {error}"));
    let initialization_started = std::time::Instant::now();
    let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads_and_provider_and_graph_optimization(
        &model_dir,
        Some(batch_size),
        semantic_profile_with_args(&model_id, args),
        intra_threads,
        semantic_execution_provider(args),
        semantic_graph_optimization_level(args),
    )
    .unwrap_or_else(|error| panic!("load semantic query model: {error}"));
    let initialization = vesc_knowledge_index::benchmark::TimingDistribution::single(
        u64::try_from(initialization_started.elapsed().as_micros()).unwrap_or(u64::MAX),
    );
    let mut report = benchmark_semantic_queries(
        &mut provider,
        &vector,
        &queries,
        &limits,
        warmup,
        repetitions,
    )
    .unwrap_or_else(|error| panic!("run semantic query benchmark: {error}"));
    report.cold_initialization = Some(initialization);
    match format {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize semantic query benchmark")
        ),
        "text" => println!("{report:#?}"),
        other => panic!("unsupported query benchmark format {other:?}; use json or text"),
    }
}

#[cfg(not(feature = "semantic-fastembed"))]
fn run_semantic_query_benchmark(
    _args: &[String],
    _artifact_root: Option<&Path>,
    _suite_path: &Path,
    _format: &str,
    _warmup: usize,
    _repetitions: usize,
) {
    panic!("semantic query benchmarks require the semantic-fastembed feature");
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
    let execution_provider = semantic_execution_provider(args);
    let graph_optimization_level = semantic_graph_optimization_level(args);
    let verbose_ort = args.iter().any(|arg| arg == "--semantic-verbose-ort");
    configure_ort_verbose_logging(verbose_ort)
        .unwrap_or_else(|error| panic!("configure verbose ONNX Runtime logging: {error}"));
    let diagnostics = semantic_runtime_diagnostics(execution_provider)
        .unwrap_or_else(|error| panic!("inspect semantic runtime: {error}"));
    eprintln!(
        "semantic-runtime: {}",
        serde_json::to_string(&diagnostics).expect("serialize semantic runtime diagnostics")
    );
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
    let lossless_windowing = args.iter().any(|arg| arg == "--semantic-lossless-windows");
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
    let token_statistics_only = args
        .iter()
        .any(|arg| arg == "--semantic-token-statistics-only");
    let longest_chunks = argument_value(args, "--semantic-longest-chunks").map(|value| {
        let size = value
            .parse::<usize>()
            .expect("--semantic-longest-chunks must be a positive integer");
        assert!(
            size > 0,
            "--semantic-longest-chunks must be a positive integer"
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
    let queries = read_evaluation_queries(suite_path);
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
    let initialization_started = std::time::Instant::now();
    let mut provider = FastEmbedProvider::from_model_dir_with_profile_and_threads_and_provider_and_graph_optimization(
        &model_dir,
        Some(batch_sizes[0]),
        semantic_profile_with_args(&model_id, args),
        intra_threads,
        execution_provider,
        graph_optimization_level,
    )
    .unwrap_or_else(|error| panic!("load semantic model: {error}"));
    provider.set_lossless_windowing(lossless_windowing);
    provider.set_window_aggregation(semantic_window_aggregation(args));
    let chunks = if length_bucketed {
        let lengths = provider
            .token_lengths_iter(chunks.iter().map(embedding_text))
            .unwrap_or_else(|error| panic!("measure token lengths: {error}"));
        let mut indexed = chunks.into_iter().zip(lengths).collect::<Vec<_>>();
        indexed.sort_unstable_by(|(left_chunk, left_length), (right_chunk, right_length)| {
            left_length
                .cmp(right_length)
                .then_with(|| left_chunk.path.cmp(&right_chunk.path))
                .then_with(|| left_chunk.ordinal.cmp(&right_chunk.ordinal))
                .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
        });
        indexed
            .into_iter()
            .map(|(chunk, _)| chunk)
            .collect::<Vec<_>>()
    } else {
        chunks
    };
    let chunks = if let Some(size) = longest_chunks {
        let lengths = provider
            .token_lengths_iter(chunks.iter().map(embedding_text))
            .unwrap_or_else(|error| panic!("measure token lengths: {error}"));
        let mut indexed = chunks.into_iter().zip(lengths).collect::<Vec<_>>();
        indexed.sort_unstable_by(|(left_chunk, left_length), (right_chunk, right_length)| {
            right_length
                .cmp(left_length)
                .then_with(|| left_chunk.path.cmp(&right_chunk.path))
                .then_with(|| left_chunk.ordinal.cmp(&right_chunk.ordinal))
                .then_with(|| left_chunk.chunk_id.cmp(&right_chunk.chunk_id))
        });
        indexed
            .into_iter()
            .take(size)
            .map(|(chunk, _)| chunk)
            .collect::<Vec<_>>()
    } else {
        chunks
    };
    if token_statistics_only {
        let statistics = provider
            .token_statistics_iter(chunks.iter().map(embedding_text))
            .unwrap_or_else(|error| panic!("measure semantic token statistics: {error}"));
        match format {
            "json" => println!(
                "{}",
                serde_json::to_string_pretty(&statistics)
                    .expect("serialize semantic token statistics")
            ),
            "text" => println!("{statistics:#?}"),
            other => panic!("unsupported format {other:?} for token statistics; use json or text"),
        }
        return;
    }
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
                .token_statistics_iter(chunks.iter().map(embedding_text))
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
                active_generation_path(root)
                    .unwrap_or_else(|error| panic!("resolve benchmark artifact: {error}"))
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
    let mut chunks = LexicalIndex::read_artifact_chunks(&path)
        .unwrap_or_else(|error| panic!("read benchmark lexical artifact: {error}"));
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
        || {
            search_lexical_knowledge(query, None, 50)
                .unwrap_or_else(|error| panic!("search embedded lexical artifact: {error}"))
        },
        |root| {
            let path = if root.is_file() {
                root.to_owned()
            } else {
                active_generation_path(root)
                    .unwrap_or_else(|error| panic!("inspect lexical evaluation artifact: {error}"))
                    .join("lexical.json")
            };
            LexicalIndex::open_search_artifact(&path)
                .unwrap_or_else(|error| panic!("open lexical evaluation artifact: {error}"))
                .search(query, &LexicalFilters::default(), 50)
                .unwrap_or_else(|error| panic!("search lexical evaluation artifact: {error}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "open lexical evaluation artifact")]
    fn incompatible_lexical_artifact_is_an_explicit_error() {
        let artifact = tempfile::NamedTempFile::new().expect("temporary artifact");
        fs::write(artifact.path(), b"legacy-format").expect("invalid artifact fixture");

        let _ = lexical_result_ids("query", Some(artifact.path()));
    }

    #[test]
    fn release_repository_cache_root_has_explicit_stable_precedence() {
        assert_eq!(
            select_repository_cache_root(
                Some("cli".into()),
                Some("configured".into()),
                Some("xdg".into()),
                Some("home".into()),
            ),
            Ok(PathBuf::from("cli"))
        );
        assert_eq!(
            select_repository_cache_root(
                None,
                Some("configured".into()),
                Some("xdg".into()),
                Some("home".into()),
            ),
            Ok(PathBuf::from("configured"))
        );
        assert_eq!(
            select_repository_cache_root(None, None, Some("xdg".into()), Some("home".into())),
            Ok(PathBuf::from("xdg/vesc-mcp/release-repositories"))
        );
        assert!(select_repository_cache_root(None, None, None, None).is_err());
    }
}

fn read_evaluation_queries(path: &Path) -> Vec<EvaluationQuery> {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read evaluation suite {}: {error}", path.display()));
    if raw.trim_start().starts_with('{') {
        serde_json::from_str::<EvaluationSuite>(&raw)
            .unwrap_or_else(|error| panic!("parse evaluation suite {}: {error}", path.display()))
            .queries
    } else {
        serde_json::from_str::<Vec<EvaluationQuery>>(&raw)
            .unwrap_or_else(|error| panic!("parse evaluation suite {}: {error}", path.display()))
    }
}

fn argument_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn detected_rx5700xt_8600g_build_args(args: &[String]) -> Option<Vec<String>> {
    if argument_value(args, "--semantic-model-dir").is_some()
        || args.iter().any(|arg| arg == "--no-semantic")
    {
        return None;
    }
    let root = std::env::current_dir().ok()?;
    let profile = vesc_knowledge_index::Rx5700Xt8600gProfile::detect(&root)?;
    Some(rx5700xt_8600g_build_args(args, &profile))
}

fn rx5700xt_8600g_build_args(
    args: &[String],
    profile: &vesc_knowledge_index::Rx5700Xt8600gProfile,
) -> Vec<String> {
    let mut resolved = args.to_vec();
    resolved.extend([
        "--semantic-model-dir".into(),
        profile.ingestion_model_dir.display().to_string(),
        "--semantic-model-id".into(),
        vesc_knowledge_index::JINA_CODE_MODEL_ID.into(),
        "--semantic-model-revision".into(),
        vesc_knowledge_index::JINA_CODE_MODEL_REVISION.into(),
        "--semantic-provider".into(),
        "migraphx".into(),
        "--semantic-device-id".into(),
        "0".into(),
        "--semantic-max-length".into(),
        vesc_knowledge_index::JINA_CODE_INGEST_MAX_LENGTH.to_string(),
        "--semantic-batch-size".into(),
        vesc_knowledge_index::JINA_CODE_INGEST_BATCH_SIZE.to_string(),
        "--semantic-length-bucketed".into(),
        "true".into(),
        "--semantic-lossless-windows".into(),
        "--semantic-window-aggregation".into(),
        "token-weighted-mean".into(),
    ]);
    resolved
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_execution_provider(args: &[String]) -> SemanticExecutionProvider {
    let provider = argument_value(args, "--semantic-provider")
        .unwrap_or_else(|| "auto".into())
        .to_ascii_lowercase();
    match provider.as_str() {
        "auto" => SemanticExecutionProvider::Auto,
        "cpu" => SemanticExecutionProvider::Cpu,
        "coreml" => SemanticExecutionProvider::CoreMl,
        "migraphx" => {
            let device_id = argument_value(args, "--semantic-device-id").map_or(0, |value| {
                value
                    .parse::<i32>()
                    .expect("--semantic-device-id must be a signed integer")
            });
            SemanticExecutionProvider::Migraphx { device_id }
        }
        "rocm" => {
            let device_id = argument_value(args, "--semantic-device-id").map_or(0, |value| {
                value
                    .parse::<i32>()
                    .expect("--semantic-device-id must be a signed integer")
            });
            SemanticExecutionProvider::Rocm { device_id }
        }
        other => {
            panic!(
                "unsupported --semantic-provider {other:?}; use auto, cpu, coreml, migraphx, or rocm"
            )
        }
    }
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_graph_optimization_level(
    args: &[String],
) -> ort::session::builder::GraphOptimizationLevel {
    let level = argument_value(args, "--semantic-graph-optimization-level")
        .unwrap_or_else(|| "3".into())
        .parse::<u8>()
        .expect("--semantic-graph-optimization-level must be 0, 1, 2, or 3");
    match level {
        0 => ort::session::builder::GraphOptimizationLevel::Disable,
        1 => ort::session::builder::GraphOptimizationLevel::Level1,
        2 => ort::session::builder::GraphOptimizationLevel::Level2,
        3 => ort::session::builder::GraphOptimizationLevel::Level3,
        other => {
            panic!("unsupported --semantic-graph-optimization-level {other}; use 0, 1, 2, or 3")
        }
    }
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_window_aggregation(args: &[String]) -> WindowAggregation {
    match argument_value(args, "--semantic-window-aggregation").as_deref() {
        None | Some("mean") => WindowAggregation::Mean,
        Some("token-weighted-mean") => WindowAggregation::TokenWeightedMean,
        Some(other) => panic!(
            "unsupported --semantic-window-aggregation {other:?}; use mean or token-weighted-mean"
        ),
    }
}

#[cfg(feature = "semantic-fastembed")]
fn embedding_profile(model_id: &str) -> EmbeddingProfile {
    EmbeddingProfile::for_model_id(model_id)
        .unwrap_or_else(|| panic!("no embedding profile is registered for {model_id}"))
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_profile(model_id: &str) -> EmbeddingProfile {
    embedding_profile(model_id)
}

#[cfg(feature = "semantic-fastembed")]
fn semantic_profile_with_args(model_id: &str, args: &[String]) -> EmbeddingProfile {
    let mut profile = semantic_profile(model_id);
    if let Some(value) = argument_value(args, "--semantic-max-length") {
        let max_length = value
            .parse::<usize>()
            .expect("--semantic-max-length must be a positive integer");
        assert!(max_length > 0, "--semantic-max-length must be positive");
        assert!(
            max_length <= profile.max_length,
            "--semantic-max-length cannot exceed the model profile maximum"
        );
        profile.max_length = max_length;
    }
    profile
}

#[cfg(all(test, feature = "semantic-fastembed"))]
mod semantic_profile_tests {
    use super::*;

    #[test]
    fn semantic_profile_honors_shorter_max_length() {
        let args = vec!["--semantic-max-length".to_string(), "512".to_string()];

        let profile = semantic_profile_with_args("jinaai/jina-embeddings-v2-base-code", &args);

        assert_eq!(profile.max_length, 512);
    }

    #[test]
    fn semantic_window_aggregation_is_explicit() {
        let args = vec![
            "--semantic-window-aggregation".to_string(),
            "token-weighted-mean".to_string(),
        ];

        assert_eq!(
            semantic_window_aggregation(&args),
            WindowAggregation::TokenWeightedMean
        );
    }

    #[test]
    fn rx5700xt_8600g_build_args_select_measured_ingestion_path() {
        let profile = vesc_knowledge_index::Rx5700Xt8600gProfile {
            ingestion_model_dir: PathBuf::from("fp16"),
            query_model_dir: PathBuf::from("int8"),
            artifact_dir: PathBuf::from("artifact"),
        };
        let args = rx5700xt_8600g_build_args(&["build".into()], &profile);

        assert_eq!(
            argument_value(&args, "--semantic-model-dir").as_deref(),
            Some("fp16")
        );
        assert_eq!(
            argument_value(&args, "--semantic-provider").as_deref(),
            Some("migraphx")
        );
        assert_eq!(
            argument_value(&args, "--semantic-device-id").as_deref(),
            Some("0")
        );
        assert_eq!(
            argument_value(&args, "--semantic-max-length").as_deref(),
            Some("64")
        );
        assert_eq!(
            argument_value(&args, "--semantic-batch-size").as_deref(),
            Some("64")
        );
        assert_eq!(
            argument_value(&args, "--semantic-window-aggregation").as_deref(),
            Some("token-weighted-mean")
        );
        assert!(args.iter().any(|arg| arg == "--semantic-lossless-windows"));
    }

    #[test]
    fn no_semantic_disables_the_hardware_default() {
        assert!(detected_rx5700xt_8600g_build_args(&["--no-semantic".into()]).is_none());
    }
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
