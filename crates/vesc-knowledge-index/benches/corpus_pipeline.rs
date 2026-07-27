use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;

use gungraun::prelude::*;
use tempfile::{TempDir, tempdir};
use vesc_knowledge_index::corpus::git::{GitCorpusPolicy, ingest_git_commit};
use vesc_knowledge_index::{
    Chunk, ChunkingConfig, ContentDigest, FakeEmbeddingProvider, LexicalFilters, LexicalHit,
    LexicalIndex, LicenseStatus, NormalizedDocument, RepositoryId, Revision, SourceKind, TrustTier,
    VectorArtifact, chunk_document,
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

struct GitFixture {
    _root: TempDir,
    repository: PathBuf,
    revision: Revision,
}

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

library_benchmark_group!(
    name = corpus_pipeline,
    benchmarks = [
        bench_chunk_document,
        bench_ingest_git_commit,
        bench_lexical_build,
        bench_lexical_search,
        bench_corpus_inventory,
        bench_fake_vector_build,
    ]
);

main!(
    config = LibraryBenchmarkConfig::default()
        .pass_through_env("PATH")
        .valgrind_args(["--trace-children=no"]),
    library_benchmark_groups = corpus_pipeline
);
