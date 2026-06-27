//! Optional `build_vescpkg` parity tests requiring `vesc_tool` on PATH or `VESC_TOOL_PATH`.
//!
//! Skips automatically when the binary is unavailable (default CI).

use std::fs;

use vesc_mcp_core::test_support::{
    fixture_path, fixture_sandbox_roots, resolve_vesc_tool_for_tests,
};
use vesc_mcp_core::tools::build::{
    BuildVescpkgParams, DEFAULT_BUILD_TIMEOUT_SECS, RealVescToolRunner,
    build_vescpkg_tool_with_runner,
};

fn build_sha256(root: &str, mode: &str, vesc_tool: Option<&std::path::Path>) -> Option<String> {
    let root_path = fixture_path(root);
    let response = build_vescpkg_tool_with_runner(
        &BuildVescpkgParams {
            root: root_path.display().to_string(),
            mode: mode.into(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        },
        &RealVescToolRunner,
        vesc_tool,
        Some(&fixture_sandbox_roots()),
    );
    if response.ok {
        return response.sha256;
    }
    eprintln!(
        "build_vescpkg ({mode}) failed for {root}: {:?}",
        response.error
    );
    None
}

fn assert_fixture_parity(fixture: &str, golden_sha256_rel: Option<&str>) {
    let Some(vesc_tool) = resolve_vesc_tool_for_tests() else {
        eprintln!(
            "skip parity test: vesc_tool not available (set VESC_TOOL_PATH or install vesc_tool)"
        );
        return;
    };

    let rust_sha = build_sha256(fixture, "rust", None).unwrap_or_else(|| {
        panic!("rust build must succeed for {fixture}");
    });
    let tool_sha = build_sha256(fixture, "vesc_tool", Some(&vesc_tool)).unwrap_or_else(|| {
        panic!("vesc_tool build must succeed for {fixture} when binary is available");
    });

    assert_eq!(
        rust_sha, tool_sha,
        "{fixture}: rust and vesc_tool sha256 must match"
    );

    if let Some(golden_rel) = golden_sha256_rel {
        let golden_hash = fs::read_to_string(fixture_path(golden_rel)).expect("golden sha256");
        let expected = golden_hash
            .split_whitespace()
            .next()
            .expect("hash column")
            .to_ascii_lowercase();
        assert_eq!(
            rust_sha, expected,
            "{fixture}: rust build must match committed golden sha256"
        );
    }
}

#[test]
fn tool_build_parity_poc_native_lib_minimal() {
    assert_fixture_parity("poc-native-lib-minimal", Some("golden/poc-minimal.sha256"));
}

#[test]
fn tool_build_parity_refloat_minimal() {
    assert_fixture_parity("refloat-minimal", None);
}
