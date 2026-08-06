use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use vesc_knowledge_index::corpus::git::GitCorpusSource;
use vesc_knowledge_index::corpus::git::{
    GitCorpusLimits, GitCorpusPolicy, GitIngestionError, ingest_git_commit,
};
use vesc_knowledge_index::{
    Chunk, LexicalFilters, LexicalIndex, LicenseStatus, RepositoryId, Revision, TrustTier,
    build_git_artifacts, embedded_entries,
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

fn manage_repository(root: &Path, id: &str, source: &Path) -> PathBuf {
    let repositories = root.join("repositories");
    fs::create_dir_all(&repositories).expect("managed repository root");
    let target = repositories.join(format!("{id}.git"));
    fs::rename(source, &target).expect("move bare repository into managed layout");
    target
}

fn bare_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let root = tempdir().expect("fixture root");
    let work = root.path().join("work");
    let bare = root.path().join("fixture.git");
    fs::create_dir(&work).expect("worktree");
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);
    fs::create_dir(work.join("imu")).expect("imu directory");
    fs::write(
        work.join("imu/imu.c"),
        "// IMU sample-rate sensor read timing loop\n// error: IMU sample timeout\nstatic void imu_read_callback(float *accel, float *gyro) {\n    static unsigned last_time;\n    float dt = timer_seconds_elapsed_since(last_time);\n    last_time = timer_time_now();\n    switch (m_settings.mode) {\n    case AHRS_MODE_MADGWICK: ahrs_update_madgwick_imu(gyro, accel, dt, &m_att); break;\n    case AHRS_MODE_MAHONY: ahrs_update_mahony_imu(gyro, accel, dt, &m_att); break;\n    }\n}\n",
    )
    .expect("imu source");
    fs::write(
        work.join("imu/imu.h"),
        "#define IMU_SAMPLE_RATE_HZ 1000\nvoid imu_set_read_callback(void (*callback)(float *, float *));\n",
    )
    .expect("imu configuration header");
    fs::write(
        work.join("imu/ahrs.c"),
        "void ahrs_apply_mahony_feedback(float dt) { integrate_attitude_error(dt); }\n",
    )
    .expect("AHRS source");
    fs::write(
        work.join("imu/sensor.c"),
        "void bmi160_read_accel_gyro(float *accel, float *gyro) { sensor_bus_read(accel, gyro); }\n",
    )
    .expect("sensor source");
    fs::write(work.join("firmware.bin"), [0_u8, 1, 2, 3]).expect("binary");
    fs::write(work.join("binary.c"), [0_u8, 1, 2, 3]).expect("disguised binary");
    fs::write(work.join("invalid.c"), [0xff_u8, 0xfe]).expect("invalid UTF-8");
    #[cfg(unix)]
    std::os::unix::fs::symlink("imu/imu.c", work.join("imu-link.c")).expect("symlink");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "fixture"]);
    let parent = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{parent},nested-repository"),
        ],
    );
    git(&work, &["commit", "-qm", "record Gitlink metadata"]);
    let revision = git(&work, &["rev-parse", "HEAD"]);
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "-q",
            work.to_str().expect("utf8 worktree"),
            bare.to_str().expect("utf8 bare path"),
        ],
    );
    (root, work, bare, revision)
}

fn single_file_bare(path: &str, content: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let root = tempdir().expect("fixture root");
    let work = root.path().join("work");
    let bare = root.path().join("fixture.git");
    fs::create_dir(&work).expect("worktree");
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);
    let file = work.join(path);
    fs::create_dir_all(file.parent().expect("file parent")).expect("source directory");
    fs::write(&file, content).expect("source file");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "fixture"]);
    let revision = git(&work, &["rev-parse", "HEAD"]);
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "-q",
            work.to_str().expect("utf8 worktree"),
            bare.to_str().expect("utf8 bare path"),
        ],
    );
    (root, bare, revision)
}

fn document_at<'a>(
    report: &'a vesc_knowledge_index::corpus::ingest::IngestionReport,
    path: &str,
) -> &'a vesc_knowledge_index::NormalizedDocument {
    report
        .documents
        .iter()
        .find(|document| document.path == path)
        .expect("document path")
}

#[test]
fn excluded_directories_still_admit_the_mandatory_code_floor() {
    let (root, bare, _revision) = single_file_bare(
        "generated/control.cpp",
        "int generated_control(void) { return 1; }\n",
    );
    let work = root.path().join("extra-work");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            bare.to_str().expect("UTF-8 bare path"),
            work.to_str().expect("UTF-8 worktree path"),
        ],
    );
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);
    fs::write(work.join("generated/notes.md"), "excluded prose\n").expect("excluded prose");
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "add excluded prose"]);
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().expect("UTF-8 bare path"),
            "HEAD:refs/heads/main",
        ],
    );
    let revision = git(&work, &["rev-parse", "HEAD"]);
    let policy = GitCorpusPolicy {
        exclude_patterns: vec!["generated/**".into()],
        ..GitCorpusPolicy::default()
    };

    let report = ingest_git_commit(
        &bare,
        &RepositoryId::try_from("vesc").expect("repository"),
        &Revision::try_from(revision).expect("revision"),
        TrustTier::CuratedUpstream,
        &LicenseStatus::ReferenceOnly,
        &policy,
    )
    .expect("ingest excluded directory");

    assert!(report.documents.iter().any(|document| {
        document.path == "generated/control.cpp" && document.content.contains("generated_control")
    }));
    assert!(
        report
            .documents
            .iter()
            .all(|document| document.path != "generated/notes.md")
    );
}

#[test]
fn bare_commit_ingestion_yields_bounded_code_with_exact_provenance() {
    let (_root, _work, bare, revision) = bare_fixture();
    let report = ingest_git_commit(
        &bare,
        &RepositoryId::try_from("vesc").expect("repository"),
        &Revision::try_from(revision.as_str()).expect("revision"),
        TrustTier::CuratedUpstream,
        &LicenseStatus::Redistributable {
            spdx: "GPL-3.0-only".into(),
        },
        &GitCorpusPolicy::default(),
    )
    .expect("ingest bare commit");

    assert_eq!(report.documents.len(), 4);
    assert_eq!(report.visited_files, 9);
    let document = document_at(&report, "imu/imu.c");
    assert_eq!(document.path, "imu/imu.c");
    assert_eq!(document.revision.as_str(), revision);
    assert!(document.identifiers.contains("imu_read_callback"));
    assert_eq!(document.source_span.expect("source span").start_line, 1);
    assert_eq!(document.source_span.expect("source span").end_line, 11);
    assert_eq!(
        document.source_span.expect("source span").start_byte,
        Some(0)
    );
    assert_eq!(
        document.source_span.expect("source span").end_byte,
        Some(document.content.len() as u64)
    );
    assert!(
        report
            .rejected
            .iter()
            .any(|item| item.code == "unsupported")
    );
    assert!(report.rejected.iter().any(|item| item.code == "encoding"));
    assert!(report.rejected.iter().any(|item| item.code == "binary"));
    let observations = report.git_observations.as_ref().expect("Git observations");
    assert_eq!(observations.candidate_count, 6);
    assert!(observations.blob_bytes_loaded > 0);
    assert_eq!(observations.binary_rejection_count, 1);
    assert_eq!(observations.encoding_rejection_count, 1);
    assert!(
        report
            .rejected
            .iter()
            .any(|item| item.source == "nested-repository")
    );
    assert!(
        !bare.join("imu").exists(),
        "bare repository has no checkout"
    );
}

#[test]
fn git_tree_rejections_keep_a_bounded_sample_and_exact_count() {
    let root = tempdir().expect("fixture root");
    let work = root.path().join("work");
    let bare = root.path().join("fixture.git");
    fs::create_dir_all(work.join("src")).expect("source directory");
    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "fixture@example.invalid"]);
    git(&work, &["config", "user.name", "Fixture"]);
    for index in 0..100 {
        fs::write(
            work.join(format!("src/item-{index:03}.rs")),
            format!("pub fn item_{index:03}() {{}}\n"),
        )
        .expect("source file");
    }
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "many sources"]);
    let revision = git(&work, &["rev-parse", "HEAD"]);
    git(
        root.path(),
        &[
            "clone",
            "--bare",
            "-q",
            work.to_str().expect("UTF-8 worktree"),
            bare.to_str().expect("UTF-8 bare path"),
        ],
    );
    let policy = GitCorpusPolicy {
        limits: GitCorpusLimits::new(1_024, 1, 1_024).expect("valid limits"),
        ..GitCorpusPolicy::default()
    };

    let report = ingest_git_commit(
        &bare,
        &RepositoryId::try_from("vesc").expect("repository"),
        &Revision::try_from(revision).expect("revision"),
        TrustTier::CuratedUpstream,
        &LicenseStatus::ReferenceOnly,
        &policy,
    )
    .expect("ingest bounded tree");
    let observations = report.git_observations.as_ref().expect("Git observations");

    assert_eq!(report.documents.len(), 1);
    assert_eq!(observations.rejection_count, 99);
    assert_eq!(report.rejected.len(), 64);
}

#[test]
fn same_commit_and_policy_are_deterministic() {
    let (_root, _work, bare, revision) = bare_fixture();
    let ingest = || {
        ingest_git_commit(
            &bare,
            &RepositoryId::try_from("vesc").expect("repository"),
            &Revision::try_from(revision.as_str()).expect("revision"),
            TrustTier::CuratedUpstream,
            &LicenseStatus::ReferenceOnly,
            &GitCorpusPolicy::default(),
        )
        .expect("ingest")
    };

    assert_eq!(ingest(), ingest());

    let source = GitCorpusSource {
        repository_path: bare.clone(),
        repository_id: RepositoryId::try_from("vesc").expect("repository"),
        revision: Revision::try_from(revision.as_str()).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    };
    let first_root = tempdir().expect("first artifact root");
    let second_root = tempdir().expect("second artifact root");
    let first =
        build_git_artifacts(first_root.path(), std::slice::from_ref(&source)).expect("first build");
    let second = build_git_artifacts(second_root.path(), &[source]).expect("second build");
    assert_eq!(first.manifest, second.manifest);
    let artifact = |root: &Path, generation: &str| {
        fs::read(
            root.join("generations")
                .join(generation)
                .join("lexical.json"),
        )
        .expect("lexical artifact")
    };
    assert_eq!(
        artifact(first_root.path(), &first.generation),
        artifact(second_root.path(), &second.generation)
    );
    assert!(
        !serde_json::to_string(&first.manifest)
            .expect("manifest JSON")
            .contains(bare.to_str().expect("UTF-8 fixture path"))
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One end-to-end revision provenance scenario.
fn exact_commits_produce_revision_correct_content_and_ids() {
    let (_root, work, bare, first_revision) = bare_fixture();
    fs::write(
        work.join("imu/imu.c"),
        "static void imu_read_callback(float dt) {\n    ahrs_update_madgwick_imu(dt);\n}\n",
    )
    .expect("updated IMU source");
    git(&work, &["add", "imu/imu.c"]);
    git(&work, &["commit", "-qm", "update IMU"]);
    let second_revision = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().expect("bare path"),
            "HEAD:refs/heads/updated",
        ],
    );
    let repo = RepositoryId::try_from("vesc").expect("repository");
    let policy = GitCorpusPolicy::default();
    let first = ingest_git_commit(
        &bare,
        &repo,
        &Revision::try_from(first_revision.as_str()).expect("first revision"),
        TrustTier::CuratedUpstream,
        &LicenseStatus::ReferenceOnly,
        &policy,
    )
    .expect("first commit");
    let second = ingest_git_commit(
        &bare,
        &repo,
        &Revision::try_from(second_revision.as_str()).expect("second revision"),
        TrustTier::CuratedUpstream,
        &LicenseStatus::ReferenceOnly,
        &policy,
    )
    .expect("second commit");

    let first_imu = document_at(&first, "imu/imu.c");
    let second_imu = document_at(&second, "imu/imu.c");
    assert!(first_imu.content.contains("mahony"));
    assert!(second_imu.content.contains("madgwick"));
    assert_ne!(first_imu.document_id, second_imu.document_id);
    assert_eq!(second_imu.revision.as_str(), second_revision);

    let (_refloat_root, refloat_bare, refloat_revision) =
        single_file_bare("src/main.c", "void unchanged_refloat_source(void) {}\n");
    let managed = tempdir().expect("managed repository root");
    let bare = manage_repository(managed.path(), "vesc", &bare);
    let refloat_bare = manage_repository(managed.path(), "refloat", &refloat_bare);
    let source = |path: std::path::PathBuf, repository: &str, revision: &str| GitCorpusSource {
        repository_path: path,
        repository_id: RepositoryId::try_from(repository).expect("repository"),
        revision: Revision::try_from(revision).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    };
    let first_root = tempdir().expect("first combined artifact");
    let second_root = tempdir().expect("second combined artifact");
    let first_summary = build_git_artifacts(
        first_root.path(),
        &[
            source(bare.clone(), "vesc", &first_revision),
            source(refloat_bare.clone(), "refloat", &refloat_revision),
        ],
    )
    .expect("first combined build");
    let second_summary = build_git_artifacts(
        second_root.path(),
        &[
            source(bare, "vesc", &second_revision),
            source(refloat_bare, "refloat", &refloat_revision),
        ],
    )
    .expect("second combined build");
    let read = |root: &Path, generation: &str| {
        LexicalIndex::read_git_artifact_chunks(
            &root
                .join("generations")
                .join(generation)
                .join("lexical.json"),
            &managed.path().join("repositories"),
        )
        .expect("combined lexical artifact")
    };
    let first_chunks = read(first_root.path(), &first_summary.generation);
    let second_chunks = read(second_root.path(), &second_summary.generation);
    let identities = |chunks: &[Chunk], repository: &str| {
        chunks
            .iter()
            .filter(|chunk| chunk.repository.as_str() == repository)
            .map(|chunk| (chunk.chunk_id.clone(), chunk.text.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        identities(&first_chunks, "refloat"),
        identities(&second_chunks, "refloat")
    );
    assert_ne!(
        identities(&first_chunks, "vesc"),
        identities(&second_chunks, "vesc")
    );
}

#[test]
fn configured_path_filters_are_enforced() {
    let (_root, _work, bare, revision) = bare_fixture();
    let repo = RepositoryId::try_from("vesc").expect("repository");
    let revision = Revision::try_from(revision.as_str()).expect("revision");
    let ingest = |policy: &GitCorpusPolicy| {
        ingest_git_commit(
            &bare,
            &repo,
            &revision,
            TrustTier::CuratedUpstream,
            &LicenseStatus::ReferenceOnly,
            policy,
        )
    };
    let only_docs = GitCorpusPolicy {
        include_prefixes: vec!["docs".into()],
        ..GitCorpusPolicy::default()
    };
    assert!(
        ingest(&only_docs)
            .expect("filtered corpus")
            .documents
            .iter()
            .all(|document| {
                matches!(
                    std::path::Path::new(&document.path)
                        .extension()
                        .and_then(|extension| extension.to_str()),
                    Some("c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" | "inc" | "inl")
                )
            })
    );

    for prefix in ["", "../imu", "/imu", "imu/../docs"] {
        let escaped = GitCorpusPolicy {
            include_prefixes: vec![prefix.into()],
            ..GitCorpusPolicy::default()
        };
        assert!(matches!(
            ingest(&escaped),
            Err(GitIngestionError::InvalidPolicy(_))
        ));
    }
}

#[test]
fn ingestion_requires_an_existing_commit_object() {
    let (_root, work, bare, _revision) = bare_fixture();
    let tree = git(&work, &["rev-parse", concat!("HEAD^", "{tree}")]);
    let ingest = |revision: &str| {
        ingest_git_commit(
            &bare,
            &RepositoryId::try_from("vesc").expect("repository"),
            &Revision::try_from(revision).expect("revision"),
            TrustTier::CuratedUpstream,
            &LicenseStatus::ReferenceOnly,
            &GitCorpusPolicy::default(),
        )
    };

    assert!(matches!(
        ingest(&tree),
        Err(GitIngestionError::InvalidCommit(_))
    ));
    assert!(matches!(
        ingest("0000000000000000000000000000000000000000"),
        Err(GitIngestionError::InvalidCommit(_))
    ));
}

#[test]
fn git_artifact_is_additive_and_searches_symbols_paths_and_concepts() {
    let (_root, _work, bare, revision) = bare_fixture();
    let artifacts = tempdir().expect("artifact root");
    let managed = tempdir().expect("managed repository root");
    let bare = manage_repository(managed.path(), "vesc", &bare);
    let source = GitCorpusSource {
        repository_path: bare,
        repository_id: RepositoryId::try_from("vesc").expect("repository"),
        revision: Revision::try_from(revision.as_str()).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    };
    let summary = build_git_artifacts(artifacts.path(), &[source]).expect("build Git corpus");
    assert!(
        summary.document_count > 94,
        "catalog corpus remains additive"
    );
    assert_eq!(summary.manifest.sources.len(), 4);
    assert_eq!(
        summary.manifest.component_versions["git-policy"],
        "reviewed-v4"
    );
    let lexical = LexicalIndex::open_git_search_artifact(
        &artifacts
            .path()
            .join("generations")
            .join(&summary.generation)
            .join("lexical.json"),
        &managed.path().join("repositories"),
    )
    .expect("open lexical artifact");
    assert!(lexical.schema().get_field("path").is_ok());
    let search = |query| {
        lexical
            .search(query, &LexicalFilters::default(), 5)
            .expect("search")
    };

    let symbol = search("imu_read_callback");
    assert_eq!(symbol[0].chunk.path, "imu/imu.c");
    assert!(symbol[0].exact_identifier);
    assert_eq!(search("imu/imu.c")[0].chunk.path, "imu/imu.c");
    assert_eq!(search("IMU sample timeout")[0].chunk.path, "imu/imu.c");
    assert_eq!(search("IMU_SAMPLE_RATE_HZ")[0].chunk.path, "imu/imu.h");
    assert_eq!(
        search("ahrs_apply_mahony_feedback")[0].chunk.path,
        "imu/ahrs.c"
    );
    assert_eq!(
        search("bmi160_read_accel_gyro")[0].chunk.path,
        "imu/sensor.c"
    );
    assert_eq!(
        search("IMU sample timing Mahony")[0].chunk.path,
        "imu/imu.c"
    );
    assert_eq!(
        search("How does the IMU loop timing work sample rate measured dt sensor read Mahony Madgwick AHRS update")[0]
            .chunk
            .path,
        "imu/imu.c"
    );
    assert_eq!(
        lexical
            .search(
                "imu_read_callback",
                &LexicalFilters {
                    repository: Some(RepositoryId::try_from("vesc").expect("repository")),
                    revision: Some(Revision::try_from(revision.as_str()).expect("revision")),
                    ..LexicalFilters::default()
                },
                5,
            )
            .expect("revision-filtered search")[0]
            .chunk
            .path,
        "imu/imu.c"
    );
}

#[test]
fn symbol_question_returns_definitions_and_caller_from_bounded_context() {
    let (root, work, bare, _) = bare_fixture();
    fs::write(
        work.join("motor.rs"),
        "pub trait MotorControlBindings {\n    fn update_pid_position_offset(&self, position: PidPosition);\n}\n\nimpl MotorControlBindings for RealMotor {\n    fn update_pid_position_offset(&self, position: PidPosition) {\n        self.apply_position(position);\n    }\n}\n",
    )
    .expect("motor implementation");
    fs::write(
        work.join("motor_control.rs"),
        "pub fn set_position(bindings: &impl MotorControlBindings, position: PidPosition) {\n    bindings.update_pid_position_offset(position);\n}\n",
    )
    .expect("motor caller");
    git(&work, &["add", "motor.rs", "motor_control.rs"]);
    git(
        &work,
        &["commit", "-qm", "add PID position binding and caller"],
    );
    let revision = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().expect("UTF-8 bare path"),
            "HEAD:refs/heads/main",
        ],
    );

    let artifacts = tempdir().expect("artifact root");
    let managed = tempdir().expect("managed repository root");
    let bare = manage_repository(managed.path(), "vesc", &bare);
    let source = GitCorpusSource {
        repository_path: bare,
        repository_id: RepositoryId::try_from("vesc").expect("repository"),
        revision: Revision::try_from(revision.as_str()).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    };
    let summary = build_git_artifacts(artifacts.path(), &[source]).expect("build Git corpus");
    let lexical = LexicalIndex::open_git_search_artifact(
        &artifacts
            .path()
            .join("generations")
            .join(&summary.generation)
            .join("lexical.json"),
        &managed.path().join("repositories"),
    )
    .expect("open lexical artifact");
    let hits = lexical
        .search(
            "update_pid_position_offset trait MotorControlBindings impl MotorControlBindings PidPosition",
            &LexicalFilters::default(),
            5,
        )
        .expect("symbol search");
    let paths = hits
        .iter()
        .map(|hit| hit.chunk.path.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(hits[0].chunk.path, "motor.rs");
    assert!(hits[0].exact_identifier);
    assert!(paths.contains("motor_control.rs"));
    assert!(paths.contains("motor.rs"));
    drop(root);
}

#[test]
fn git_artifact_counts_a_multi_chunk_document_once() {
    let content = format!(
        "# First\n\n{}\n\n# Second\n\n{}\n",
        "alpha passage ".repeat(200),
        "beta passage ".repeat(200)
    );
    let (_root, bare, revision) = single_file_bare("README.md", &content);
    let artifacts = tempdir().expect("artifact root");
    let managed = tempdir().expect("managed repository root");
    let bare = manage_repository(managed.path(), "vesc", &bare);
    let source = GitCorpusSource {
        repository_path: bare,
        repository_id: RepositoryId::try_from("vesc").expect("repository"),
        revision: Revision::try_from(revision).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    };

    let summary = build_git_artifacts(artifacts.path(), &[source]).expect("build Git corpus");

    assert_eq!(summary.document_count, embedded_entries().len() + 1);
    assert!(summary.chunk_count > summary.document_count);
}

#[test]
fn persisted_git_hydration_enforces_the_artifact_file_limit() {
    let (_root, bare, revision) = single_file_bare(
        "src/lib.rs",
        "pub fn bounded_hydration_signal() -> bool { true }\n",
    );
    let artifacts = tempdir().expect("artifact root");
    let managed = tempdir().expect("managed repository root");
    let bare = manage_repository(managed.path(), "vesc", &bare);
    let policy = GitCorpusPolicy {
        limits: GitCorpusLimits::new(1_024, 10, 1_024).expect("valid limits"),
        ..GitCorpusPolicy::default()
    };
    let source = GitCorpusSource {
        repository_path: bare,
        repository_id: RepositoryId::try_from("vesc").expect("repository"),
        revision: Revision::try_from(revision).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy,
    };
    let summary = build_git_artifacts(artifacts.path(), &[source]).expect("build Git corpus");
    let lexical_path = artifacts
        .path()
        .join("generations")
        .join(summary.generation)
        .join("lexical.json");
    let mut descriptor: serde_json::Value =
        serde_json::from_slice(&fs::read(&lexical_path).expect("read descriptor"))
            .expect("parse descriptor");
    assert_eq!(descriptor["git_sources"]["vesc"]["max_file_bytes"], 1_024);
    descriptor["git_sources"]["vesc"]["max_file_bytes"] = serde_json::json!(1);
    fs::write(
        &lexical_path,
        serde_json::to_vec(&descriptor).expect("encode descriptor"),
    )
    .expect("tighten persisted limit");
    let lexical =
        LexicalIndex::open_git_search_artifact(&lexical_path, &managed.path().join("repositories"))
            .expect("open lexical artifact");

    let error = lexical
        .search("bounded_hydration_signal", &LexicalFilters::default(), 1)
        .expect_err("oversized Git blob must be rejected before hydration");
    assert!(
        error
            .to_string()
            .contains("exceeds the configured per-file byte limit")
    );
}

#[test]
fn judged_queries_cover_vesc_vesc_tool_and_refloat_sources() {
    let mut vesc = single_file_bare(
        "motor/mcpwm_foc.c",
        "void timer_update(void) { foc_current_control(); }\n",
    );
    let mut tool = single_file_bare(
        "commands/packagemanager.cpp",
        "void packVescPackage() { serializePackageManifest(); }\n",
    );
    let mut refloat = single_file_bare(
        "src/main.c",
        "INIT_FUN(init_refloat) { lbm_add_extension(\"get-imu\", ext_get_imu); }\n",
    );
    let managed = tempdir().expect("managed repository root");
    vesc.1 = manage_repository(managed.path(), "vesc", &vesc.1);
    tool.1 = manage_repository(managed.path(), "vesc-tool", &tool.1);
    refloat.1 = manage_repository(managed.path(), "refloat", &refloat.1);
    let fixtures = [
        (&vesc.1, "vesc", &vesc.2),
        (&tool.1, "vesc-tool", &tool.2),
        (&refloat.1, "refloat", &refloat.2),
    ];
    let sources = fixtures.map(|(path, repository, revision)| GitCorpusSource {
        repository_path: path.clone(),
        repository_id: RepositoryId::try_from(repository).expect("repository"),
        revision: Revision::try_from(revision.as_str()).expect("revision"),
        history_tips: Vec::new(),
        trust_tier: TrustTier::CuratedUpstream,
        license: LicenseStatus::ReferenceOnly,
        policy: GitCorpusPolicy::default(),
    });
    let artifacts = tempdir().expect("artifact root");
    let summary = build_git_artifacts(artifacts.path(), &sources).expect("build judged corpus");
    let lexical = LexicalIndex::open_git_search_artifact(
        &artifacts
            .path()
            .join("generations")
            .join(summary.generation)
            .join("lexical.json"),
        &managed.path().join("repositories"),
    )
    .expect("open lexical artifact");

    for (query, repository, path) in [
        ("foc_current_control", "vesc", "motor/mcpwm_foc.c"),
        (
            "packVescPackage",
            "vesc-tool",
            "commands/packagemanager.cpp",
        ),
        ("init_refloat", "refloat", "src/main.c"),
    ] {
        let result = lexical
            .search(
                query,
                &LexicalFilters {
                    repository: Some(RepositoryId::try_from(repository).expect("repository")),
                    ..LexicalFilters::default()
                },
                1,
            )
            .expect("judged search");
        assert_eq!(result[0].chunk.path, path);
        assert!(result[0].exact_identifier);
    }
}
