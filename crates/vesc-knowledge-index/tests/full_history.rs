#![cfg(feature = "git-corpus")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::git::{GitCorpusPolicy, GitCorpusSource};
use vesc_knowledge_index::{
    ContentDigest, GitHistory, GitHistoryChangeKind, LicenseStatus, RepositoryId, Revision,
    TrustTier, ingest_git_history,
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
