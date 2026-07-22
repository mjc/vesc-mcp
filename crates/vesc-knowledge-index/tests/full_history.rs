use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::git::{GitCorpusPolicy, GitCorpusSource};
use vesc_knowledge_index::{
    Chunk, FakeEmbeddingProvider, GitHistoryRefreshObservations, GitHistoryTip, LicenseStatus,
    RepositoryId, Revision, TrustTier, VectorArtifact, build_git_history_artifacts_incrementally,
    ingest_git_history_fast_forward,
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

fn snapshot_tips(sources: &[GitCorpusSource]) -> Vec<GitHistoryTip> {
    let mut tips = sources
        .iter()
        .map(|source| GitHistoryTip {
            repository: source.repository_id.clone(),
            revision: source.revision.clone(),
        })
        .collect::<Vec<_>>();
    tips.sort_by(|left, right| left.repository.cmp(&right.repository));
    tips
}

fn cold_history(sources: &[GitCorpusSource]) -> (Vec<Chunk>, GitHistoryRefreshObservations) {
    ingest_git_history_fast_forward(sources, &[], &[])
        .expect("cold history")
        .expect("empty cache accepts every Git history")
}

#[test]
fn full_history_ingests_changed_blobs_once_and_noop_refresh_reuses_everything() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (contents, cold) = cold_history(std::slice::from_ref(&source));

    assert_eq!(cold.reachable_commits, 3);
    assert_eq!(cold.ingested_commits, 3);
    assert_eq!(cold.ingested_blobs, 3);
    assert_eq!(contents.len(), 3);

    let (reused, warm) = ingest_git_history_fast_forward(
        std::slice::from_ref(&source),
        &snapshot_tips(std::slice::from_ref(&source)),
        &contents,
    )
    .expect("warm history")
    .expect("unchanged tip is reusable");
    assert_eq!(reused, contents);
    assert_eq!(warm.reused_commits, 3);
    assert_eq!(warm.ingested_commits, 0);
    assert_eq!(warm.ingested_blobs, 0);
}

#[test]
fn binary_blobs_do_not_become_search_chunks() {
    let (_root, work) = fixture();
    fs::write(work.join("firmware.rs"), [0_u8, 1, 2, 3]).expect("binary fixture");
    git(&work, &["add", "firmware.rs"]);
    git(&work, &["commit", "-qm", "binary"]);
    let source = at_head(source(work.clone(), "fixture"), &work);

    let (contents, observations) = cold_history(&[source]);

    assert_eq!(observations.git.binary_rejection_count, 1);
    assert!(
        contents
            .iter()
            .all(|chunk| chunk.path.as_str() != "firmware.rs")
    );
}

#[test]
fn full_history_build_with_provider_writes_matching_vectors() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let mut provider = FakeEmbeddingProvider::new(8);

    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[source],
        None,
        None,
        Some((&mut provider, "fake", "test-revision")),
        None,
        None,
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
}

#[test]
fn changed_tip_reuses_existing_vectors_and_embeds_only_new_chunks() {
    let (_root, work) = fixture();
    let first_source = at_head(source(work.clone(), "fixture"), &work);
    let (first_contents, _) = cold_history(std::slice::from_ref(&first_source));
    let first_artifacts = tempdir().expect("first artifact root");
    let mut first_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_incrementally(
        first_artifacts.path(),
        std::slice::from_ref(&first_source),
        None,
        None,
        Some((&mut first_provider, "fake", "test-revision")),
        None,
        None,
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
    let second_source = at_head(source(work.clone(), "fixture"), &work);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut second_provider = FakeEmbeddingProvider::new(8);
    let second = build_git_history_artifacts_incrementally(
        second_artifacts.path(),
        &[second_source],
        Some(snapshot_tips(&[first_source])),
        Some(first_contents),
        Some((&mut second_provider, "fake", "test-revision")),
        Some(first_vector),
        None,
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
    let (first, _) = cold_history(std::slice::from_ref(&first_source));

    fs::write(work.join("incremental.md"), "incremental only\n").expect("new passage");
    git(&work, &["add", "incremental.md"]);
    git(&work, &["commit", "-qm", "incremental passage"]);
    let next_source = at_head(source(work.clone(), "fixture"), &work);
    let incremental = ingest_git_history_fast_forward(
        std::slice::from_ref(&next_source),
        &snapshot_tips(&[first_source]),
        &first,
    )
    .expect("incremental history")
    .expect("fast-forward");
    let (cold, _) = cold_history(&[next_source]);

    assert_eq!(incremental.1.ingested_commits, 1);
    assert_eq!(incremental.0, cold);
}

#[test]
fn fast_forward_reuses_existing_repository_when_one_is_added() {
    let (_first_root, first_work) = fixture();
    let (_second_root, second_work) = fixture();
    let first_source = at_head(source(first_work.clone(), "first"), &first_work);
    let (first, _) = cold_history(std::slice::from_ref(&first_source));
    let second_source = at_head(source(second_work.clone(), "second"), &second_work);

    let incremental = ingest_git_history_fast_forward(
        &[first_source.clone(), second_source.clone()],
        &snapshot_tips(std::slice::from_ref(&first_source)),
        &first,
    )
    .expect("incremental history")
    .expect("repository addition remains incremental");
    let (cold, _) = cold_history(&[first_source, second_source]);

    assert_eq!(incremental.1.reachable_commits, 6);
    assert!(incremental.1.reused_commits >= 3);
    assert_eq!(
        incremental.1.reused_commits + incremental.1.ingested_commits,
        incremental.1.reachable_commits
    );
    assert_eq!(incremental.0, cold);
}

#[test]
fn added_checkout_reuses_history_known_under_another_repository_id() {
    let (_root, work) = fixture();
    let managed = at_head(source(work.clone(), "managed"), &work);
    let (first, _) = cold_history(std::slice::from_ref(&managed));
    let checkout = at_head(source(work.clone(), "checkout"), &work);

    let incremental = ingest_git_history_fast_forward(
        &[managed.clone(), checkout],
        &snapshot_tips(&[managed]),
        &first,
    )
    .expect("incremental history")
    .expect("shared Git lineage is recognized");

    assert_eq!(incremental.1.ingested_commits, 0);
    assert_eq!(incremental.0.len(), first.len());
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

    let (history, _) = cold_history(&[source]);
    let alpha = history
        .iter()
        .find(|chunk| chunk.text.contains("alpha_unique"))
        .expect("alpha passage");
    let omega = history
        .iter()
        .find(|chunk| chunk.text.contains("omega_unique"))
        .expect("omega passage");

    assert!(alpha.identifiers.contains("alpha_unique"));
    assert!(!alpha.identifiers.contains("omega_unique"));
    assert!(omega.identifiers.contains("omega_unique"));
    assert!(!omega.identifiers.contains("alpha_unique"));
}

#[test]
fn source_order_is_deterministic_and_shared_history_is_not_duplicated() {
    let (_root, work) = fixture();
    let first = at_head(source(work.clone(), "alpha"), &work);
    let second = at_head(source(work.clone(), "beta"), &work);
    let third = at_head(source(work.clone(), "gamma"), &work);
    let (forward, forward_observations) =
        cold_history(&[first.clone(), second.clone(), third.clone()]);
    let (reverse, reverse_observations) = cold_history(&[third, second, first]);

    assert_eq!(forward, reverse);
    assert_eq!(forward_observations.ingested_commits, 3);
    assert_eq!(forward_observations.reused_commits, 6);
    assert_eq!(
        (
            forward_observations.reachable_commits,
            forward_observations.reused_commits,
            forward_observations.ingested_commits,
            forward_observations.ingested_blobs,
        ),
        (
            reverse_observations.reachable_commits,
            reverse_observations.reused_commits,
            reverse_observations.ingested_commits,
            reverse_observations.ingested_blobs,
        )
    );
    assert_eq!(forward.len(), 3);
}

#[test]
fn rewritten_history_rejects_the_cache_and_rebuilds_from_git() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (before, _) = cold_history(std::slice::from_ref(&source));

    git(&work, &["checkout", "-q", "--orphan", "rewritten"]);
    git(&work, &["rm", "-q", "-r", "-f", "."]);
    fs::create_dir_all(work.join("src")).expect("rewritten source directory");
    fs::write(work.join("src/lib.rs"), "pub fn second() -> u8 { 2 }\n").expect("same blob");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "rewritten root"]);
    let rewritten = at_head(source.clone(), &work);

    let incremental = ingest_git_history_fast_forward(
        std::slice::from_ref(&rewritten),
        &snapshot_tips(&[source]),
        &before,
    )
    .expect("rewritten history check");
    let (cold, refresh) = cold_history(&[rewritten]);

    assert!(incremental.is_none());
    assert_eq!(refresh.reachable_commits, 1);
    assert_eq!(refresh.ingested_commits, 1);
    assert_eq!(cold.len(), 1);
}
