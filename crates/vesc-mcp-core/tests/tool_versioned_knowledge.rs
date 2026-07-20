#![cfg(feature = "managed-git")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use vesc_mcp_core::config::McpConfig;
use vesc_mcp_core::managed_repositories::DataRootInputs;
use vesc_mcp_core::test_support::McpTestHarness;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn fixture_remote(root: &Path) -> (PathBuf, String, String) {
    let work = root.join("work");
    let remote = root.join("remote.git");
    fs::create_dir(&work).expect("work tree");
    git(&work, &["init", "-b", "main"]);
    fs::write(work.join("README.md"), "alphaunique old release\n").expect("old source");
    git(&work, &["add", "README.md"]);
    git(
        &work,
        &[
            "-c",
            "user.name=Test Author",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "old release",
        ],
    );
    let old = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["branch", "release_6_06", &old]);
    fs::write(work.join("README.md"), "betaunique refloat tag\n").expect("tagged source");
    git(
        &work,
        &[
            "-c",
            "user.name=Test Author",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-am",
            "refloat tag",
        ],
    );
    let tagged = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["tag", "v1.2.3", &tagged]);
    git(
        &work,
        &[
            "clone",
            "--bare",
            ".",
            remote.to_str().expect("UTF-8 remote"),
        ],
    );
    (remote, old, tagged)
}

fn repository(id: &str) -> String {
    format!(
        r#"
[[knowledge.repositories]]
id = "{id}"
remote_url = "https://example.invalid/{id}.git"
default_ref = "refs/heads/main"
policy = "required"
include = ["**/*.md"]
exclude = []
trust_tier = "official"
license = "MIT"
attribution = "Test fixture"
max_file_bytes = 1048576
max_files = 100
max_total_bytes = 10485760
"#
    )
}

#[tokio::test]
async fn agent_can_list_prepare_search_and_read_an_explicit_snapshot() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let (remote, old, tagged) = fixture_remote(temp.path());
    let data_root = temp.path().join("data");
    let toml = format!(
        "[knowledge]\ndata_root = {:?}\n{}{}{}",
        data_root,
        repository("bldc"),
        repository("refloat"),
        repository("vesc_tool")
    );
    let knowledge = McpConfig::from_toml(&toml, &DataRootInputs::default())
        .expect("knowledge config")
        .knowledge;
    let harness = McpTestHarness::with_knowledge_config(knowledge);
    for id in ["bldc", "refloat", "vesc_tool"] {
        harness
            .sync_managed_source(id, remote.to_str().expect("UTF-8 remote"))
            .await;
    }

    let listed: Value = serde_json::from_str(&harness.call_tool(
        "list_vesc_source_versions",
        json!({"ref_kinds": ["branch", "tag"], "limit": 20}),
    ))
    .expect("list response");
    assert!(listed["ok"].as_bool().unwrap_or_default());

    let selection = json!({
        "sources": {
            "bldc": "refs/heads/release_6_06",
            "vesc_tool": "refs/heads/release_6_06",
            "refloat": "refs/tags/v1.2.3"
        }
    });
    let first: Value = serde_json::from_str(
        &harness
            .call_tool_async("prepare_vesc_knowledge", selection.clone())
            .await,
    )
    .expect("prepare response");
    assert_eq!(first["status"], "built");
    assert_eq!(first["sources"]["bldc"], old);
    assert_eq!(first["sources"]["vesc_tool"], old);
    assert_eq!(first["sources"]["refloat"], tagged);
    let snapshot = first["snapshot_id"].as_str().expect("snapshot ID");

    let second: Value = serde_json::from_str(
        &harness
            .call_tool_async("prepare_vesc_knowledge", selection)
            .await,
    )
    .expect("repeat prepare response");
    assert_eq!(second["snapshot_id"], snapshot);
    assert_eq!(second["status"], "reused");

    let search: Value = serde_json::from_str(&harness.call_tool(
        "search_vesc_knowledge",
        json!({
            "query": "alphaunique",
            "snapshot_id": snapshot,
            "mode": "lexical",
            "detail": "full",
            "limit": 1
        }),
    ))
    .expect("search response");
    assert_eq!(search["index"]["snapshot_id"], snapshot);
    assert_eq!(search["index"]["repositories"]["bldc"], old);
    let uri = search["results"][0]["resource_uri"]
        .as_str()
        .expect("versioned resource URI");
    assert!(harness.read_resource(uri).contains("alphaunique"));

    assert_eq!(
        fs::read_dir(data_root.join("artifacts"))
            .expect("artifact directory")
            .count(),
        1
    );
}
