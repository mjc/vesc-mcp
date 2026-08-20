#![allow(clippy::must_use_candidate)] // Gungraun generates the public benchmark shim.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use gungraun::prelude::*;
use gungraun::{Dhat, DhatMetric, Massif};
use tempfile::{TempDir, tempdir};
use vesc_knowledge_index::benchmark::{
    EmbeddingProjectionFixture, benchmark_embedding_projection_from_fixture, embedding_chunk_ids,
    prepare_embedding_projection,
};
#[cfg(feature = "semantic-fastembed")]
use vesc_knowledge_index::bounded_document_windows;
use vesc_knowledge_index::corpus::git::{
    GitCorpusLimits, GitCorpusPolicy, GitCorpusSource, ingest_git_commit,
};
use vesc_knowledge_index::{
    Chunk, ChunkId, ChunkingConfig, ContentDigest, FakeEmbeddingProvider, GitHistoryTip,
    LexicalFilters, LexicalHit, LexicalIndex, LicenseStatus, NormalizedDocument,
    PreviousGitHistoryArtifact, RepositoryId, Revision, SourceKind, TrustTier, VectorArtifact,
    build_git_history_artifacts_from_previous, build_git_history_artifacts_incrementally,
    chunk_document, ingest_git_history_fast_forward,
};

const CHUNK_COUNT: usize = 256;

fn fixture_document() -> NormalizedDocument {
    let section = r"
## Observer update

```c
static void observer_update(float current, float voltage) {
    motor_state.phase = current * voltage;
}
```

The observer feeds the motor-control state estimator and fault checks.
";
    NormalizedDocument::new(
        "Motor observer",
        SourceKind::Fixture,
        RepositoryId::try_from("benchmark").expect("repository ID"),
        Revision::try_from("benchmark-v1").expect("revision"),
        "src/observer.md",
        "text/markdown",
        section.repeat(256),
    )
    .expect("fixture document")
}

fn fixture_chunks() -> Vec<Chunk> {
    let repository = RepositoryId::try_from("benchmark").expect("repository ID");
    let revision = Revision::try_from("benchmark-v1").expect("revision");
    (0..CHUNK_COUNT)
        .map(|index| {
            NormalizedDocument::new(
                format!("Motor control source {index}"),
                SourceKind::Fixture,
                repository.clone(),
                revision.clone(),
                format!("src/motor_{index:03}.c"),
                "text/x-c",
                format!(
                    "static void motor_{index:03}_update(float current) {{ observer_update(current); }}\n\
                     // motor control observer current voltage fault state\n"
                ),
            )
            .expect("fixture document")
            .catalog_chunk()
            .expect("fixture chunk")
        })
        .collect()
}

struct ArtifactFixture {
    _root: TempDir,
    path: PathBuf,
}

fn fixture_artifact() -> ArtifactFixture {
    let root = tempdir().expect("artifact root");
    let path = root.path().join("lexical.json");
    LexicalIndex::build(&fixture_chunks())
        .expect("lexical index")
        .write_artifact(&path)
        .expect("lexical artifact");
    ArtifactFixture { _root: root, path }
}

fn fixture_lexical_index() -> LexicalIndex {
    LexicalIndex::build(&fixture_chunks()).expect("build lexical index")
}

fn fixture_embedding_inventory_index() -> LexicalIndex {
    std::env::var_os("VESC_MCP_BENCH_LEXICAL_ARTIFACT").map_or_else(fixture_lexical_index, |path| {
        LexicalIndex::open_search_artifact(Path::new(&path))
            .expect("open VESC_MCP_BENCH_LEXICAL_ARTIFACT")
    })
}

struct GitFixture {
    _root: TempDir,
    repository: PathBuf,
    revision: Revision,
}

struct HistoryGitFixture {
    _root: TempDir,
    source: GitCorpusSource,
    previous_tips: Vec<GitHistoryTip>,
    cached: Vec<Chunk>,
}

struct PersistedRewriteFixture {
    _history: HistoryGitFixture,
    _previous_root: TempDir,
    output_root: TempDir,
    source: GitCorpusSource,
    previous: Option<PreviousGitHistoryArtifact>,
}

static HISTORY_FIXTURE_TEARDOWN: Mutex<Option<HistoryGitFixture>> = Mutex::new(None);
static PERSISTED_FIXTURE_TEARDOWN: Mutex<Option<PersistedRewriteFixture>> = Mutex::new(None);

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn fixture_git() -> GitFixture {
    let root = tempdir().expect("Git fixture root");
    let work = root.path().join("work");
    let repository = root.path().join("fixture.git");
    fs::create_dir(&work).expect("worktree");
    fs::create_dir(work.join("src")).expect("source directory");
    git(&work, &["init", "-q"]);
    git(
        &work,
        &["config", "user.email", "benchmark@example.invalid"],
    );
    git(&work, &["config", "user.name", "Benchmark"]);
    git(&work, &["config", "gc.auto", "0"]);
    git(&work, &["config", "maintenance.auto", "false"]);
    for index in 0..32 {
        fs::write(
            work.join(format!("src/motor_{index:02}.c")),
            format!(
                "static void motor_{index:02}_update(float current) {{\n\
                     observer_update(current);\n\
                 }}\n\
                 // motor control observer current voltage fault state\n"
            ),
        )
        .expect("fixture source");
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "fixture"]);
    let revision = git(&work, &["rev-parse", "HEAD"]);
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "-q",
            work.to_str().expect("UTF-8 worktree path"),
            repository.to_str().expect("UTF-8 repository path"),
        ],
    );
    GitFixture {
        _root: root,
        repository,
        revision: Revision::try_from(revision).expect("revision"),
    }
}

fn fixture_git_many_tips() -> HistoryGitFixture {
    const DIRECTORY_COUNT: usize = 16;
    const FILES_PER_DIRECTORY: usize = 8;
    const TIP_COUNT: usize = 64;

    let root = tempdir().expect("Git fixture root");
    let work = root.path().join("work");
    let repository = root.path().join("fixture.git");
    fs::create_dir(&work).expect("worktree");
    git(&work, &["init", "-q", "-b", "main"]);
    git(
        &work,
        &["config", "user.email", "benchmark@example.invalid"],
    );
    git(&work, &["config", "user.name", "Benchmark"]);
    git(&work, &["config", "gc.auto", "0"]);
    git(&work, &["config", "maintenance.auto", "false"]);
    for directory in 0..DIRECTORY_COUNT {
        let path = work.join(format!("src/dir_{directory:02}"));
        fs::create_dir_all(&path).expect("source directory");
        for file in 0..FILES_PER_DIRECTORY {
            fs::write(
                path.join(format!("motor_{file:02}.c")),
                format!("static int motor_{directory}_{file};\n"),
            )
            .expect("fixture source");
        }
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "base tree"]);
    let base = git(&work, &["rev-parse", "HEAD"]);
    let mut tips = vec![Revision::try_from(base.clone()).expect("base revision")];
    for index in 0..TIP_COUNT {
        let branch = format!("branch-{index:03}");
        git(&work, &["switch", "-q", "-c", &branch, &base]);
        let directory = index % DIRECTORY_COUNT;
        fs::write(
            work.join(format!("src/dir_{directory:02}/branch.c")),
            format!("static int branch_{index};\n"),
        )
        .expect("branch source");
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "branch tip"]);
        tips.push(Revision::try_from(git(&work, &["rev-parse", "HEAD"])).expect("branch revision"));
    }
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "-q",
            repository.to_str().expect("UTF-8 repository path"),
        ],
    );
    git(
        &work,
        &[
            "push",
            "-q",
            "--all",
            repository.to_str().expect("UTF-8 repository path"),
        ],
    );
    let policy = GitCorpusPolicy {
        limits: GitCorpusLimits::new(4_096, 1, 4_096).expect("benchmark limits"),
        ..GitCorpusPolicy::default()
    };
    let source = GitCorpusSource {
        repository_path: repository,
        repository_id: RepositoryId::try_from("many-tips").expect("repository ID"),
        revision: tips[0].clone(),
        history_tips: tips,
        trust_tier: TrustTier::Fixture,
        license: LicenseStatus::InRepo,
        policy,
    };
    let mut previous = source.clone();
    previous.history_tips.truncate(1);
    let cached = ingest_git_history_fast_forward(std::slice::from_ref(&previous), &[], &[])
        .expect("ingest predecessor fixture")
        .expect("cold predecessor is accepted")
        .0;
    let previous_tips = vec![GitHistoryTip {
        repository: previous.repository_id,
        revision: previous.revision,
    }];
    HistoryGitFixture {
        _root: root,
        source,
        previous_tips,
        cached,
    }
}

fn fixture_persisted_rewrite() -> PersistedRewriteFixture {
    let mut history = fixture_git_many_tips();
    let previous_source = history.source.clone();
    let previous_root = tempdir().expect("previous artifact root");
    let first = build_git_history_artifacts_incrementally(
        previous_root.path(),
        std::slice::from_ref(&previous_source),
        None,
        None,
        None,
        None,
        None,
        &mut |_| {},
    )
    .expect("build persisted predecessor");
    let previous_generation = previous_root
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    history.source.history_tips.truncate(1);
    history.source.revision = history.source.history_tips[0].clone();
    PersistedRewriteFixture {
        source: history.source.clone(),
        _history: history,
        _previous_root: previous_root,
        output_root: tempdir().expect("output artifact root"),
        previous: Some(PreviousGitHistoryArtifact {
            tips: previous_source
                .history_tips
                .into_iter()
                .map(|revision| GitHistoryTip {
                    repository: previous_source.repository_id.clone(),
                    revision,
                })
                .collect(),
            lexical_path: previous_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: None,
            vector_path: None,
            lexical_format_compatible: true,
        }),
    }
}

fn fixture_persisted_fast_forward() -> PersistedRewriteFixture {
    let history = fixture_git_many_tips();
    let current_source = history.source.clone();
    let mut previous_source = current_source.clone();
    previous_source.history_tips.truncate(1);
    previous_source.revision = previous_source.history_tips[0].clone();
    let previous_root = tempdir().expect("previous artifact root");
    let first = build_git_history_artifacts_incrementally(
        previous_root.path(),
        std::slice::from_ref(&previous_source),
        None,
        None,
        None,
        None,
        None,
        &mut |_| {},
    )
    .expect("build persisted predecessor");
    let previous_generation = previous_root
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let previous_repository = previous_source.repository_id.clone();
    PersistedRewriteFixture {
        _history: history,
        _previous_root: previous_root,
        output_root: tempdir().expect("output artifact root"),
        source: current_source,
        previous: Some(PreviousGitHistoryArtifact {
            tips: previous_source
                .history_tips
                .into_iter()
                .map(|revision| GitHistoryTip {
                    repository: previous_repository.clone(),
                    revision,
                })
                .collect(),
            lexical_path: previous_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: None,
            vector_path: None,
            lexical_format_compatible: true,
        }),
    }
}

fn fixture_embedding_projection() -> (PersistedRewriteFixture, EmbeddingProjectionFixture) {
    let fixture = fixture_persisted_rewrite();
    let previous = fixture.previous.as_ref().expect("persisted predecessor");
    let projection = prepare_embedding_projection(
        &previous.lexical_path,
        std::slice::from_ref(&fixture.source),
    )
    .expect("prepare embedding projection fixture");
    (fixture, projection)
}

#[library_benchmark(setup = fixture_document)]
fn bench_chunk_document(document: NormalizedDocument) -> Vec<Chunk> {
    let document = black_box(document);
    black_box(chunk_document(&document, ChunkingConfig::default()).expect("chunk fixture document"))
}

#[library_benchmark(setup = fixture_git)]
fn bench_ingest_git_commit(fixture: GitFixture) -> usize {
    let fixture = black_box(fixture);
    black_box(
        ingest_git_commit(
            &fixture.repository,
            &RepositoryId::try_from("benchmark").expect("repository ID"),
            &fixture.revision,
            TrustTier::Fixture,
            &LicenseStatus::InRepo,
            &GitCorpusPolicy::default(),
        )
        .expect("ingest fixture commit")
        .documents
        .len(),
    )
}

fn history_memory_benchmark_config() -> LibraryBenchmarkConfig {
    let mut dhat = Dhat::default();
    dhat.format([
        DhatMetric::TotalBytes,
        DhatMetric::TotalBlocks,
        DhatMetric::AtTGmaxBytes,
        DhatMetric::AtTGmaxBlocks,
    ]);
    let mut config = LibraryBenchmarkConfig::default();
    config.tool(dhat).tool(Massif::default());
    config
}

fn teardown_history_fixture(_: ()) {
    drop(
        HISTORY_FIXTURE_TEARDOWN
            .lock()
            .expect("history fixture teardown mutex")
            .take(),
    );
}

fn teardown_persisted_fixture(_: ()) {
    drop(
        PERSISTED_FIXTURE_TEARDOWN
            .lock()
            .expect("persisted fixture teardown mutex")
            .take(),
    );
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_git_many_tips,
    teardown = teardown_history_fixture
)]
fn bench_ingest_many_mostly_shared_tips(fixture: HistoryGitFixture) {
    let fixture = black_box(fixture);
    black_box(
        ingest_git_history_fast_forward(std::slice::from_ref(&fixture.source), &[], &[])
            .expect("ingest many-tip fixture")
            .expect("cold history is accepted")
            .0
            .len(),
    );
    *HISTORY_FIXTURE_TEARDOWN
        .lock()
        .expect("history fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_git_many_tips,
    teardown = teardown_history_fixture
)]
fn bench_incremental_many_divergent_tips(fixture: HistoryGitFixture) {
    let fixture = black_box(fixture);
    black_box(
        ingest_git_history_fast_forward(
            std::slice::from_ref(&fixture.source),
            &fixture.previous_tips,
            &fixture.cached,
        )
        .expect("ingest many-tip fixture")
        .expect("added tips are incremental")
        .0
        .len(),
    );
    *HISTORY_FIXTURE_TEARDOWN
        .lock()
        .expect("history fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_persisted_rewrite,
    teardown = teardown_persisted_fixture
)]
fn bench_persisted_reconcile_removed_tips(fixture: PersistedRewriteFixture) {
    let mut fixture = black_box(fixture);
    let previous = fixture.previous.take().expect("persisted predecessor");
    black_box(
        build_git_history_artifacts_from_previous(
            fixture.output_root.path(),
            std::slice::from_ref(&fixture.source),
            Some(previous),
            None,
            None,
            &mut |_| {},
        )
        .expect("persisted rewrite reconciliation")
        .artifacts
        .chunk_count,
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_persisted_fast_forward,
    teardown = teardown_persisted_fixture
)]
fn bench_persisted_reconcile_new_tips(fixture: PersistedRewriteFixture) {
    let mut fixture = black_box(fixture);
    let previous = fixture.previous.take().expect("persisted predecessor");
    black_box(
        build_git_history_artifacts_from_previous(
            fixture.output_root.path(),
            std::slice::from_ref(&fixture.source),
            Some(previous),
            None,
            None,
            &mut |_| {},
        )
        .expect("persisted fast-forward reconciliation")
        .artifacts
        .chunk_count,
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_embedding_projection,
    teardown = teardown_persisted_fixture
)]
fn bench_embedding_projection(
    (fixture, projection): (PersistedRewriteFixture, EmbeddingProjectionFixture),
) {
    black_box(
        benchmark_embedding_projection_from_fixture(&projection, true)
            .expect("projected embedding text reconstruction"),
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_embedding_projection,
    teardown = teardown_persisted_fixture
)]
fn bench_embedding_projection_legacy(
    (fixture, projection): (PersistedRewriteFixture, EmbeddingProjectionFixture),
) {
    black_box(
        benchmark_embedding_projection_from_fixture(&projection, false)
            .expect("legacy embedding text reconstruction"),
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_persisted_rewrite,
    teardown = teardown_persisted_fixture
)]
fn bench_persisted_reconcile_removed_tips_embedding(fixture: PersistedRewriteFixture) {
    let mut fixture = black_box(fixture);
    let previous = fixture.previous.take().expect("persisted predecessor");
    let mut provider = FakeEmbeddingProvider::new(64);
    black_box(
        build_git_history_artifacts_from_previous(
            fixture.output_root.path(),
            std::slice::from_ref(&fixture.source),
            Some(previous),
            Some((&mut provider, "benchmark-model", "v1")),
            None,
            &mut |_| {},
        )
        .expect("persisted embedding reconciliation")
        .artifacts
        .chunk_count,
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_persisted_rewrite,
    teardown = teardown_persisted_fixture
)]
fn bench_persisted_reconcile_removed_tips_embedding_legacy(mut fixture: PersistedRewriteFixture) {
    let previous = fixture.previous.as_ref().expect("persisted predecessor");
    let sidecar = previous.lexical_path.with_extension("embedding-input.json");
    fs::remove_file(sidecar).expect("remove embedding sidecar for legacy baseline");
    let previous = fixture.previous.take().expect("persisted predecessor");
    let mut provider = FakeEmbeddingProvider::new(64);
    black_box(
        build_git_history_artifacts_from_previous(
            fixture.output_root.path(),
            std::slice::from_ref(&fixture.source),
            Some(previous),
            Some((&mut provider, "benchmark-model", "v1")),
            None,
            &mut |_| {},
        )
        .expect("legacy persisted embedding reconciliation")
        .artifacts
        .chunk_count,
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(
    config = history_memory_benchmark_config(),
    setup = fixture_persisted_rewrite,
    teardown = teardown_persisted_fixture
)]
fn bench_persisted_cold_after_removed_tips(fixture: PersistedRewriteFixture) {
    let fixture = black_box(fixture);
    black_box(
        build_git_history_artifacts_incrementally(
            fixture.output_root.path(),
            std::slice::from_ref(&fixture.source),
            None,
            None,
            None,
            None,
            None,
            &mut |_| {},
        )
        .expect("persisted cold rebuild")
        .artifacts
        .chunk_count,
    );
    *PERSISTED_FIXTURE_TEARDOWN
        .lock()
        .expect("persisted fixture teardown mutex") = Some(fixture);
}

#[library_benchmark(setup = fixture_chunks)]
fn bench_lexical_build(chunks: Vec<Chunk>) -> LexicalIndex {
    let chunks = black_box(chunks);
    black_box(LexicalIndex::build(&chunks).expect("build lexical index"))
}

#[library_benchmark(setup = fixture_lexical_index)]
fn bench_lexical_search(index: LexicalIndex) -> (LexicalIndex, Vec<LexicalHit>) {
    let index = black_box(index);
    let hits = index
        .search(
            black_box("motor control observer"),
            &LexicalFilters::default(),
            10,
        )
        .expect("search lexical index");
    black_box((index, hits))
}

#[library_benchmark(setup = fixture_artifact)]
fn bench_corpus_inventory(fixture: ArtifactFixture) -> (usize, usize, ContentDigest) {
    let fixture = black_box(fixture);
    black_box(LexicalIndex::corpus_inventory(&fixture.path).expect("corpus inventory"))
}

#[library_benchmark(setup = fixture_embedding_inventory_index)]
fn bench_embedding_chunk_ids(index: LexicalIndex) -> (LexicalIndex, Vec<ChunkId>) {
    let index = black_box(index);
    let ids = embedding_chunk_ids(&index).expect("embedding chunk IDs");
    black_box((index, ids))
}

#[library_benchmark(setup = fixture_chunks)]
fn bench_fake_vector_build(chunks: Vec<Chunk>) -> VectorArtifact {
    let chunks = black_box(chunks);
    let mut provider = FakeEmbeddingProvider::new(64);
    black_box(
        VectorArtifact::from_provider(
            &mut provider,
            &chunks,
            "benchmark-model",
            "v1",
            ContentDigest::of(b"benchmark corpus"),
        )
        .expect("build vector artifact"),
    )
}
#[cfg(feature = "semantic-fastembed")]
fn fixture_bounded_document_windows() -> (tokenizers::Tokenizer, Vec<String>) {
    let tokenizer = tokenizers::Tokenizer::from_bytes(
        br#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0,"fn":1,"update":2,"motor":3,"controller":4,"unicode":5,"oversized":6},"unk_token":"[UNK]"}}"#,
    )
    .expect("benchmark tokenizer");
    let documents = vec![
        "# Markdown\n\nfn update_motor_controller() { motor += 1; }\n".repeat(64),
        "Unicode … naïve café Привет мир\n".repeat(128),
        "oversized_identifier ".repeat(2048),
    ];
    (tokenizer, documents)
}

#[cfg(feature = "semantic-fastembed")]
#[library_benchmark(setup = fixture_bounded_document_windows)]
fn bench_bounded_document_windows(
    (tokenizer, documents): (tokenizers::Tokenizer, Vec<String>),
) -> usize {
    let total = documents
        .iter()
        .map(|document| {
            bounded_document_windows(&tokenizer, black_box(document.as_str()), 32)
                .expect("bounded document windows")
                .iter()
                .map(|(_, token_count)| *token_count)
                .sum::<usize>()
        })
        .sum();
    black_box(total)
}

#[cfg(feature = "semantic-fastembed")]
library_benchmark_group!(
    name = corpus_pipeline,
    benchmarks = [
        bench_chunk_document,
        bench_ingest_git_commit,
        bench_ingest_many_mostly_shared_tips,
        bench_incremental_many_divergent_tips,
        bench_embedding_projection,
        bench_embedding_projection_legacy,
        bench_persisted_reconcile_removed_tips,
        bench_persisted_reconcile_new_tips,
        bench_persisted_reconcile_removed_tips_embedding,
        bench_persisted_reconcile_removed_tips_embedding_legacy,
        bench_persisted_cold_after_removed_tips,
        bench_lexical_build,
        bench_lexical_search,
        bench_corpus_inventory,
        bench_embedding_chunk_ids,
        bench_fake_vector_build,
        bench_bounded_document_windows,
    ]
);

#[cfg(not(feature = "semantic-fastembed"))]
library_benchmark_group!(
    name = corpus_pipeline,
    benchmarks = [
        bench_chunk_document,
        bench_ingest_git_commit,
        bench_ingest_many_mostly_shared_tips,
        bench_incremental_many_divergent_tips,
        bench_embedding_projection,
        bench_embedding_projection_legacy,
        bench_persisted_reconcile_removed_tips,
        bench_persisted_reconcile_new_tips,
        bench_persisted_reconcile_removed_tips_embedding,
        bench_persisted_reconcile_removed_tips_embedding_legacy,
        bench_persisted_cold_after_removed_tips,
        bench_lexical_build,
        bench_lexical_search,
        bench_corpus_inventory,
        bench_embedding_chunk_ids,
        bench_fake_vector_build,
    ]
);

main!(
    config = LibraryBenchmarkConfig::default()
        .pass_through_env("PATH")
        .valgrind_args(["--trace-children=no"]),
    library_benchmark_groups = corpus_pipeline
);
