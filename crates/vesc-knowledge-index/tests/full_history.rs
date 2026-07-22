#![cfg(feature = "git-corpus")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::git::{GitCorpusPolicy, GitCorpusSource};
use vesc_knowledge_index::{
    ContentDigest, FakeEmbeddingProvider, GitHistory, GitHistoryChangeKind, LicenseStatus,
    RepositoryId, Revision, TrustTier, VectorArtifact, build_git_history_artifacts_incrementally,
    build_git_history_artifacts_with_provider, ingest_git_history, ingest_git_history_fast_forward,
};

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output")
        .trim()
        .to_owned()
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempdir().expect("fixture root");
    let work = root.path().join("work");
    fs::create_dir(&work).expect("worktree");
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);

    fs::create_dir(work.join("src")).expect("source directory");
    fs::write(work.join("src/lib.rs"), "pub fn first() -> u8 { 1 }\n").expect("v1");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "first"]);
    git(&work, &["tag", "v1"]);

    fs::write(work.join("src/lib.rs"), "pub fn second() -> u8 { 2 }\n").expect("v2");
    git(&work, &["commit", "-qam", "second"]);

    fs::write(work.join("README.md"), "# Fixture\n\nThird version.\n").expect("v3");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "third"]);
    (root, work)
}

fn source(path: PathBuf, repository: &str) -> GitCorpusSource {
    GitCorpusSource {
        repository_path: path,
        repository_id: RepositoryId::try_from(repository).expect("repository"),
        revision: Revision::try_from("0".repeat(40)).expect("placeholder revision"),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    }
}

fn at_head(mut source: GitCorpusSource, work: &Path) -> GitCorpusSource {
    source.revision = Revision::try_from(git(work, &["rev-parse", "HEAD"])).expect("revision");
    source
}

#[test]
fn full_history_ingests_changed_blobs_once_and_noop_refresh_reuses_everything() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (history, cold) =
        ingest_git_history(std::slice::from_ref(&source), None).expect("cold history");

    assert_eq!(cold.reachable_commits, 3);
    assert_eq!(cold.ingested_commits, 3);
    assert_eq!(cold.ingested_blobs, 3);
    assert_eq!(history.commits.len(), 3);
    assert_eq!(history.contents.len(), 3);
    assert!(
        history
            .refs
            .iter()
            .any(|reference| reference.name == "refs/tags/v1")
    );
    assert!(history.occurrences.iter().any(|occurrence| {
        occurrence.path == "src/lib.rs" && occurrence.kind == GitHistoryChangeKind::Modified
    }));

    let (reused, warm) = ingest_git_history(&[source], Some(&history)).expect("warm history");
    assert_eq!(reused, history);
    assert_eq!(warm.reused_commits, 3);
    assert_eq!(warm.ingested_commits, 0);
    assert_eq!(warm.ingested_blobs, 0);

    let artifact = work.join("history.json");
    history.write_artifact(&artifact).expect("write history");
    assert_eq!(
        GitHistory::read_artifact(&artifact).expect("read history"),
        history
    );
    let mut corrupt = history;
    corrupt.occurrences[0].content_keys[0] = ContentDigest::of(b"not stored content");
    fs::write(
        &artifact,
        serde_json::to_vec(&corrupt).expect("serialize corrupt history"),
    )
    .expect("write corrupt history");
    assert!(GitHistory::read_artifact(&artifact).is_err());
}

#[test]
fn binary_blobs_remain_in_history_without_becoming_search_chunks() {
    let (_root, work) = fixture();
    fs::write(work.join("firmware.rs"), [0_u8, 1, 2, 3]).expect("binary fixture");
    git(&work, &["add", "firmware.rs"]);
    git(&work, &["commit", "-qm", "binary"]);
    let source = at_head(source(work.clone(), "fixture"), &work);

    let (history, observations) =
        ingest_git_history(&[source], None).expect("history with binary blob");

    assert_eq!(observations.git.binary_rejection_count, 1);
    assert!(history.occurrences.iter().any(|occurrence| {
        occurrence.path == "firmware.rs" && occurrence.content_keys.is_empty()
    }));
    assert!(
        history
            .contents
            .iter()
            .all(|content| content.chunk.path.as_str() != "firmware.rs")
    );
}

#[test]
fn full_history_build_with_provider_writes_matching_vectors() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let mut provider = FakeEmbeddingProvider::new(8);

    let summary = build_git_history_artifacts_with_provider(
        artifacts.path(),
        &[source],
        None,
        Some((&mut provider, "fake", "test-revision")),
    )
    .expect("semantic history build");

    let vector = VectorArtifact::open_artifact(
        &artifacts
            .path()
            .join("generations")
            .join(&summary.artifacts.generation)
            .join("vectors.bin"),
    )
    .expect("vector artifact");
    assert_eq!(vector.model_id, "fake");
    assert_eq!(vector.model_revision, "test-revision");
    assert_eq!(vector.ids.len(), summary.artifacts.chunk_count);
    assert!(!artifacts.path().join("history.json").exists());
}

#[test]
fn changed_tip_reuses_existing_vectors_and_embeds_only_new_chunks() {
    let (_root, work) = fixture();
    let first_artifacts = tempdir().expect("first artifact root");
    let mut first_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_with_provider(
        first_artifacts.path(),
        &[at_head(source(work.clone(), "fixture"), &work)],
        None,
        Some((&mut first_provider, "fake", "test-revision")),
    )
    .expect("first semantic history build");
    let first_vector = VectorArtifact::open_artifact(
        &first_artifacts
            .path()
            .join("generations")
            .join(&first.artifacts.generation)
            .join("vectors.bin"),
    )
    .expect("first vector artifact");

    fs::write(work.join("new.md"), "a new semantic passage\n").expect("new passage");
    git(&work, &["add", "new.md"]);
    git(&work, &["commit", "-qm", "new passage"]);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut second_provider = FakeEmbeddingProvider::new(8);
    let cached_chunks = first
        .history
        .contents
        .iter()
        .map(|content| content.chunk.clone())
        .collect::<Vec<_>>();
    let second = build_git_history_artifacts_incrementally(
        second_artifacts.path(),
        &[at_head(source(work.clone(), "fixture"), &work)],
        Some(first.history.tips.clone()),
        Some(cached_chunks),
        Some((&mut second_provider, "fake", "test-revision")),
        Some(first_vector),
    )
    .expect("incremental semantic history build");
    let observations = second
        .artifacts
        .observations
        .vector_build
        .expect("vector observations");

    assert!(second.reused_snapshot);
    assert_eq!(second.refresh.ingested_commits, 1);
    assert_eq!(observations.reused_vectors, first.artifacts.chunk_count);
    assert_eq!(
        observations.embedded_vectors,
        second.artifacts.chunk_count - first.artifacts.chunk_count
    );
}

#[test]
fn fast_forward_uses_cached_chunks_and_ingests_only_new_commits() {
    let (_root, work) = fixture();
    let first_source = at_head(source(work.clone(), "fixture"), &work);
    let (first, _) = ingest_git_history(&[first_source], None).expect("first history");
    let cached_chunks = first
        .contents
        .iter()
        .map(|content| content.chunk.clone())
        .collect::<Vec<_>>();

    fs::write(work.join("incremental.md"), "incremental only\n").expect("new passage");
    git(&work, &["add", "incremental.md"]);
    git(&work, &["commit", "-qm", "incremental passage"]);
    let next_source = at_head(source(work.clone(), "fixture"), &work);
    let incremental = ingest_git_history_fast_forward(
        std::slice::from_ref(&next_source),
        &first.tips,
        &cached_chunks,
    )
    .expect("incremental history")
    .expect("fast-forward");
    let (cold, _) = ingest_git_history(&[next_source], None).expect("cold history");
    let incremental_ids = incremental
        .0
        .iter()
        .map(|content| content.chunk.chunk_id.clone())
        .collect::<Vec<_>>();
    let cold_ids = cold
        .contents
        .iter()
        .map(|content| content.chunk.chunk_id.clone())
        .collect::<Vec<_>>();

    assert_eq!(incremental.1.ingested_commits, 1);
    assert_eq!(incremental_ids, cold_ids);
}

#[test]
fn historical_chunks_keep_only_passage_local_identifiers() {
    let (_root, work) = fixture();
    let content = format!(
        "pub fn alpha_unique() {{}}\n{}pub fn omega_unique() {{}}\n",
        "// padding\n".repeat(2_000)
    );
    fs::write(work.join("src/large.rs"), content).expect("large source");
    git(&work, &["add", "src/large.rs"]);
    git(&work, &["commit", "-qm", "large source"]);
    let source = at_head(source(work.clone(), "fixture"), &work);

    let (history, _) = ingest_git_history(&[source], None).expect("history");
    let alpha = history
        .contents
        .iter()
        .map(|content| &content.chunk)
        .find(|chunk| chunk.text.contains("alpha_unique"))
        .expect("alpha passage");
    let omega = history
        .contents
        .iter()
        .map(|content| &content.chunk)
        .find(|chunk| chunk.text.contains("omega_unique"))
        .expect("omega passage");

    assert!(alpha.identifiers.contains("alpha_unique"));
    assert!(!alpha.identifiers.contains("omega_unique"));
    assert!(omega.identifiers.contains("omega_unique"));
    assert!(!omega.identifiers.contains("alpha_unique"));
}

#[test]
fn fast_forward_ingests_only_the_new_commit_and_matches_a_cold_rebuild() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (before, _) =
        ingest_git_history(std::slice::from_ref(&source), None).expect("initial history");

    fs::write(work.join("README.md"), "# Fixture\n\nFourth version.\n").expect("v4");
    git(&work, &["commit", "-qam", "fourth"]);
    let advanced = at_head(source, &work);
    let (incremental, refresh) = ingest_git_history(std::slice::from_ref(&advanced), Some(&before))
        .expect("incremental history");
    let (cold, _) = ingest_git_history(&[advanced], None).expect("cold rebuild");

    assert_eq!(refresh.reachable_commits, 4);
    assert_eq!(refresh.reused_commits, 3);
    assert_eq!(refresh.ingested_commits, 1);
    assert_eq!(refresh.ingested_blobs, 1);
    assert_eq!(incremental, cold);
}

#[test]
fn source_order_does_not_change_one_combined_history_set() {
    let (_root, work) = fixture();
    let first = at_head(source(work.clone(), "alpha"), &work);
    let second = at_head(source(work.clone(), "beta"), &work);
    let third = at_head(source(work.clone(), "gamma"), &work);
    let (forward, _) =
        ingest_git_history(&[first.clone(), second.clone(), third.clone()], None).expect("forward");
    let (reverse, _) = ingest_git_history(&[third, second, first], None).expect("reverse");

    assert_eq!(forward, reverse);
    assert_eq!(forward.tips.len(), 3);
    assert_eq!(forward.commits.len(), 9);
    assert_eq!(forward.contents.len(), 9);
    assert_eq!(
        forward
            .contents
            .iter()
            .map(|content| &content.embedding_key)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
}

#[test]
fn rewritten_history_drops_unreachable_commits_and_matches_a_cold_rebuild() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (before, _) =
        ingest_git_history(std::slice::from_ref(&source), None).expect("initial history");

    git(&work, &["checkout", "-q", "--orphan", "rewritten"]);
    git(&work, &["rm", "-q", "-r", "-f", "."]);
    fs::create_dir_all(work.join("src")).expect("rewritten source directory");
    fs::write(work.join("src/lib.rs"), "pub fn second() -> u8 { 2 }\n").expect("same blob");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "rewritten root"]);
    let rewritten = at_head(source, &work);

    let (incremental, refresh) =
        ingest_git_history(std::slice::from_ref(&rewritten), Some(&before))
            .expect("rewritten incremental history");
    let (cold, _) = ingest_git_history(&[rewritten], None).expect("rewritten cold history");

    assert_eq!(refresh.reachable_commits, 1);
    assert_eq!(refresh.ingested_commits, 1);
    assert_eq!(incremental, cold);
    incremental.validate().expect("valid rewritten history");
}
