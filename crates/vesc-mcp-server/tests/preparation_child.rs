use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;
use vesc_mcp_core::config::{KnowledgeConfig, VESC_MCP_CONFIG_ENV};
use vesc_mcp_core::managed_repositories::DataRoot;
use vesc_mcp_core::tools::prepare_knowledge::{
    PREPARE_KNOWLEDGE_CHILD_ARG, PrepareKnowledgeChildRequest, PrepareVescKnowledgeParams,
};

fn test_config() -> (tempfile::TempDir, std::path::PathBuf, KnowledgeConfig) {
    let temp = tempfile::tempdir().expect("temporary config directory");
    let data_root = temp.path().join("data");
    let config_path = temp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[knowledge]\ndata_root = {:?}\n",
            data_root.to_string_lossy()
        ),
    )
    .expect("write test configuration");
    let config = KnowledgeConfig {
        data_root: Some(DataRoot::new(data_root).expect("absolute data root")),
        ..KnowledgeConfig::default()
    };
    (temp, config_path, config)
}

fn run_child(request: &PrepareKnowledgeChildRequest, config_path: &std::path::Path) -> Value {
    let server = std::env::var("CARGO_BIN_EXE_vesc-mcp-server")
        .map(std::path::PathBuf::from)
        .expect("CARGO_BIN_EXE_vesc-mcp-server");
    let mut child = Command::new(server)
        .arg(PREPARE_KNOWLEDGE_CHILD_ARG)
        .env(VESC_MCP_CONFIG_ENV, config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start preparation child");
    let mut stdin = child.stdin.take().expect("preparation child stdin");
    serde_json::to_writer(&mut stdin, request).expect("write preparation request");
    stdin.flush().expect("flush preparation request");
    drop(stdin);
    let output = child.wait_with_output().expect("preparation child output");
    assert!(
        output.status.success(),
        "preparation child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON preparation response")
}

#[test]
fn preparation_child_round_trips_a_structured_response() {
    let (_temp, config_path, config) = test_config();
    let request = PrepareKnowledgeChildRequest::new(PrepareVescKnowledgeParams::default(), &config);

    let response = run_child(&request, &config_path);

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "not_configured");
}

#[test]
fn preparation_child_rejects_a_changed_configuration() {
    let (_temp, config_path, config) = test_config();
    let mut request =
        PrepareKnowledgeChildRequest::new(PrepareVescKnowledgeParams::default(), &config);
    request.config_fingerprint = "changed".into();

    let response = run_child(&request, &config_path);

    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "not_ready");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("restart"))
    );
}
