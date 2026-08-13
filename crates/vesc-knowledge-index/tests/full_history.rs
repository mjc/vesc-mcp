use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::git::{GitCorpusLimits, GitCorpusPolicy, GitCorpusSource};
use vesc_knowledge_index::{
    BuildPhase, Chunk, ContentDigest, EmbeddingBatchSize, EmbeddingError, EmbeddingProvider,
    FakeEmbeddingProvider, FusionConfig, GitHistoryRefreshObservations, GitHistoryTip,
    GraphArtifact, LexicalFilters, LexicalIndex, LicenseStatus, OutputNormalization,
    PreviousGitHistoryArtifact, PreviousVectorArtifact, RepositoryId, Revision, SourceKind,
    TrustTier, VectorArtifact, build_git_artifacts_with_provider,
    build_git_history_artifacts_from_previous, build_git_history_artifacts_incrementally,
    fuse_candidate_metadata, ingest_git_history_fast_forward,
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

fn add_tokenized_path_history(work: &Path) {
    fs::write(
        work.join("src/full_history.rs"),
        "pub fn underscored_path() {}\n",
    )
    .expect("underscored path");
    fs::write(work.join("foo-bar.md"), "hyphenated path\n").expect("hyphenated path");
    git(work, &["add", "."]);
    git(work, &["commit", "-qm", "tokenized paths"]);
}

fn assert_cold_equivalent_lexical(incremental: &Path, cold: &Path) {
    assert_eq!(
        LexicalIndex::corpus_inventory(incremental).expect("incremental inventory"),
        LexicalIndex::corpus_inventory(cold).expect("cold inventory"),
    );
}

fn source(path: PathBuf, repository: &str) -> GitCorpusSource {
    GitCorpusSource {
        repository_path: path,
        repository_id: RepositoryId::try_from(repository).expect("repository"),
        revision: Revision::try_from("0".repeat(40)).expect("placeholder revision"),
        history_tips: vec![Revision::try_from("0".repeat(40)).expect("placeholder history tip")],
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    }
}

fn at_head(mut source: GitCorpusSource, work: &Path) -> GitCorpusSource {
    source.revision = Revision::try_from(git(work, &["rev-parse", "HEAD"])).expect("revision");
    source.history_tips = vec![source.revision.clone()];
    source
}

fn snapshot_tips(sources: &[GitCorpusSource]) -> Vec<GitHistoryTip> {
    let mut tips = sources
        .iter()
        .flat_map(|source| {
            source.history_tips.iter().map(|revision| GitHistoryTip {
                repository: source.repository_id.clone(),
                revision: revision.clone(),
            })
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

fn bounded_source(work: &Path) -> GitCorpusSource {
    let mut source = at_head(source(work.to_owned(), "fixture"), work);
    source.policy.include_prefixes = vec!["limits".into()];
    source.policy.limits = GitCorpusLimits::new(16, 2, 16).expect("valid test limits");
    source
}

fn add_bounded_delta(work: &Path) {
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/a-oversized.rs"), "x".repeat(17)).expect("oversized source");
    fs::write(work.join("limits/b-first.rs"), "pub fn b(){}\n").expect("first source");
    fs::write(work.join("limits/c-total.rs"), "pub fn c(){}\n").expect("total-bound source");
    fs::write(work.join("limits/d-second.rs"), "d\n").expect("second source");
    fs::write(work.join("limits/e-file.rs"), "e\n").expect("file-bound source");
    git(work, &["add", "limits"]);
    git(work, &["commit", "-qm", "bounded delta"]);
}

fn assert_bounded_delta(contents: &[Chunk], observations: &GitHistoryRefreshObservations) {
    let mut paths = contents
        .iter()
        .filter(|chunk| chunk.source_kind == SourceKind::GitBlob)
        .map(|chunk| chunk.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    assert_eq!(paths, ["limits/b-first.rs", "limits/d-second.rs"]);
    assert_eq!(observations.ingested_blobs, 2);
    assert_eq!(observations.budget_rejections, 3);
    assert_eq!(
        observations.git.blob_bytes_loaded, 15,
        "oversized and over-budget blobs must be rejected from header metadata before content load"
    );
}

const fn ignore_build_phase(_: BuildPhase) {}

fn previous_vector_artifact(
    generation: &Path,
    corpus_digest: ContentDigest,
    checksum: ContentDigest,
) -> PreviousVectorArtifact {
    PreviousVectorArtifact::open(
        generation.join("lexical.json"),
        corpus_digest,
        checksum,
        generation.join("vectors.bin"),
        "fake",
        "test-revision",
        None,
    )
    .expect("validated previous vector artifact")
}

struct FailingEmbeddingProvider;

impl EmbeddingProvider for FailingEmbeddingProvider {
    fn embedding_dimension(&self) -> Option<usize> {
        Some(8)
    }

    fn embed_documents(&mut self, _texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Err(EmbeddingError::Provider("expected semantic failure".into()))
    }

    fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        unreachable!("artifact builds do not embed queries")
    }
}

struct FailAfterOneBatch {
    calls: usize,
    provider: FakeEmbeddingProvider,
}

impl Default for FailAfterOneBatch {
    fn default() -> Self {
        Self {
            calls: 0,
            provider: FakeEmbeddingProvider::new(8),
        }
    }
}

impl EmbeddingProvider for FailAfterOneBatch {
    fn embedding_dimension(&self) -> Option<usize> {
        Some(8)
    }

    fn output_normalization(&self) -> vesc_knowledge_index::OutputNormalization {
        vesc_knowledge_index::OutputNormalization::Guaranteed
    }

    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        self.calls += 1;
        if self.calls > 1 {
            return Err(EmbeddingError::Provider(
                "expected semantic failure after one batch".into(),
            ));
        }
        self.provider.embed_documents(texts)
    }

    fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        unreachable!("artifact builds do not embed queries")
    }
}

struct SingleItemEmbeddingProvider {
    provider: FakeEmbeddingProvider,
    embedded_documents: usize,
}

impl SingleItemEmbeddingProvider {
    const fn new(dimension: usize) -> Self {
        Self {
            provider: FakeEmbeddingProvider::new(dimension),
            embedded_documents: 0,
        }
    }
}

impl EmbeddingProvider for SingleItemEmbeddingProvider {
    fn embedding_dimension(&self) -> Option<usize> {
        self.provider.embedding_dimension()
    }

    fn embedding_batch_size(&self) -> EmbeddingBatchSize {
        EmbeddingBatchSize::new(1).expect("one is a valid batch size")
    }

    fn output_normalization(&self) -> OutputNormalization {
        self.provider.output_normalization()
    }

    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        self.embedded_documents = self.embedded_documents.saturating_add(texts.len());
        self.provider.embed_documents(texts)
    }

    fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        unreachable!("artifact builds do not embed queries")
    }
}

#[test]
fn full_history_ingests_changed_blobs_once_and_noop_refresh_reuses_everything() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (contents, cold) = cold_history(std::slice::from_ref(&source));

    assert_eq!(cold.reachable_commits, 3);
    assert_eq!(cold.ingested_commits, 3);
    assert_eq!(cold.ingested_blobs, 3);
    assert_eq!(cold.ingested_commit_messages, 3);
    assert_eq!(contents.len(), 7);
    assert!(
        contents
            .iter()
            .any(|chunk| chunk.text.contains("pub fn first()"))
    );
    assert!(
        contents
            .iter()
            .any(|chunk| chunk.text.contains("pub fn second()"))
    );

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
    assert_eq!(warm.reused_commit_messages, 3);
}

#[test]
fn full_history_walks_divergent_tips_once_and_keeps_conflicting_paths() {
    let (_root, work) = fixture();
    let main = git(&work, &["rev-parse", "main"]);
    git(&work, &["switch", "-q", "-c", "release/7", "v1"]);
    fs::write(
        work.join("src/lib.rs"),
        "pub fn release_seven_only() -> u8 { 7 }\n",
    )
    .expect("release source");
    git(&work, &["commit", "-qam", "release seven divergent fix"]);
    let release = git(&work, &["rev-parse", "HEAD"]);
    let mut source = source(work, "fixture");
    source.revision = Revision::try_from(main.clone()).expect("main revision");
    source.history_tips = vec![
        Revision::try_from(main).expect("main history tip"),
        Revision::try_from(release).expect("release history tip"),
    ];

    let (chunks, observations) = cold_history(std::slice::from_ref(&source));

    assert_eq!(observations.reachable_commits, 4);
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk.text.contains("pub fn second()"))
    );
    assert!(
        chunks
            .iter()
            .any(|chunk| chunk.text.contains("release_seven_only"))
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.text.contains("release seven divergent fix"))
            .count(),
        1
    );
}

#[test]
fn multi_tip_refresh_ingests_added_history_and_removes_deleted_history() {
    let (_root, work) = fixture();
    let main = git(&work, &["rev-parse", "main"]);
    let mut previous = source(work.clone(), "fixture");
    previous.revision = Revision::try_from(main.clone()).expect("main revision");
    previous.history_tips = vec![Revision::try_from(main).expect("main history tip")];
    let (cached, _) = cold_history(std::slice::from_ref(&previous));
    let previous_tips = snapshot_tips(std::slice::from_ref(&previous));

    git(&work, &["switch", "-q", "-c", "release/7", "v1"]);
    fs::write(work.join("release.md"), "release branch evidence\n").expect("release source");
    git(&work, &["add", "release.md"]);
    git(&work, &["commit", "-qm", "add divergent release evidence"]);
    let release = git(&work, &["rev-parse", "HEAD"]);
    let mut current = previous.clone();
    current
        .history_tips
        .push(Revision::try_from(release).expect("release history tip"));

    let (incremental, refresh) =
        ingest_git_history_fast_forward(std::slice::from_ref(&current), &previous_tips, &cached)
            .expect("multi-tip refresh")
            .expect("added ref is incremental");
    let (cold, _) = cold_history(std::slice::from_ref(&current));
    assert_eq!(refresh.ingested_commit_messages, 1, "{refresh:#?}");
    assert_eq!(refresh.ingested_blobs, 1, "{refresh:#?}");
    assert_eq!(incremental, cold);
    assert!(
        incremental
            .iter()
            .any(|chunk| chunk.text.contains("release branch evidence"))
    );

    let current_tips = snapshot_tips(std::slice::from_ref(&current));
    let (removed, removed_refresh) = ingest_git_history_fast_forward(
        std::slice::from_ref(&previous),
        &current_tips,
        &incremental,
    )
    .expect("removed ref refresh")
    .expect("removed history is reconciled incrementally");
    let (rebuilt, _) = cold_history(&[previous]);
    assert_eq!(removed, rebuilt);
    assert!(removed_refresh.reused_commits > 0, "{removed_refresh:#?}");
    assert!(
        removed_refresh.removed_documents > 0,
        "{removed_refresh:#?}"
    );
    assert!(
        removed_refresh.removed_commit_messages > 0,
        "{removed_refresh:#?}"
    );
    assert!(
        removed
            .iter()
            .all(|chunk| !chunk.text.contains("release branch evidence"))
    );
}

#[test]
fn multi_tip_refresh_reserves_the_new_divergent_tip_under_a_tight_budget() {
    let (_root, work) = fixture();
    let main = git(&work, &["rev-parse", "main"]);
    let mut previous = source(work.clone(), "fixture");
    previous.revision = Revision::try_from(main.clone()).expect("main revision");
    previous.history_tips = vec![Revision::try_from(main).expect("main history tip")];
    previous.policy.include_prefixes = vec!["limits".into()];
    previous.policy.limits = GitCorpusLimits::new(64, 1, 64).expect("valid test limits");
    let (cached, _) = cold_history(std::slice::from_ref(&previous));
    let previous_tips = snapshot_tips(std::slice::from_ref(&previous));

    git(&work, &["switch", "-q", "-c", "release/7", "v1"]);
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/release.rs"), "pub fn old_release() {}\n")
        .expect("old release source");
    git(&work, &["add", "limits/release.rs"]);
    git(&work, &["commit", "-qm", "old release source"]);
    fs::write(
        work.join("limits/release.rs"),
        "pub fn current_release() {}\n",
    )
    .expect("current release source");
    git(&work, &["commit", "-qam", "current release source"]);
    let release = git(&work, &["rev-parse", "HEAD"]);

    let mut current = previous;
    current
        .history_tips
        .push(Revision::try_from(release).expect("new divergent history tip"));
    let (contents, observations) =
        ingest_git_history_fast_forward(std::slice::from_ref(&current), &previous_tips, &cached)
            .expect("multi-tip refresh")
            .expect("added ref is incremental");
    let blobs = contents
        .iter()
        .filter(|chunk| chunk.source_kind == SourceKind::GitBlob)
        .collect::<Vec<_>>();

    assert_eq!(blobs.len(), 1, "the one-file budget admits one tip blob");
    assert!(blobs[0].text.contains("current_release"));
    assert!(!blobs[0].text.contains("old_release"));
    assert_eq!(observations.ingested_blobs, 1);
    assert_eq!(observations.budget_rejections, 1);
}

#[test]
fn divergent_tip_reservation_diffs_from_the_merge_base() {
    let (_root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/a-main.rs"), "pub fn main_only() {}\n").expect("main source");
    fs::write(
        work.join("limits/z-retained.rs"),
        "pub fn retained_on_release() {}\n",
    )
    .expect("shared source");
    git(&work, &["add", "limits"]);
    git(&work, &["commit", "-qm", "shared branch point"]);
    let branch_point = git(&work, &["rev-parse", "HEAD"]);

    fs::remove_file(work.join("limits/z-retained.rs")).expect("remove source from main");
    git(&work, &["commit", "-qam", "main removes release source"]);
    let main = git(&work, &["rev-parse", "HEAD"]);
    let mut previous = source(work.clone(), "fixture");
    previous.revision = Revision::try_from(main.clone()).expect("main revision");
    previous.history_tips = vec![Revision::try_from(main).expect("main history tip")];
    previous.policy.include_prefixes = vec!["limits".into()];
    previous.policy.limits = GitCorpusLimits::new(64, 1, 64).expect("valid test limits");
    let (cached, _) = cold_history(std::slice::from_ref(&previous));
    let previous_tips = snapshot_tips(std::slice::from_ref(&previous));

    git(&work, &["switch", "-q", "-c", "release/7", &branch_point]);
    fs::write(
        work.join("limits/zz-new.rs"),
        "pub fn genuinely_new_release_delta() {}\n",
    )
    .expect("new release source");
    git(&work, &["add", "limits/zz-new.rs"]);
    git(&work, &["commit", "-qm", "new release delta"]);
    let release = git(&work, &["rev-parse", "HEAD"]);
    let mut current = previous;
    current
        .history_tips
        .push(Revision::try_from(release).expect("release history tip"));

    let (contents, _) =
        ingest_git_history_fast_forward(std::slice::from_ref(&current), &previous_tips, &cached)
            .expect("multi-tip refresh")
            .expect("added ref is incremental");

    assert!(
        contents
            .iter()
            .any(|chunk| chunk.text.contains("genuinely_new_release_delta")),
        "the file budget must be reserved for the change since the branch point"
    );
}

#[test]
fn full_history_never_omits_c_or_cpp_from_a_narrow_repository_allowlist() {
    let (_root, work) = fixture();
    let mut previous_source = at_head(source(work.clone(), "fixture"), &work);
    previous_source.policy.include_patterns = vec!["**/*.md".into()];
    let (cached, _) = cold_history(std::slice::from_ref(&previous_source));
    let previous_tips = snapshot_tips(std::slice::from_ref(&previous_source));

    fs::write(
        work.join("src/native.c"),
        "int native(void) { return 1; }\n",
    )
    .expect("C source");
    fs::write(work.join("src/native.hpp"), "int native_cpp();\n").expect("C++ header");
    git(&work, &["add", "src"]);
    git(&work, &["commit", "-qm", "add native package code"]);
    let mut current_source = at_head(source(work.clone(), "fixture"), &work);
    current_source.policy.include_patterns = vec!["**/*.md".into()];

    let (incremental, _) = ingest_git_history_fast_forward(
        std::slice::from_ref(&current_source),
        &previous_tips,
        &cached,
    )
    .expect("incremental history")
    .expect("fast-forward history");
    let (cold, _) = cold_history(&[current_source]);
    let paths = incremental
        .iter()
        .map(|chunk| chunk.path.as_str())
        .collect::<BTreeSet<_>>();

    assert!(paths.contains("src/native.c"));
    assert!(paths.contains("src/native.hpp"));
    assert_eq!(incremental, cold);
}

#[test]
fn full_history_indexes_commit_subject_and_body_from_git() {
    let (_root, work) = fixture();
    git(
        &work,
        &[
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "Explain loader repair",
            "-m",
            "quasar_loader_regression was caused by stale package metadata",
        ],
    );
    let source = at_head(source(work.clone(), "fixture"), &work);

    let (contents, _) = cold_history(&[source]);
    let message = contents
        .iter()
        .find(|chunk| chunk.text.contains("quasar_loader_regression"))
        .expect("commit body becomes history evidence");

    assert_eq!(message.source_kind, SourceKind::GitCommit);
    assert!(message.text.contains("Explain loader repair"));
}

#[test]
fn commit_message_budget_cannot_consume_the_source_budget() {
    let (root, work) = fixture();
    fs::write(work.join("native.c"), "int native(void) { return 1; }\n").expect("C source");
    git(&work, &["add", "native.c"]);
    git(&work, &["commit", "-qm", "add native source"]);
    git(&work, &["commit", "--allow-empty", "-qm", "document one"]);
    git(&work, &["commit", "--allow-empty", "-qm", "document two"]);
    let bare = root.path().join("bounded.git");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().expect("UTF-8 worktree path"),
            bare.to_str().expect("UTF-8 bare repository path"),
        ],
    );
    let mut source = at_head(source(bare, "fixture"), &work);
    source.policy.include_patterns = vec!["native.c".into()];
    source.policy.limits = GitCorpusLimits::new(128, 1, 128).expect("bounded corpus");

    let (contents, observations) = cold_history(&[source]);

    assert!(contents.iter().any(|chunk| chunk.path == "native.c"));
    assert_eq!(
        contents
            .iter()
            .filter(|chunk| chunk.source_kind == SourceKind::GitCommit)
            .count(),
        1
    );
    assert!(observations.rejected_commit_messages >= 1);
}

#[test]
fn commit_message_budget_keeps_the_newest_history() {
    let (_root, work) = fixture();
    for message in [
        "historical message one",
        "historical message two",
        "current message three",
        "current message four",
    ] {
        git(&work, &["commit", "--allow-empty", "-qm", message]);
    }
    let mut source = at_head(source(work.clone(), "fixture"), &work);
    source.policy.limits = GitCorpusLimits::new(128, 2, 256).expect("bounded messages");

    let (contents, observations) = cold_history(&[source]);
    let messages = contents
        .iter()
        .filter(|chunk| chunk.source_kind == SourceKind::GitCommit)
        .map(|chunk| chunk.text.as_str())
        .collect::<Vec<_>>();

    assert!(
        messages
            .iter()
            .any(|message| message.contains("current message four"))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("current message three"))
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("historical message"))
    );
    assert!(observations.rejected_commit_messages >= 1);
}

#[test]
fn fast_forward_commit_message_budget_matches_a_cold_build() {
    let (_root, work) = fixture();
    let mut previous = at_head(source(work.clone(), "fixture"), &work);
    previous.policy.limits = GitCorpusLimits::new(128, 2, 256).expect("bounded messages");
    let (cached, _) = cold_history(std::slice::from_ref(&previous));

    git(
        &work,
        &["commit", "--allow-empty", "-qm", "newest message one"],
    );
    git(
        &work,
        &["commit", "--allow-empty", "-qm", "newest message two"],
    );
    let mut current = at_head(source(work.clone(), "fixture"), &work);
    current.policy.limits = previous.policy.limits;

    let (incremental, _) = ingest_git_history_fast_forward(
        std::slice::from_ref(&current),
        &snapshot_tips(std::slice::from_ref(&previous)),
        &cached,
    )
    .expect("incremental history")
    .expect("fast-forward history");
    let (cold, _) = cold_history(std::slice::from_ref(&current));

    assert_eq!(incremental, cold);
    assert_eq!(
        incremental
            .iter()
            .filter(|chunk| chunk.source_kind == SourceKind::GitCommit)
            .count(),
        2
    );
}

#[test]
fn persisted_fast_forward_commit_message_budget_matches_cold_vectors() {
    let (_root, work) = fixture();
    let mut previous = at_head(source(work.clone(), "fixture"), &work);
    previous.policy.limits = GitCorpusLimits::new(128, 2, 256).expect("bounded messages");
    let previous_root = tempdir().expect("previous artifacts");
    let mut previous_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_incrementally(
        previous_root.path(),
        std::slice::from_ref(&previous),
        None,
        None,
        Some((&mut previous_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("previous history");
    let previous_generation = previous_root
        .path()
        .join("generations")
        .join(&first.artifacts.generation);

    git(
        &work,
        &["commit", "--allow-empty", "-qm", "newest message one"],
    );
    git(
        &work,
        &["commit", "--allow-empty", "-qm", "newest message two"],
    );
    let mut current = at_head(source(work.clone(), "fixture"), &work);
    current.policy.limits = previous.policy.limits;
    let incremental_root = tempdir().expect("incremental artifacts");
    let mut incremental_provider = FakeEmbeddingProvider::new(8);
    let incremental = build_git_history_artifacts_from_previous(
        incremental_root.path(),
        std::slice::from_ref(&current),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(&[previous]),
            lexical_path: previous_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: first.artifacts.manifest.vector_checksum,
            vector_path: Some(previous_generation.join("vectors.bin")),
            lexical_format_compatible: true,
        }),
        Some((&mut incremental_provider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect("incremental history");
    let cold_root = tempdir().expect("cold artifacts");
    let mut cold_provider = FakeEmbeddingProvider::new(8);
    let cold = build_git_history_artifacts_incrementally(
        cold_root.path(),
        std::slice::from_ref(&current),
        None,
        None,
        Some((&mut cold_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("cold history");
    let generation = |root: &Path, name: &str| root.join("generations").join(name);
    let incremental_generation =
        generation(incremental_root.path(), &incremental.artifacts.generation);
    let cold_generation = generation(cold_root.path(), &cold.artifacts.generation);

    assert_cold_equivalent_lexical(
        &incremental_generation.join("lexical.json"),
        &cold_generation.join("lexical.json"),
    );
    let incremental_vectors =
        VectorArtifact::open_artifact(&incremental_generation.join("vectors.bin"))
            .expect("incremental vectors");
    let cold_vectors =
        VectorArtifact::open_artifact(&cold_generation.join("vectors.bin")).expect("cold vectors");
    assert_eq!(incremental_vectors.ids, cold_vectors.ids);
    assert_eq!(incremental_vectors.values, cold_vectors.values);
}

#[test]
fn persisted_rewrite_deletes_lost_history_and_reuses_unchanged_vectors() {
    let (_root, work) = fixture();
    let previous = at_head(source(work.clone(), "fixture"), &work);
    let previous_root = tempdir().expect("previous artifacts");
    let mut previous_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_incrementally(
        previous_root.path(),
        std::slice::from_ref(&previous),
        None,
        None,
        Some((&mut previous_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("previous history");
    let previous_generation = previous_root
        .path()
        .join("generations")
        .join(&first.artifacts.generation);

    git(&work, &["checkout", "-q", "--orphan", "rewritten"]);
    git(&work, &["rm", "-q", "-r", "-f", "."]);
    fs::create_dir_all(work.join("src")).expect("rewritten source directory");
    fs::write(work.join("src/lib.rs"), "pub fn second() -> u8 { 2 }\n").expect("same blob");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "rewritten root"]);
    let rewritten = at_head(previous.clone(), &work);

    let incremental_root = tempdir().expect("incremental artifacts");
    let mut incremental_provider = FakeEmbeddingProvider::new(8);
    let incremental = build_git_history_artifacts_from_previous(
        incremental_root.path(),
        std::slice::from_ref(&rewritten),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(&[previous]),
            lexical_path: previous_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: first.artifacts.manifest.vector_checksum,
            vector_path: Some(previous_generation.join("vectors.bin")),
            lexical_format_compatible: true,
        }),
        Some((&mut incremental_provider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect("incremental rewritten history");
    let cold_root = tempdir().expect("cold artifacts");
    let mut cold_provider = FakeEmbeddingProvider::new(8);
    let cold = build_git_history_artifacts_incrementally(
        cold_root.path(),
        &[rewritten],
        None,
        None,
        Some((&mut cold_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("cold rewritten history");
    let incremental_generation = incremental_root
        .path()
        .join("generations")
        .join(&incremental.artifacts.generation);
    let cold_generation = cold_root
        .path()
        .join("generations")
        .join(&cold.artifacts.generation);

    assert!(incremental.reused_snapshot);
    assert_cold_equivalent_lexical(
        &incremental_generation.join("lexical.json"),
        &cold_generation.join("lexical.json"),
    );
    let vectors = incremental
        .artifacts
        .observations
        .vector_build
        .expect("rewrite vector observations");
    assert!(vectors.reused_vectors > 0, "{vectors:#?}");
    assert!(vectors.embedded_vectors > 0, "{vectors:#?}");
    let incremental_vectors =
        VectorArtifact::open_artifact(&incremental_generation.join("vectors.bin"))
            .expect("incremental vectors");
    let cold_vectors =
        VectorArtifact::open_artifact(&cold_generation.join("vectors.bin")).expect("cold vectors");
    assert_eq!(incremental_vectors.ids, cold_vectors.ids);
    assert_eq!(incremental_vectors.values, cold_vectors.values);
}

#[test]
fn persisted_rewrite_resumes_its_immutable_lexical_stage_after_vector_failure() {
    let (_root, work) = fixture();
    let previous = at_head(source(work.clone(), "fixture"), &work);
    let previous_root = tempdir().expect("previous artifacts");
    let mut previous_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_incrementally(
        previous_root.path(),
        std::slice::from_ref(&previous),
        None,
        None,
        Some((&mut previous_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("previous history");
    let previous_generation = previous_root
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let previous_artifact = || PreviousGitHistoryArtifact {
        tips: snapshot_tips(std::slice::from_ref(&previous)),
        lexical_path: previous_generation.join("lexical.json"),
        corpus_digest: first.artifacts.manifest.corpus.content_digest.clone(),
        vector_checksum: first.artifacts.manifest.vector_checksum.clone(),
        vector_path: Some(previous_generation.join("vectors.bin")),
        lexical_format_compatible: true,
    };

    git(&work, &["checkout", "-q", "--orphan", "rewritten"]);
    git(&work, &["rm", "-q", "-r", "-f", "."]);
    fs::create_dir_all(work.join("src")).expect("rewritten source directory");
    fs::write(work.join("src/lib.rs"), "pub fn second() -> u8 { 2 }\n").expect("same blob");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "rewritten root"]);
    let rewritten = at_head(previous.clone(), &work);
    let artifacts = tempdir().expect("rewrite artifacts");

    build_git_history_artifacts_from_previous(
        artifacts.path(),
        std::slice::from_ref(&rewritten),
        Some(previous_artifact()),
        Some((&mut FailingEmbeddingProvider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect_err("vector failure after rewrite lexical stage");

    let mut retry_provider = FakeEmbeddingProvider::new(8);
    let mut phases = Vec::new();
    let retry = build_git_history_artifacts_from_previous(
        artifacts.path(),
        &[rewritten],
        Some(previous_artifact()),
        Some((&mut retry_provider, "fake", "test-revision")),
        None,
        &mut |phase| phases.push(phase),
    )
    .expect("resume rewritten snapshot");

    assert!(retry.artifacts.observations.reused_lexical_stage);
    assert_eq!(phases, [BuildPhase::Inference, BuildPhase::Activation]);
    assert!(retry.reused_snapshot);
}

#[test]
fn persisted_history_rehydrates_commit_messages_from_managed_git() {
    let (root, work) = fixture();
    git(
        &work,
        &[
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "Document package fix",
            "-m",
            "nebula_commit_only_evidence explains the loader transition",
        ],
    );
    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let bare = repositories.join("fixture.git");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().expect("UTF-8 worktree path"),
            bare.to_str().expect("UTF-8 bare repository path"),
        ],
    );
    let source = at_head(source(bare, "fixture"), &work);
    let artifact_root = root.path().join("artifacts").join("fixture");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &[source],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(&summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");

    let hit = index
        .search("nebula_commit_only_evidence", &LexicalFilters::default(), 1)
        .expect("search commit evidence")
        .pop()
        .expect("commit message hit");

    assert_eq!(hit.chunk.source_kind, SourceKind::GitCommit);
    assert!(hit.chunk.text.contains("nebula_commit_only_evidence"));
}

#[test]
fn commit_messages_do_not_bury_an_exact_code_identifier() {
    let (root, work) = fixture();
    for ordinal in 0..120 {
        git(
            &work,
            &[
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                &format!("Discuss second implementation {ordinal}"),
            ],
        );
    }
    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let bare = repositories.join("fixture.git");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            "--no-local",
            work.to_str().expect("UTF-8 worktree path"),
            bare.to_str().expect("UTF-8 bare repository path"),
        ],
    );
    let source = at_head(source(bare, "fixture"), &work);
    let artifact_root = root.path().join("artifacts").join("fixture");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &[source],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");

    let hits = index
        .search("second", &LexicalFilters::default(), 5)
        .expect("search identifier");

    assert!(
        hits.iter().any(|hit| {
            hit.chunk.source_kind == SourceKind::GitBlob
                && hit.chunk.text.contains("pub fn second()")
        }),
        "bounded unfiltered results buried the exact code evidence"
    );
}

#[test]
fn repeated_history_materializes_only_distinct_chunks() {
    let (_root, work) = fixture();
    fs::write(work.join("repeat.md"), "# Repeat\n\nalpha\n").expect("alpha");
    git(&work, &["add", "repeat.md"]);
    git(&work, &["commit", "-qm", "repeat alpha"]);
    fs::write(work.join("repeat.md"), "# Repeat\n\nbeta\n").expect("beta");
    git(&work, &["commit", "-qam", "repeat beta"]);
    fs::write(work.join("repeat.md"), "# Repeat\n\nalpha\n").expect("alpha again");
    git(&work, &["commit", "-qam", "repeat alpha again"]);
    let source = at_head(source(work.clone(), "fixture"), &work);

    let (contents, observations) = cold_history(&[source]);
    let repeated = contents
        .iter()
        .filter(|chunk| chunk.path == "repeat.md")
        .collect::<Vec<_>>();

    assert_eq!(repeated.len(), 3);
    assert_eq!(observations.ingested_blobs, 5);
    assert_eq!(observations.reused_blobs, 1);
    assert_eq!(observations.reused_contents, 5);
    assert!(observations.materialized_chunks >= observations.candidate_chunks);
    assert_eq!(
        observations
            .candidate_identifier_count_histogram
            .iter()
            .sum::<u64>(),
        observations.candidate_chunks as u64
    );
    assert_eq!(
        observations
            .materialized_identifier_count_histogram
            .iter()
            .sum::<u64>(),
        observations.materialized_chunks as u64
    );
    assert!(repeated.iter().all(|chunk| !chunk.identifiers.is_empty()));
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
fn cold_history_bounds_files_and_bytes_before_loading_blobs() {
    let (_root, work) = fixture();
    add_bounded_delta(&work);
    let source = bounded_source(&work);

    let (contents, observations) = cold_history(&[source]);

    assert_bounded_delta(&contents, &observations);
}

#[test]
fn cold_history_spends_a_bounded_budget_on_the_current_tree_first() {
    let (_root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/obsolete.rs"), "pub fn obsolete() {}\n").expect("obsolete source");
    git(&work, &["add", "limits"]);
    git(&work, &["commit", "-qm", "obsolete source"]);
    fs::remove_file(work.join("limits/obsolete.rs")).expect("remove obsolete source");
    fs::write(work.join("limits/current.rs"), "pub fn current() {}\n").expect("current source");
    git(&work, &["add", "limits"]);
    git(&work, &["commit", "-qm", "current source"]);
    let mut source = at_head(source(work.clone(), "fixture"), &work);
    source.policy.include_prefixes = vec!["limits".into()];
    source.policy.limits = GitCorpusLimits::new(64, 1, 64).expect("valid test limits");

    let (contents, observations) = cold_history(&[source]);

    assert_eq!(
        contents
            .iter()
            .filter(|chunk| chunk.source_kind == SourceKind::GitBlob)
            .map(|chunk| chunk.path.as_str())
            .collect::<Vec<_>>(),
        ["limits/current.rs"],
        "deleted historical files must not starve the selected tip"
    );
    assert_eq!(observations.ingested_blobs, 1);
    assert_eq!(observations.budget_rejections, 1);
}

#[test]
fn cold_history_seeds_files_unchanged_at_the_selected_tip_before_deleted_history() {
    let (_root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/current.rs"), "pub fn current() {}\n").expect("current source");
    git(&work, &["add", "limits/current.rs"]);
    git(&work, &["commit", "-qm", "long-lived current source"]);
    fs::write(work.join("limits/obsolete.rs"), "pub fn obsolete() {}\n").expect("obsolete source");
    git(&work, &["add", "limits/obsolete.rs"]);
    git(&work, &["commit", "-qm", "obsolete source"]);
    fs::remove_file(work.join("limits/obsolete.rs")).expect("remove obsolete source");
    git(&work, &["add", "limits/obsolete.rs"]);
    git(&work, &["commit", "-qm", "remove obsolete source"]);
    let mut source = at_head(source(work.clone(), "fixture"), &work);
    source.policy.include_prefixes = vec!["limits".into()];
    source.policy.limits = GitCorpusLimits::new(64, 1, 64).expect("valid test limits");

    let (contents, _) = cold_history(&[source]);

    assert_eq!(
        contents
            .iter()
            .filter(|chunk| chunk.source_kind == SourceKind::GitBlob)
            .map(|chunk| chunk.path.as_str())
            .collect::<Vec<_>>(),
        [
            "limits/current.rs",
            "limits/current.rs",
            "limits/current.rs",
        ]
    );
}

#[test]
fn incremental_history_bounds_only_the_new_delta_before_loading_blobs() {
    let (_root, work) = fixture();
    let previous_source = bounded_source(&work);
    let (cached, _) = cold_history(std::slice::from_ref(&previous_source));
    assert!(
        cached
            .iter()
            .all(|chunk| chunk.source_kind == SourceKind::GitCommit)
    );
    let previous_tips = snapshot_tips(std::slice::from_ref(&previous_source));
    add_bounded_delta(&work);
    let current_source = bounded_source(&work);

    let (contents, observations) =
        ingest_git_history_fast_forward(&[current_source], &previous_tips, &cached)
            .expect("incremental history")
            .expect("fast-forward history");

    assert_eq!(observations.reused_commits, 3);
    assert_eq!(observations.ingested_commits, 1);
    assert_bounded_delta(&contents, &observations);
}

#[test]
fn incremental_tip_reservation_does_not_charge_unchanged_rejected_files() {
    let (_root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/a.rs"), "pub fn admitted() {}\n").expect("admitted source");
    fs::write(work.join("limits/b.rs"), "pub fn rejected() {}\n").expect("rejected source");
    git(&work, &["add", "limits"]);
    git(&work, &["commit", "-qm", "two bounded files"]);
    let mut previous = at_head(source(work.clone(), "fixture"), &work);
    previous.policy.include_prefixes = vec!["limits".into()];
    previous.policy.limits = GitCorpusLimits::new(64, 1, 64).expect("valid test limits");
    let (cached, _) = cold_history(std::slice::from_ref(&previous));
    let previous_tips = snapshot_tips(std::slice::from_ref(&previous));
    assert_eq!(
        cached
            .iter()
            .filter(|chunk| chunk.source_kind == SourceKind::GitBlob)
            .count(),
        1,
        "one of the two previous files must be budget-rejected"
    );

    fs::write(work.join("limits/c.rs"), "pub fn new_delta() {}\n").expect("delta source");
    git(&work, &["add", "limits/c.rs"]);
    git(&work, &["commit", "-qm", "new bounded delta"]);
    let current = at_head(previous, &work);
    let (contents, observations) =
        ingest_git_history_fast_forward(std::slice::from_ref(&current), &previous_tips, &cached)
            .expect("incremental history")
            .expect("fast-forward history");

    assert!(
        contents
            .iter()
            .any(|chunk| chunk.source_kind == SourceKind::GitBlob
                && chunk.text.contains("new_delta")),
        "the new delta must receive the incremental budget"
    );
    assert_eq!(observations.ingested_blobs, 1);
}

#[test]
fn full_history_build_with_provider_writes_matching_vectors() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let mut provider = FakeEmbeddingProvider::new(8);
    let checkpoint = artifacts.path().join("vector-checkpoint.bin");

    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut provider, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect("semantic history build");

    assert_eq!(
        summary.refresh.materialized_chunks, 0,
        "history ingestion must retain compact Git locators, not owned chunks"
    );
    assert!(checkpoint.is_file());
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
    let graph_checksum = summary
        .artifacts
        .manifest
        .graph_checksum
        .as_ref()
        .expect("full-history graph checksum");
    let graph_path = artifacts
        .path()
        .join("generations")
        .join(&summary.artifacts.generation)
        .join("graph.bin");
    let graph = GraphArtifact::open(&graph_path).expect("full-history graph");
    assert_eq!(
        graph.encoded_digest().expect("graph digest"),
        *graph_checksum
    );
    assert_eq!(graph.nodes.len(), summary.artifacts.chunk_count);
    assert_eq!(
        summary.artifacts.graph_bytes,
        Some(fs::metadata(graph_path).expect("graph metadata").len())
    );
}

#[test]
fn semantic_build_embeds_each_git_history_occurrence() {
    let (_root, work) = fixture();
    let passage = "bounded semantic passage ".repeat(300);
    let mut content = String::new();
    for section in 0..8 {
        writeln!(content, "# Section {section}\n\n{passage}\n").expect("write fixture content");
    }
    fs::write(work.join("many-chunks.md"), content).expect("multi-chunk source");
    git(&work, &["add", "many-chunks.md"]);
    git(&work, &["commit", "-qm", "many chunks"]);
    let source = at_head(source(work.clone(), "fixture"), &work);
    let (history, _observations) = cold_history(std::slice::from_ref(&source));
    assert!(
        history
            .iter()
            .filter(|chunk| chunk.path == "many-chunks.md")
            .count()
            > 1,
        "fixture must cross semantic batch boundaries"
    );

    let artifacts = tempdir().expect("artifact root");
    let mut provider = SingleItemEmbeddingProvider::new(8);
    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[source],
        None,
        None,
        Some((&mut provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("semantic history build");

    assert_eq!(
        provider.embedded_documents, summary.artifacts.chunk_count,
        "semantic inference includes every indexed revision occurrence"
    );
}

#[test]
fn full_history_build_reports_real_phase_boundaries() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let mut provider = FakeEmbeddingProvider::new(8);
    let mut phases = Vec::new();

    build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[source],
        None,
        None,
        Some((&mut provider, "fake", "test-revision")),
        None,
        None,
        &mut |phase| phases.push(phase),
    )
    .expect("semantic history build");

    assert_eq!(
        phases,
        [
            BuildPhase::Lexical,
            BuildPhase::Inference,
            BuildPhase::Activation,
        ]
    );
}

#[test]
fn semantic_retry_reuses_completed_lexical_stage_and_checkpoint() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let checkpoint = artifacts.path().join("vector-checkpoint.bin");
    let mut failing = FailAfterOneBatch::default();

    let error = build_git_history_artifacts_incrementally(
        artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut failing, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect_err("semantic failure");
    assert!(
        error
            .to_string()
            .contains("expected semantic failure after one batch")
    );
    assert!(
        checkpoint.is_file(),
        "the successful first batch is durable"
    );

    let mut retry = FakeEmbeddingProvider::new(8);
    let mut retry_phases = Vec::new();
    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut retry, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut |phase| retry_phases.push(phase),
    )
    .expect("resume semantic build");

    assert!(
        summary.artifacts.observations.reused_lexical_stage,
        "retry must skip Git planning and Tantivy construction"
    );
    assert_eq!(
        retry_phases,
        [BuildPhase::Inference, BuildPhase::Activation],
        "a resumed stage must not report lexical construction"
    );
    let vectors = summary
        .artifacts
        .observations
        .vector_build
        .expect("vector observations");
    assert!(vectors.reused_vectors > 0, "checkpoint rows must be reused");
    assert_eq!(
        vectors.reused_vectors + vectors.embedded_vectors,
        summary.artifacts.chunk_count
    );

    let clean_artifacts = tempdir().expect("clean artifact root");
    let mut clean_provider = FakeEmbeddingProvider::new(8);
    let clean = build_git_history_artifacts_incrementally(
        clean_artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut clean_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("clean semantic build");
    assert_eq!(summary.artifacts.manifest, clean.artifacts.manifest);
    assert_eq!(
        fs::read(
            artifacts
                .path()
                .join("generations")
                .join(&summary.artifacts.generation)
                .join("vectors.bin")
        )
        .expect("resumed vectors"),
        fs::read(
            clean_artifacts
                .path()
                .join("generations")
                .join(&clean.artifacts.generation)
                .join("vectors.bin")
        )
        .expect("clean vectors"),
        "retry must produce the same final vector artifact as a clean build"
    );
}

#[test]
fn changed_tip_rejects_completed_lexical_stage() {
    let (_root, work) = fixture();
    let first_source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let checkpoint = artifacts.path().join("vector-checkpoint.bin");
    let mut failing = FailingEmbeddingProvider;
    build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[first_source],
        None,
        None,
        Some((&mut failing, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect_err("semantic failure");

    fs::write(work.join("new.md"), "new tip passage\n").expect("new passage");
    git(&work, &["add", "new.md"]);
    git(&work, &["commit", "-qm", "new tip"]);
    let second_source = at_head(source(work.clone(), "fixture"), &work);
    let mut retry = FakeEmbeddingProvider::new(8);
    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[second_source],
        None,
        None,
        Some((&mut retry, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect("changed-tip build");

    assert!(
        !summary.artifacts.observations.reused_lexical_stage,
        "a different immutable source contract must rebuild lexical data"
    );
    assert_eq!(summary.refresh.reachable_commits, 4);
}

#[test]
fn corrupt_completed_lexical_stage_is_rebuilt() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let checkpoint = artifacts.path().join("vector-checkpoint.bin");
    let mut failing = FailingEmbeddingProvider;
    build_git_history_artifacts_incrementally(
        artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut failing, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect_err("semantic failure");
    fs::write(
        artifacts.path().join("lexical-stage/complete.json"),
        b"corrupt",
    )
    .expect("corrupt stage marker");

    let mut retry = FakeEmbeddingProvider::new(8);
    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[source],
        None,
        None,
        Some((&mut retry, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect("rebuild corrupt stage");

    assert!(
        !summary.artifacts.observations.reused_lexical_stage,
        "corrupt private staging must never become query-visible"
    );
}

#[test]
fn changed_completed_lexical_sidecar_is_rebuilt() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let artifacts = tempdir().expect("artifact root");
    let checkpoint = artifacts.path().join("vector-checkpoint.bin");
    let mut failing = FailingEmbeddingProvider;
    build_git_history_artifacts_incrementally(
        artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut failing, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect_err("semantic failure");
    fs::write(
        artifacts
            .path()
            .join("lexical-stage/lexical.tantivy/untracked"),
        b"changed sidecar",
    )
    .expect("change completed sidecar without changing its chunk IDs");

    let mut retry = FakeEmbeddingProvider::new(8);
    let summary = build_git_history_artifacts_incrementally(
        artifacts.path(),
        &[source],
        None,
        None,
        Some((&mut retry, "fake", "test-revision")),
        None,
        Some(&checkpoint),
        &mut ignore_build_phase,
    )
    .expect("rebuild changed sidecar");

    assert!(
        !summary.artifacts.observations.reused_lexical_stage,
        "the completion marker must bind every immutable Tantivy sidecar file"
    );
}

#[test]
fn persisted_history_hydrates_top_hits_from_managed_git_without_stored_chunks() {
    let (root, work) = fixture();
    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let bare = repositories.join("fixture.git");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().expect("UTF-8 worktree path"),
            bare.to_str().expect("UTF-8 bare repository path"),
        ],
    );
    let source = at_head(source(bare.clone(), "fixture"), &work);
    let artifact_root = root.path().join("artifacts").join("fixture");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &[source],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(&summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");

    assert!(
        index.schema().get_field("chunk_json").is_err(),
        "persisted lexical schema must not retain serialized chunks"
    );
    assert!(
        index.schema().get_field("git_object_id").is_ok(),
        "Git locators must address objects directly instead of walking commit trees per chunk"
    );
    let hit = index
        .search(
            "second",
            &LexicalFilters {
                source_kind: Some(SourceKind::GitBlob),
                ..LexicalFilters::default()
            },
            1,
        )
        .expect("hydrate Git hit")
        .pop()
        .expect("matching history chunk");
    assert_eq!(hit.chunk.path, "src/lib.rs");
    assert!(hit.chunk.text.contains("pub fn second()"));
    hit.chunk.validate().expect("hydrated chunk contract");

    fs::remove_dir_all(&bare).expect("remove managed repository");
    let error = index
        .search(
            "first",
            &LexicalFilters {
                source_kind: Some(SourceKind::GitBlob),
                ..LexicalFilters::default()
            },
            1,
        )
        .expect_err("missing canonical Git storage must fail");
    assert!(error.to_string().contains("fixture"));
    assert!(error.to_string().contains("managed Git repository"));
}

#[test]
fn persisted_search_hydrates_only_returned_top_k() {
    let (root, work) = fixture();
    fs::write(
        work.join("src/exact.rs"),
        "pub fn exact_signal() -> bool { true }\n",
    )
    .expect("exact identifier source");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "exact identifier"]);

    let prose = root.path().join("prose-work");
    fs::create_dir(&prose).expect("prose worktree");
    git(&prose, &["init", "-q", "-b", "main"]);
    git(&prose, &["config", "user.email", "fixture@example.invalid"]);
    git(&prose, &["config", "user.name", "Fixture"]);
    fs::write(
        prose.join("README.md"),
        "# Notes\n\nThis passage mentions exact_signal only in prose.\n",
    )
    .expect("lower-ranked prose source");
    git(&prose, &["add", "."]);
    git(&prose, &["commit", "-qm", "prose mention"]);

    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let exact_bare = repositories.join("exact.git");
    let prose_bare = repositories.join("prose.git");
    for (worktree, bare) in [(&work, &exact_bare), (&prose, &prose_bare)] {
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                worktree.to_str().expect("UTF-8 worktree path"),
                bare.to_str().expect("UTF-8 bare repository path"),
            ],
        );
    }

    let sources = [
        at_head(source(exact_bare, "exact"), &work),
        at_head(source(prose_bare.clone(), "prose"), &prose),
    ];
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &sources,
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(&summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");

    fs::remove_dir_all(prose_bare).expect("remove lower-ranked repository");
    let candidates = index
        .search_candidates("exact_signal", &LexicalFilters::default(), 20)
        .expect("candidate ranking must not hydrate Git passages");
    assert_eq!(
        candidates.len(),
        2,
        "the unavailable lower-ranked candidate must participate in fusion"
    );
    let metadata = candidates
        .iter()
        .map(|candidate| (candidate.chunk.chunk_id.clone(), candidate.chunk.clone()))
        .collect::<BTreeMap<_, _>>();
    let fused = fuse_candidate_metadata(
        &candidates,
        &[],
        &metadata,
        FusionConfig {
            limit: 1,
            ..FusionConfig::default()
        },
    );
    let ids = fused
        .iter()
        .map(|candidate| candidate.chunk.chunk_id.clone())
        .collect::<BTreeSet<_>>();
    let mut chunks = index
        .chunks_by_id(&ids)
        .expect("only the returned top-k passage is hydrated");
    let top = chunks
        .remove(&fused[0].chunk.chunk_id)
        .expect("fused top candidate");

    assert_eq!(top.repository.as_str(), "exact");
    assert!(fused[0].exact_identifier);
}

#[test]
fn repository_filter_matches_a_canonical_locator_present_in_a_managed_fork() {
    let (root, work) = fixture();
    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let alpha = repositories.join("alpha.git");
    let beta = repositories.join("beta.git");
    for bare in [&alpha, &beta] {
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().expect("UTF-8 worktree path"),
                bare.to_str().expect("UTF-8 bare repository path"),
            ],
        );
    }
    let sources = [
        at_head(source(alpha, "alpha"), &work),
        at_head(source(beta, "beta"), &work),
    ];
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &sources,
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("beta").expect("repository")),
        source_kind: Some(SourceKind::GitBlob),
        ..LexicalFilters::default()
    };

    let candidates = index
        .search_candidates("first", &filters, 10)
        .expect("shared revision matches repository filter");
    assert!(!candidates.is_empty());
    let ids = candidates
        .iter()
        .map(|candidate| candidate.chunk.chunk_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        index
            .metadata_by_id(&ids, &filters)
            .expect("filtered metadata")
            .len(),
        candidates.len()
    );
}

#[test]
fn repository_filter_does_not_cross_git_corpus_contracts() {
    let (root, work) = fixture();
    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let alpha = repositories.join("alpha.git");
    let beta = repositories.join("beta.git");
    for bare in [&alpha, &beta] {
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().expect("UTF-8 worktree path"),
                bare.to_str().expect("UTF-8 bare repository path"),
            ],
        );
    }
    let alpha = at_head(source(alpha, "alpha"), &work);
    let mut beta = at_head(source(beta, "beta"), &work);
    beta.policy.exclude_prefixes.push("src".into());
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &[alpha, beta],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("beta").expect("repository")),
        source_kind: Some(SourceKind::GitBlob),
        ..LexicalFilters::default()
    };

    assert!(
        index
            .search_candidates("first", &filters, 10)
            .expect("repository-filtered search")
            .is_empty(),
        "a repository filter must not alias evidence excluded by its corpus contract"
    );
}

#[test]
fn repository_filter_requires_revision_reachability_not_object_presence() {
    let (root, work) = fixture();
    git(&work, &["switch", "-qc", "alpha"]);
    fs::write(
        work.join("src/alpha.rs"),
        "pub fn alpha_exclusive_signal() {}\n",
    )
    .expect("alpha source");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "alpha-only"]);
    let alpha_tip = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["switch", "-qc", "beta", "v1"]);
    fs::write(work.join("src/beta.rs"), "pub fn beta_signal() {}\n").expect("beta source");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "beta-only"]);
    let beta_tip = git(&work, &["rev-parse", "HEAD"]);

    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let shared = repositories.join("shared.git");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            work.to_str().expect("UTF-8 worktree path"),
            shared.to_str().expect("UTF-8 bare repository path"),
        ],
    );
    let beta_repository = repositories.join("beta.git");
    git(
        root.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            shared.to_str().expect("UTF-8 shared repository path"),
            beta_repository
                .to_str()
                .expect("UTF-8 beta repository path"),
        ],
    );
    let alpha_id =
        gix::ObjectId::from_hex(alpha_tip.as_bytes()).expect("valid alpha object identifier");
    assert!(
        gix::open(&beta_repository)
            .expect("open beta repository")
            .has_object(alpha_id),
        "the beta object database must contain the unreachable alpha commit"
    );
    let mut alpha = source(shared, "alpha");
    alpha.revision = Revision::try_from(alpha_tip).expect("alpha revision");
    alpha.history_tips = vec![alpha.revision.clone()];
    let mut beta = source(beta_repository, "beta");
    beta.revision = Revision::try_from(beta_tip).expect("beta revision");
    beta.history_tips = vec![beta.revision.clone()];
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &[alpha, beta],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("beta").expect("repository")),
        ..LexicalFilters::default()
    };

    assert!(
        index
            .search_candidates("alpha_exclusive_signal", &filters, 10)
            .expect("repository-filtered search")
            .is_empty(),
        "an unreachable commit in the same object database is not repository membership"
    );
}

#[test]
fn repository_filter_is_applied_before_the_global_top_docs_limit() {
    let root = tempdir().expect("fixture root");
    let noise = root.path().join("noise-work");
    let target = root.path().join("target-work");
    for work in [&noise, &target] {
        fs::create_dir(work).expect("worktree");
        git(work, &["init", "-q", "-b", "main"]);
        git(work, &["config", "user.email", "fixture@example.invalid"]);
        git(work, &["config", "user.name", "Fixture"]);
    }
    fs::create_dir(noise.join("src")).expect("noise source directory");
    for index in 0..128 {
        fs::write(
            noise.join(format!("src/noise-{index}.rs")),
            "buried_signal ".repeat(32),
        )
        .expect("noise source");
    }
    git(&noise, &["add", "."]);
    git(&noise, &["commit", "-qm", "high-scoring noise"]);
    fs::write(target.join("target.rs"), "buried_signal\n").expect("target source");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-qm", "target"]);

    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let noise_bare = repositories.join("noise.git");
    let target_bare = repositories.join("target.git");
    for (work, bare) in [(&noise, &noise_bare), (&target, &target_bare)] {
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().expect("UTF-8 worktree path"),
                bare.to_str().expect("UTF-8 bare repository path"),
            ],
        );
    }
    let sources = [
        at_head(source(noise_bare, "noise"), &noise),
        at_head(source(target_bare, "target"), &target),
    ];
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &sources,
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("target").expect("repository")),
        ..LexicalFilters::default()
    };

    let candidates = index
        .search_candidates("buried_signal", &filters, 1)
        .expect("repository-filtered search");
    assert_eq!(candidates.len(), 1);
}

#[test]
fn enabling_semantic_reuses_lexical_history_and_builds_only_vectors() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let first_artifacts = tempdir().expect("first artifact root");
    let first = build_git_history_artifacts_incrementally(
        first_artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("lexical history build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut provider = FakeEmbeddingProvider::new(8);

    let second = build_git_history_artifacts_from_previous(
        second_artifacts.path(),
        std::slice::from_ref(&source),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(std::slice::from_ref(&source)),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: None,
            vector_path: None,
            lexical_format_compatible: true,
        }),
        Some((&mut provider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect("semantic upgrade");
    let vectors = second
        .artifacts
        .observations
        .vector_build
        .expect("vector observations");

    assert!(second.reused_snapshot);
    assert_eq!(second.refresh.ingested_commits, 0);
    assert_eq!(vectors.reused_vectors, 0);
    assert_eq!(vectors.embedded_vectors, second.artifacts.chunk_count);
}

#[test]
fn selected_tree_reuses_every_vector_already_present_in_complete_history() {
    let (_root, work) = fixture();
    let history_source = at_head(source(work.clone(), "fixture"), &work);
    let history_root = tempdir().expect("history artifact root");
    let mut history_provider = FakeEmbeddingProvider::new(8);
    let history = build_git_history_artifacts_incrementally(
        history_root.path(),
        std::slice::from_ref(&history_source),
        None,
        None,
        Some((&mut history_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("complete-history semantic build");
    let history_generation = history_root
        .path()
        .join("generations")
        .join(&history.artifacts.generation);
    let selected_source = GitCorpusSource {
        revision: Revision::try_from(git(&work, &["rev-parse", "v1"])).expect("v1 revision"),
        history_tips: Vec::new(),
        ..history_source
    };
    let selected_root = tempdir().expect("selected artifact root");

    let selected = build_git_artifacts_with_provider(
        selected_root.path(),
        std::slice::from_ref(&selected_source),
        Some((&mut FailingEmbeddingProvider, "fake", "test-revision")),
        Some(&previous_vector_artifact(
            &history_generation,
            history.artifacts.manifest.corpus.content_digest,
            history
                .artifacts
                .manifest
                .vector_checksum
                .expect("history vector checksum"),
        )),
        None,
    )
    .expect("selected tree reuses history vectors");
    let vectors = selected
        .observations
        .vector_build
        .expect("vector observations");

    assert_eq!(vectors.reused_vectors, selected.chunk_count);
    assert_eq!(vectors.embedded_vectors, 0);
    let lexical = LexicalIndex::open_git_search_artifact(
        &selected_root
            .path()
            .join("generations")
            .join(selected.generation)
            .join("lexical.json"),
        work.parent().expect("fixture repository parent"),
    )
    .expect("selected lexical index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("fixture").expect("repository")),
        ..LexicalFilters::default()
    };
    assert!(
        lexical
            .search_candidates("second", &filters, 1)
            .expect("selected search")
            .is_empty(),
        "the selected-tree index must not expose later history"
    );
}

#[test]
fn selected_tree_reuses_split_source_vectors_with_passage_local_identifiers() {
    let (_root, work) = fixture();
    let mut source_text = String::with_capacity(16 * 1024);
    for index in 0..256 {
        writeln!(
            source_text,
            "int release_symbol_{index}(void) {{ return {index}; }}"
        )
        .expect("write source fixture");
    }
    fs::write(work.join("src/release.c"), source_text).expect("split source");
    git(&work, &["add", "src/release.c"]);
    git(&work, &["commit", "-qm", "add split release source"]);
    fs::write(
        work.join("README.md"),
        "# Fixture\n\nLater documentation.\n",
    )
    .expect("later source");
    git(&work, &["commit", "-qam", "later documentation"]);
    let selected_revision = git(&work, &["rev-parse", "HEAD"]);

    let history_source = at_head(source(work.clone(), "fixture"), &work);
    let history_root = tempdir().expect("history artifact root");
    let mut history_provider = FakeEmbeddingProvider::new(8);
    let history = build_git_history_artifacts_incrementally(
        history_root.path(),
        std::slice::from_ref(&history_source),
        None,
        None,
        Some((&mut history_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("complete-history semantic build");
    let history_generation = history_root
        .path()
        .join("generations")
        .join(&history.artifacts.generation);
    let selected_source = GitCorpusSource {
        revision: Revision::try_from(selected_revision).expect("selected revision"),
        history_tips: Vec::new(),
        ..history_source
    };
    let selected_root = tempdir().expect("selected artifact root");

    let selected = build_git_artifacts_with_provider(
        selected_root.path(),
        std::slice::from_ref(&selected_source),
        Some((&mut FailingEmbeddingProvider, "fake", "test-revision")),
        Some(&previous_vector_artifact(
            &history_generation,
            history.artifacts.manifest.corpus.content_digest,
            history
                .artifacts
                .manifest
                .vector_checksum
                .expect("history vector checksum"),
        )),
        None,
    )
    .expect("selected split source reuses history vectors");
    let vectors = selected
        .observations
        .vector_build
        .expect("vector observations");

    assert_eq!(vectors.reused_vectors, selected.chunk_count);
    assert_eq!(vectors.embedded_vectors, 0);
}

#[test]
fn selected_tree_embeds_only_chunks_missing_from_complete_history() {
    let (_root, work) = fixture();
    let history_source = at_head(source(work.clone(), "fixture"), &work);
    let history_root = tempdir().expect("history artifact root");
    let mut history_provider = FakeEmbeddingProvider::new(8);
    let history = build_git_history_artifacts_incrementally(
        history_root.path(),
        std::slice::from_ref(&history_source),
        None,
        None,
        Some((&mut history_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("complete-history semantic build");
    let history_generation = history_root
        .path()
        .join("generations")
        .join(&history.artifacts.generation);
    fs::write(work.join("src/novel.rs"), "pub fn novel_vector() {}\n").expect("novel source");
    git(&work, &["add", "src/novel.rs"]);
    git(&work, &["commit", "-qm", "novel source"]);
    let selected_source = at_head(history_source, &work);
    let selected_root = tempdir().expect("selected artifact root");
    let mut selected_provider = FakeEmbeddingProvider::new(8);

    let selected = build_git_artifacts_with_provider(
        selected_root.path(),
        std::slice::from_ref(&selected_source),
        Some((&mut selected_provider, "fake", "test-revision")),
        Some(&previous_vector_artifact(
            &history_generation,
            history.artifacts.manifest.corpus.content_digest,
            history
                .artifacts
                .manifest
                .vector_checksum
                .expect("history vector checksum"),
        )),
        None,
    )
    .expect("selected tree reconciles history vectors");
    let vectors = selected
        .observations
        .vector_build
        .expect("vector observations");

    assert_eq!(vectors.embedded_vectors, 1);
    assert_eq!(
        vectors.reused_vectors + vectors.embedded_vectors,
        selected.chunk_count
    );
}

#[test]
fn model_change_reuses_lexical_history_without_reusing_incompatible_vectors() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let first_artifacts = tempdir().expect("first artifact root");
    let mut first_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_incrementally(
        first_artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut first_provider, "fake", "first-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("first semantic history build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut second_provider = FakeEmbeddingProvider::new(8);

    let second = build_git_history_artifacts_from_previous(
        second_artifacts.path(),
        std::slice::from_ref(&source),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(std::slice::from_ref(&source)),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: first.artifacts.manifest.vector_checksum,
            vector_path: Some(first_generation.join("vectors.bin")),
            lexical_format_compatible: true,
        }),
        Some((&mut second_provider, "fake", "next-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect("semantic model change");
    let vectors = second
        .artifacts
        .observations
        .vector_build
        .expect("vector observations");

    assert!(second.reused_snapshot);
    assert_eq!(second.refresh.ingested_commits, 0);
    assert_eq!(vectors.reused_vectors, 0);
    assert_eq!(vectors.embedded_vectors, second.artifacts.chunk_count);
}

#[test]
fn lexical_format_upgrade_reindexes_but_reuses_all_matching_vectors() {
    let (_root, work) = fixture();
    let source = at_head(source(work.clone(), "fixture"), &work);
    let first_artifacts = tempdir().expect("first artifact root");
    let mut first_provider = FakeEmbeddingProvider::new(8);
    let first = build_git_history_artifacts_incrementally(
        first_artifacts.path(),
        std::slice::from_ref(&source),
        None,
        None,
        Some((&mut first_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("first semantic history build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut second_provider = FakeEmbeddingProvider::new(8);

    let second = build_git_history_artifacts_from_previous(
        second_artifacts.path(),
        std::slice::from_ref(&source),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(std::slice::from_ref(&source)),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: first.artifacts.manifest.vector_checksum,
            vector_path: Some(first_generation.join("vectors.bin")),
            lexical_format_compatible: false,
        }),
        Some((&mut second_provider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect("reindexed semantic history build");
    let observations = second
        .artifacts
        .observations
        .vector_build
        .expect("vector observations");

    assert!(second.reused_snapshot);
    assert_eq!(observations.reused_vectors, second.artifacts.chunk_count);
    assert_eq!(observations.embedded_vectors, 0);
}

#[test]
#[allow(clippy::too_many_lines)] // One end-to-end incremental vector reuse scenario.
fn changed_tip_reuses_existing_vectors_and_embeds_only_new_chunks() {
    let (_root, work) = fixture();
    add_tokenized_path_history(&work);
    let first_source = at_head(source(work.clone(), "fixture"), &work);
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
        &mut ignore_build_phase,
    )
    .expect("first semantic history build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let first_vector_path = first_generation.join("vectors.bin");
    let first_vector_bytes = fs::read(&first_vector_path).expect("first vector bytes");

    fs::write(work.join("new.md"), "a new semantic passage\n").expect("new passage");
    fs::write(
        work.join("src/full_history.rs"),
        "pub fn underscored_path_v2() {}\n",
    )
    .expect("changed underscored path");
    fs::write(work.join("foo-bar.md"), "changed hyphenated path\n")
        .expect("changed hyphenated path");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "new passage"]);
    let second_source = at_head(source(work.clone(), "fixture"), &work);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut second_provider = FakeEmbeddingProvider::new(8);
    let second = build_git_history_artifacts_from_previous(
        second_artifacts.path(),
        std::slice::from_ref(&second_source),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(&[first_source]),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest.clone(),
            vector_checksum: first.artifacts.manifest.vector_checksum.clone(),
            vector_path: Some(first_vector_path.clone()),
            lexical_format_compatible: true,
        }),
        Some((&mut second_provider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
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
    assert!(second.artifacts.manifest.corpus.documents.is_empty());
    assert!(second.artifacts.manifest.corpus.chunks.is_empty());
    assert_eq!(
        fs::read(&first_vector_path).expect("unchanged predecessor"),
        first_vector_bytes
    );

    let cold_artifacts = tempdir().expect("cold artifact root");
    let mut cold_provider = FakeEmbeddingProvider::new(8);
    let cold = build_git_history_artifacts_incrementally(
        cold_artifacts.path(),
        &[second_source],
        None,
        None,
        Some((&mut cold_provider, "fake", "test-revision")),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("cold semantic history build");
    assert_eq!(
        second.artifacts.manifest.corpus,
        cold.artifacts.manifest.corpus
    );
    assert_eq!(
        second.artifacts.manifest.corpus.content_digest,
        cold.artifacts.manifest.corpus.content_digest
    );
    assert_eq!(
        second.artifacts.manifest.vector_checksum,
        cold.artifacts.manifest.vector_checksum
    );
    let second_generation = second_artifacts
        .path()
        .join("generations")
        .join(&second.artifacts.generation);
    let cold_generation = cold_artifacts
        .path()
        .join("generations")
        .join(&cold.artifacts.generation);
    assert_eq!(
        VectorArtifact::open_artifact(&second_generation.join("vectors.bin"))
            .expect("incremental vectors"),
        VectorArtifact::open_artifact(&cold_generation.join("vectors.bin")).expect("cold vectors")
    );
    assert_cold_equivalent_lexical(
        &second_generation.join("lexical.json"),
        &cold_generation.join("lexical.json"),
    );
}

#[test]
fn corrupt_previous_vector_falls_back_during_a_lexical_format_upgrade() {
    let (_root, work) = fixture();
    let first_source = at_head(source(work.clone(), "fixture"), &work);
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
        &mut ignore_build_phase,
    )
    .expect("first build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let original_vector_path = first_generation.join("vectors.bin");
    let original_vector_bytes = fs::read(&original_vector_path).expect("original vectors");
    let corrupt_vector_path = first_artifacts.path().join("corrupt-vectors.bin");
    let mut corrupt_vector_bytes = original_vector_bytes.clone();
    corrupt_vector_bytes[16] ^= 0xff;
    fs::write(&corrupt_vector_path, corrupt_vector_bytes).expect("corrupt copy");

    fs::write(work.join("new.md"), "a new semantic passage\n").expect("new passage");
    git(&work, &["add", "new.md"]);
    git(&work, &["commit", "-qm", "new passage"]);
    let second_source = at_head(source(work.clone(), "fixture"), &work);
    let second_artifacts = tempdir().expect("second artifact root");
    let mut second_provider = FakeEmbeddingProvider::new(8);
    let second = build_git_history_artifacts_from_previous(
        second_artifacts.path(),
        &[second_source],
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(&[first_source]),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: first.artifacts.manifest.vector_checksum,
            vector_path: Some(corrupt_vector_path),
            lexical_format_compatible: false,
        }),
        Some((&mut second_provider, "fake", "test-revision")),
        None,
        &mut ignore_build_phase,
    )
    .expect("fallback build");

    assert!(!second.reused_snapshot);
    assert_eq!(
        second
            .artifacts
            .observations
            .vector_build
            .expect("vector observations")
            .reused_vectors,
        0
    );
    assert_eq!(
        fs::read(original_vector_path).expect("original predecessor"),
        original_vector_bytes
    );
}

#[test]
fn mismatched_lexical_inventory_falls_back_to_a_complete_build() {
    let (_root, work) = fixture();
    let first_source = at_head(source(work.clone(), "fixture"), &work);
    let first_artifacts = tempdir().expect("first artifact root");
    let first = build_git_history_artifacts_incrementally(
        first_artifacts.path(),
        std::slice::from_ref(&first_source),
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("first build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let mismatched_digest = ContentDigest::of(b"wrong inventory");

    fs::write(work.join("new.md"), "a new passage\n").expect("new passage");
    git(&work, &["add", "new.md"]);
    git(&work, &["commit", "-qm", "new passage"]);
    let second_source = at_head(source(work.clone(), "fixture"), &work);
    let second_artifacts = tempdir().expect("second artifact root");
    let second = build_git_history_artifacts_from_previous(
        second_artifacts.path(),
        &[second_source],
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(&[first_source]),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: mismatched_digest,
            vector_checksum: None,
            vector_path: None,
            lexical_format_compatible: true,
        }),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("fallback build");

    assert!(!second.reused_snapshot);
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
    assert!(
        incremental.0.iter().all(|chunk| {
            chunk.resource_uri.as_ref().is_some_and(|uri| {
                uri.as_str() == format!("vesc://knowledge/chunk/{}", chunk.chunk_id)
            })
        }),
        "public history ingestion must return canonical chunk resource URIs"
    );
}

#[test]
fn fast_forward_reuses_existing_repository_when_one_is_added() {
    let (_first_root, first_work) = fixture();
    let (_second_root, second_work) = fixture();
    git(&second_work, &["commit", "--amend", "-qm", "distinct tip"]);
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
    assert_eq!(incremental.0.len(), cold.len());
    assert_eq!(incremental.0, cold);
}

#[test]
fn persisted_fast_forward_reuses_existing_repository_when_one_is_added() {
    let (_first_root, first_work) = fixture();
    let (_second_root, second_work) = fixture();
    git(&second_work, &["commit", "--amend", "-qm", "distinct tip"]);
    let first_source = at_head(source(first_work.clone(), "first"), &first_work);
    let second_source = at_head(source(second_work.clone(), "second"), &second_work);
    let first_artifacts = tempdir().expect("first artifact root");
    let first = build_git_history_artifacts_incrementally(
        first_artifacts.path(),
        std::slice::from_ref(&first_source),
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("first history build");
    let first_generation = first_artifacts
        .path()
        .join("generations")
        .join(&first.artifacts.generation);
    let incremental_artifacts = tempdir().expect("incremental artifact root");
    let incremental = build_git_history_artifacts_from_previous(
        incremental_artifacts.path(),
        &[first_source.clone(), second_source.clone()],
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(std::slice::from_ref(&first_source)),
            lexical_path: first_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: None,
            vector_path: None,
            lexical_format_compatible: true,
        }),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("incremental history build");
    let cold_artifacts = tempdir().expect("cold artifact root");
    let cold = build_git_history_artifacts_incrementally(
        cold_artifacts.path(),
        &[first_source, second_source],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("cold history build");
    let incremental_generation = incremental_artifacts
        .path()
        .join("generations")
        .join(&incremental.artifacts.generation);
    let cold_generation = cold_artifacts
        .path()
        .join("generations")
        .join(&cold.artifacts.generation);

    assert!(incremental.reused_snapshot);
    assert_eq!(
        incremental.artifacts.chunk_count,
        cold.artifacts.chunk_count
    );
    assert_cold_equivalent_lexical(
        &incremental_generation.join("lexical.json"),
        &cold_generation.join("lexical.json"),
    );
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

    assert!(
        alpha
            .identifiers
            .iter()
            .any(|identifier| identifier == "alpha_unique")
    );
    assert!(
        !alpha
            .identifiers
            .iter()
            .any(|identifier| identifier == "omega_unique")
    );
    assert!(
        omega
            .identifiers
            .iter()
            .any(|identifier| identifier == "omega_unique")
    );
    assert!(
        !omega
            .identifiers
            .iter()
            .any(|identifier| identifier == "alpha_unique")
    );
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
    assert_eq!(forward.len(), 7);
}

#[test]
fn shared_commits_are_reused_only_after_their_selected_content_was_indexed() {
    let (_root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/shared.rs"), "pub fn shared_signal() {}\n").expect("shared source");
    git(&work, &["add", "limits/shared.rs"]);
    git(&work, &["commit", "-qm", "shared source"]);
    let shared_tip = git(&work, &["rev-parse", "HEAD"]);
    fs::write(work.join("limits/alpha.rs"), "pub fn alpha_signal() {}\n").expect("alpha source");
    git(&work, &["add", "limits/alpha.rs"]);
    git(&work, &["commit", "-qm", "alpha source"]);
    let alpha_tip = git(&work, &["rev-parse", "HEAD"]);
    let bounded = |repository: &str, revision: &str| {
        let mut source = source(work.clone(), repository);
        source.revision = Revision::try_from(revision).expect("revision");
        source.history_tips = vec![source.revision.clone()];
        source.policy.include_prefixes = vec!["limits".into()];
        source.policy.limits = GitCorpusLimits::new(64, 1, 64).expect("valid test limits");
        source
    };

    let (contents, _) = cold_history(&[bounded("alpha", &alpha_tip), bounded("beta", &shared_tip)]);

    assert!(
        contents
            .iter()
            .any(|chunk| chunk.text.contains("shared_signal")),
        "the second source must ingest shared content rejected by the first source's exhausted budget"
    );
}

#[test]
fn shared_blobs_are_reused_only_when_the_stored_locator_is_reachable() {
    let (root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    fs::write(work.join("limits/shared.rs"), "pub fn shared_signal() {}\n").expect("shared source");
    git(&work, &["add", "limits/shared.rs"]);
    git(&work, &["commit", "-qm", "shared source"]);
    let shared_tip = git(&work, &["rev-parse", "HEAD"]);
    fs::write(work.join("limits/alpha.rs"), "pub fn alpha_signal() {}\n").expect("alpha source");
    git(&work, &["add", "limits/alpha.rs"]);
    git(&work, &["commit", "-qm", "alpha source"]);
    let alpha_tip = git(&work, &["rev-parse", "HEAD"]);

    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let alpha = repositories.join("alpha.git");
    let beta = repositories.join("beta.git");
    for bare in [&alpha, &beta] {
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().expect("UTF-8 worktree path"),
                bare.to_str().expect("UTF-8 bare repository path"),
            ],
        );
    }
    let bounded = |repository: &str, path: PathBuf, revision: &str| {
        let mut source = source(path, repository);
        source.revision = Revision::try_from(revision).expect("revision");
        source.history_tips = vec![source.revision.clone()];
        source.policy.include_prefixes = vec!["limits".into()];
        source
    };
    let sources = [
        bounded("alpha", alpha, &alpha_tip),
        bounded("beta", beta, &shared_tip),
    ];
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &sources,
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("beta").expect("repository")),
        ..LexicalFilters::default()
    };

    assert!(
        !index
            .search_candidates("shared_signal", &filters, 10)
            .expect("search beta history")
            .is_empty(),
        "cross-repository deduplication must retain a locator reachable from the filtered repository"
    );
}

#[test]
fn partially_reused_blob_is_not_proof_of_a_reachable_locator() {
    let (root, work) = fixture();
    fs::create_dir(work.join("limits")).expect("limit fixture directory");
    let stable = format!(
        "pub fn shared_chunk_signal() {{}}\n{}\n",
        "let stable_padding = 1;\n".repeat(80)
    );
    fs::write(
        work.join("limits/shared.rs"),
        format!("{stable}{}", "pub fn old_tail_signal() {}\n".repeat(80)),
    )
    .expect("old multi-chunk source");
    git(&work, &["add", "limits/shared.rs"]);
    git(&work, &["commit", "-qm", "shared multi-chunk source"]);
    let shared_tip = git(&work, &["rev-parse", "HEAD"]);
    fs::write(
        work.join("limits/shared.rs"),
        format!("{stable}{}", "pub fn new_tail_signal() {}\n".repeat(80)),
    )
    .expect("new multi-chunk source");
    git(&work, &["commit", "-qam", "change only the final chunks"]);
    let alpha_tip = git(&work, &["rev-parse", "HEAD"]);

    let repositories = root.path().join("repositories");
    fs::create_dir(&repositories).expect("repository directory");
    let alpha = repositories.join("alpha.git");
    let beta = repositories.join("beta.git");
    for bare in [&alpha, &beta] {
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                work.to_str().expect("UTF-8 worktree path"),
                bare.to_str().expect("UTF-8 bare repository path"),
            ],
        );
    }
    let bounded = |repository: &str, path: PathBuf, revision: &str| {
        let mut source = source(path, repository);
        source.revision = Revision::try_from(revision).expect("revision");
        source.history_tips = vec![source.revision.clone()];
        source.policy.include_prefixes = vec!["limits".into()];
        source
    };
    let sources = [
        bounded("alpha", alpha, &alpha_tip),
        bounded("beta", beta, &shared_tip),
    ];
    let artifact_root = root.path().join("artifacts");
    let summary = build_git_history_artifacts_incrementally(
        &artifact_root,
        &sources,
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("history build");
    let lexical_path = artifact_root
        .join("generations")
        .join(summary.artifacts.generation)
        .join("lexical.json");
    let index =
        LexicalIndex::open_git_search_artifact(&lexical_path, &repositories).expect("open index");
    let filters = LexicalFilters {
        repository: Some(RepositoryId::try_from("beta").expect("repository")),
        ..LexicalFilters::default()
    };

    assert!(
        !index
            .search_candidates("shared_chunk_signal", &filters, 10)
            .expect("search beta history")
            .is_empty(),
        "a partially reused blob must be materialized under a locator reachable from beta"
    );
}

#[test]
fn identical_blob_in_unrelated_repository_does_not_require_the_foreign_commit() {
    let root = tempdir().expect("fixture root");
    let mut sources = Vec::new();
    let mut revisions = Vec::new();
    for repository in ["alpha", "beta"] {
        let work = root.path().join(repository);
        fs::create_dir_all(work.join("shared")).expect("source directory");
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "user.email", "fixture@example.invalid"]);
        git(&work, &["config", "user.name", "Fixture"]);
        fs::write(
            work.join("shared/same.rs"),
            "pub fn unrelated_shared_blob_signal() {}\n",
        )
        .expect("shared blob");
        fs::write(work.join(format!("{repository}.txt")), repository).expect("unique source");
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", &format!("{repository} root")]);
        let mut source = at_head(source(work.clone(), repository), &work);
        source.policy.include_prefixes = vec!["shared".into()];
        revisions.push(source.revision.clone());
        sources.push(source);
    }
    let alpha_revision =
        gix::ObjectId::from_hex(revisions[0].as_str().as_bytes()).expect("alpha revision");
    assert!(
        !gix::open(&sources[1].repository_path)
            .expect("open beta")
            .has_object(alpha_revision),
        "the beta object database must not contain alpha's commit"
    );

    let (chunks, _) = cold_history(&sources);
    let repositories = chunks
        .iter()
        .map(|chunk| chunk.repository.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(repositories, BTreeSet::from(["alpha", "beta"]));
}

#[test]
fn rewritten_history_reconciles_the_cache_from_git() {
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

    let (incremental, incremental_refresh) = ingest_git_history_fast_forward(
        std::slice::from_ref(&rewritten),
        &snapshot_tips(&[source]),
        &before,
    )
    .expect("rewritten history check")
    .expect("rewritten history is reconciled incrementally");
    let (cold, refresh) = cold_history(&[rewritten]);

    assert_eq!(incremental, cold);
    assert_eq!(incremental_refresh.reachable_commits, 1);
    assert_eq!(incremental_refresh.ingested_commits, 1);
    assert_eq!(refresh.reachable_commits, 1);
    assert_eq!(refresh.ingested_commits, 1);
    assert_eq!(cold.len(), 2);
}

#[test]
fn rewritten_representative_is_recreated_when_identical_content_remains_reachable() {
    let (_root, work) = fixture();
    git(&work, &["switch", "-q", "-c", "removed", "v1"]);
    fs::write(work.join("shared.c"), "static int retained_signal;\n").expect("removed copy");
    git(&work, &["add", "shared.c"]);
    git(&work, &["commit", "-qm", "removed representative"]);
    let previous = at_head(source(work.clone(), "fixture"), &work);
    let (cached, _) = cold_history(std::slice::from_ref(&previous));
    let previous_root = tempdir().expect("previous artifacts");
    let first = build_git_history_artifacts_incrementally(
        previous_root.path(),
        std::slice::from_ref(&previous),
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("previous persisted history");
    let previous_generation = previous_root
        .path()
        .join("generations")
        .join(&first.artifacts.generation);

    git(&work, &["switch", "-q", "-c", "retained", "v1"]);
    fs::write(work.join("shared.c"), "static int retained_signal;\n").expect("retained copy");
    git(&work, &["add", "shared.c"]);
    git(&work, &["commit", "-qm", "retained representative"]);
    let current = at_head(previous.clone(), &work);

    let (incremental, refresh) = ingest_git_history_fast_forward(
        std::slice::from_ref(&current),
        &snapshot_tips(std::slice::from_ref(&previous)),
        &cached,
    )
    .expect("representative rewrite")
    .expect("representative rewrite is incremental");
    let (cold, _) = cold_history(std::slice::from_ref(&current));

    assert_eq!(incremental, cold);
    assert!(refresh.removed_documents > 0, "{refresh:#?}");
    assert!(
        incremental
            .iter()
            .any(|chunk| chunk.text.contains("retained_signal"))
    );

    let incremental_root = tempdir().expect("incremental artifacts");
    let persisted = build_git_history_artifacts_from_previous(
        incremental_root.path(),
        std::slice::from_ref(&current),
        Some(PreviousGitHistoryArtifact {
            tips: snapshot_tips(std::slice::from_ref(&previous)),
            lexical_path: previous_generation.join("lexical.json"),
            corpus_digest: first.artifacts.manifest.corpus.content_digest,
            vector_checksum: None,
            vector_path: None,
            lexical_format_compatible: true,
        }),
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("persisted representative rewrite");
    let cold_root = tempdir().expect("cold artifacts");
    let cold = build_git_history_artifacts_incrementally(
        cold_root.path(),
        &[current],
        None,
        None,
        None,
        None,
        None,
        &mut ignore_build_phase,
    )
    .expect("cold retained history");
    assert!(persisted.reused_snapshot);
    assert_cold_equivalent_lexical(
        &incremental_root
            .path()
            .join("generations")
            .join(persisted.artifacts.generation)
            .join("lexical.json"),
        &cold_root
            .path()
            .join("generations")
            .join(cold.artifacts.generation)
            .join("lexical.json"),
    );
}

#[test]
fn rewritten_history_drops_cached_commits_missing_from_the_object_database() {
    let (_root, work) = fixture();
    let previous = at_head(source(work.clone(), "fixture"), &work);
    let previous_id =
        gix::ObjectId::from_hex(previous.revision.as_str().as_bytes()).expect("previous object id");
    let (before, _) = cold_history(std::slice::from_ref(&previous));

    git(&work, &["checkout", "-q", "--orphan", "rewritten"]);
    git(&work, &["rm", "-q", "-r", "-f", "."]);
    fs::create_dir_all(work.join("src")).expect("rewritten source directory");
    fs::write(work.join("src/lib.rs"), "pub fn replacement() {}\n").expect("replacement");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "replacement root"]);
    git(&work, &["branch", "-D", "main"]);
    git(&work, &["reflog", "expire", "--expire=now", "--all"]);
    git(&work, &["gc", "--prune=now"]);
    assert!(
        !gix::open(&work)
            .expect("rewritten repository")
            .has_object(previous_id),
        "the replaced commit must actually be absent"
    );
    let rewritten = at_head(previous.clone(), &work);

    let (incremental, refresh) = ingest_git_history_fast_forward(
        std::slice::from_ref(&rewritten),
        &snapshot_tips(&[previous]),
        &before,
    )
    .expect("missing predecessor reconciliation")
    .expect("missing predecessor does not require a cold rebuild");
    let (cold, _) = cold_history(&[rewritten]);

    assert_eq!(incremental, cold);
    assert!(refresh.removed_documents > 0, "{refresh:#?}");
    assert!(refresh.removed_commit_messages > 0, "{refresh:#?}");
}
