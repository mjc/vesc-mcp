#![cfg(feature = "git-corpus")]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::git::GitCorpusPolicy;
use vesc_knowledge_index::corpus::history::{
    ChangeKind, EmbeddingContract, HistoryVectorIndex, TaggedHistorySource, ingest_tagged_history,
};
use vesc_knowledge_index::{
    ChunkingConfig, ContentDigest, EmbeddingBatchSize, EmbeddingError, EmbeddingProfile,
    EmbeddingProvider, JINA_CODE_FP16_SHA256, JINA_CODE_INT8_SHA256, JINA_CODE_MODEL_ID,
    JINA_CODE_MODEL_REVISION, LicenseStatus, RepositoryId, TrustTier,
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

fn tagged_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempdir().expect("fixture root");
    let work = root.path().join("work");
    fs::create_dir(&work).expect("worktree");
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);

    fs::create_dir(work.join("src")).expect("source directory");
    fs::write(
        work.join("src/control.c"),
        "void alpha_control(void) { run_alpha(); }\n",
    )
    .expect("v1 source");
    fs::write(
        work.join("src/stable.h"),
        "#define STABLE_CONFIGURATION 1\n",
    )
    .expect("unchanged source");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "v1"]);
    git(&work, &["tag", "v1"]);
    git(&work, &["tag", "stable-1"]);

    fs::write(
        work.join("src/control.c"),
        "void alpha_control(void) { run_alpha(); }\nvoid beta_control(void) { run_beta(); }\n",
    )
    .expect("v2 source");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "v2"]);
    git(&work, &["tag", "v2"]);

    git(&work, &["mv", "src/control.c", "src/motor_control.c"]);
    git(&work, &["commit", "-qm", "v3"]);
    git(&work, &["tag", "v3"]);

    fs::remove_file(work.join("src/motor_control.c")).expect("remove source");
    git(&work, &["add", "-u"]);
    git(&work, &["commit", "-qm", "v4"]);
    git(&work, &["tag", "v4"]);

    (root, work)
}

fn source(repository_path: std::path::PathBuf) -> TaggedHistorySource {
    TaggedHistorySource {
        repository_path,
        repository_id: RepositoryId::try_from("fixture").expect("repository"),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
        chunking: ChunkingConfig::default(),
    }
}

#[test]
fn tagged_history_preserves_aliases_and_version_change_evidence() {
    let (root, work) = tagged_fixture();
    let history = ingest_tagged_history(&source(work)).expect("tagged history");

    assert_eq!(history.releases.len(), 4);
    assert_eq!(
        history.release_for_tag("stable-1"),
        history.release_for_tag("v1")
    );
    assert_eq!(
        history.tags_for_identifier("alpha_control"),
        vec!["stable-1", "v1", "v2", "v3"]
    );
    assert_eq!(history.first_seen("beta_control"), Some("v2"));
    assert_eq!(history.last_seen("beta_control"), Some("v3"));

    let beta_changes = history.changes_for_identifier("beta_control");
    assert!(
        beta_changes
            .iter()
            .any(|change| change.kind == ChangeKind::Modified && change.to_tags == ["v2"])
    );
    assert!(
        beta_changes
            .iter()
            .any(|change| change.kind == ChangeKind::Moved && change.to_tags == ["v3"])
    );
    assert!(
        beta_changes
            .iter()
            .any(|change| change.kind == ChangeKind::Removed && change.to_tags == ["v4"])
    );
    assert!(
        beta_changes
            .iter()
            .all(|change| !change.evidence.is_empty())
    );
    assert_eq!(history.changes_between("v1", "v2").len(), 1);
    assert_eq!(history.changes_in_tag("v3").len(), 1);

    let artifact = root.path().join("history.json");
    history.write_artifact(&artifact).expect("write history");
    assert_eq!(
        vesc_knowledge_index::TaggedHistory::read_artifact(&artifact).expect("read history"),
        history
    );
}

#[test]
fn history_uses_remote_release_branches_when_repository_has_no_tags() {
    let root = tempdir().expect("fixture root");
    let work = root.path().join("work");
    fs::create_dir(&work).expect("worktree");
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);

    fs::write(work.join("release.c"), "void release_one(void) {}\n").expect("v1 source");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "release 1"]);
    git(
        &work,
        &["update-ref", "refs/remotes/origin/release_1_00", "HEAD"],
    );

    fs::write(work.join("release.c"), "void release_two(void) {}\n").expect("v2 source");
    git(&work, &["commit", "-qam", "release 2"]);
    git(
        &work,
        &["update-ref", "refs/remotes/origin/release_2_00", "HEAD"],
    );

    let mut source = source(work);
    source.chunking = ChunkingConfig {
        target_chars: 10,
        hard_max_chars: 10,
        minimum_chars: 1,
        overlap_chars: 0,
    };
    let history = ingest_tagged_history(&source).expect("release branch history");

    assert_eq!(history.observations.tag_count, 2);
    assert_eq!(history.releases.len(), 2);
    assert!(history.release_for_tag("release_1_00").is_some());
    assert!(history.release_for_tag("release_2_00").is_some());
}

#[derive(Default)]
struct CountingProvider {
    inputs: usize,
    fail_after: Option<usize>,
}

impl EmbeddingProvider for CountingProvider {
    fn embedding_batch_size(&self) -> EmbeddingBatchSize {
        EmbeddingBatchSize::new(1).expect("one input per crash-safe cache batch")
    }

    fn embed_documents(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if self.fail_after.is_some_and(|limit| self.inputs >= limit) {
            return Err(EmbeddingError::Provider("simulated interruption".into()));
        }
        self.inputs += texts.len();
        Ok(texts.iter().map(|_| vec![1.0, 2.0]).collect())
    }

    fn embed_query(&mut self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(vec![1.0, 1.0])
    }
}

fn contract() -> EmbeddingContract {
    let mut profile = EmbeddingProfile::jina_v2_base_code();
    profile.dimension = 2;
    EmbeddingContract {
        schema: 1,
        model_id: "fixture-model".into(),
        model_revision: "fixture-revision".into(),
        model_digest: ContentDigest::of(b"fixture model"),
        tokenizer_digest: ContentDigest::of(b"fixture tokenizer"),
        profile,
        windowing: "lossless-v1".into(),
        embedding_text_version: 1,
    }
}

fn parsed_digest(hex: &str) -> ContentDigest {
    serde_json::from_str(&format!("\"sha256:{hex}\"")).expect("valid digest")
}

#[test]
fn fp16_jina_history_accepts_the_pinned_int8_cpu_query_model() {
    let mut artifact = contract();
    artifact.model_id = JINA_CODE_MODEL_ID.into();
    artifact.model_revision = JINA_CODE_MODEL_REVISION.into();
    artifact.model_digest = parsed_digest(JINA_CODE_FP16_SHA256);
    artifact.profile.max_length = 64;
    let mut query = artifact.clone();
    query.model_digest = parsed_digest(JINA_CODE_INT8_SHA256);
    query.profile.max_length = 512;

    assert!(artifact.supports_query(&query));
    query.tokenizer_digest = ContentDigest::of(b"different tokenizer");
    assert!(!artifact.supports_query(&query));
}

#[test]
fn history_vectors_embed_unique_inputs_once_and_resume_from_cache() {
    let (_root, work) = tagged_fixture();
    let history = ingest_tagged_history(&source(work)).expect("tagged history");
    let cache = tempdir().expect("cache root");

    assert!(history.observations.git.blob_cache_hits > 0);

    let mut interrupted_provider = CountingProvider {
        fail_after: Some(1),
        ..CountingProvider::default()
    };
    assert!(
        HistoryVectorIndex::build_with_cache(
            &mut interrupted_provider,
            &history,
            contract(),
            cache.path(),
        )
        .is_err()
    );
    assert_eq!(interrupted_provider.inputs, 1);

    let mut first_provider = CountingProvider::default();
    let (first, first_observations) = HistoryVectorIndex::build_with_cache(
        &mut first_provider,
        &history,
        contract(),
        cache.path(),
    )
    .expect("first vector build");
    assert_eq!(first_provider.inputs, history.contents.len() - 1);
    assert_eq!(first.unique_vector_count(), history.contents.len());
    assert!(first.occurrence_count() > first.unique_vector_count());
    assert_eq!(first_observations.cache_hits, 1);
    let hits = first
        .search(&[0.5, 0.5], &contract(), Some("stable-1"), 10)
        .expect("version-filtered semantic search");
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| hit.occurrence.tag == "stable-1"));

    let mut second_provider = CountingProvider::default();
    let (second, second_observations) = HistoryVectorIndex::build_with_cache(
        &mut second_provider,
        &history,
        contract(),
        cache.path(),
    )
    .expect("resumed vector build");
    assert_eq!(second_provider.inputs, 0);
    assert_eq!(second_observations.cache_hits, history.contents.len());
    assert_eq!(first, second);

    let artifact = cache.path().join("history-vectors.json");
    second.write_artifact(&artifact).expect("write artifact");
    assert_eq!(
        HistoryVectorIndex::read_artifact(&artifact).expect("read artifact"),
        second
    );
}

#[test]
fn history_search_rejects_an_incompatible_query_contract() {
    let (_root, work) = tagged_fixture();
    let history = ingest_tagged_history(&source(work)).expect("tagged history");
    let cache = tempdir().expect("cache root");
    let mut provider = CountingProvider::default();
    let (index, _) =
        HistoryVectorIndex::build_with_cache(&mut provider, &history, contract(), cache.path())
            .expect("vector build");
    let mut incompatible = contract();
    incompatible.tokenizer_digest = ContentDigest::of(b"another tokenizer");

    assert!(index.search(&[0.5, 0.5], &incompatible, None, 10).is_err());
}
