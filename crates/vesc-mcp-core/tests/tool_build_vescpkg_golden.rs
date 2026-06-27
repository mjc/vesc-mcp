//! Optional `build_vescpkg` golden-stability tests requiring `vesc_tool` on PATH or `VESC_TOOL_PATH`.
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

fn build_sha256(root: &str, vesc_tool: &std::path::Path) -> Option<String> {
    let root_path = fixture_path(root);
    let response = build_vescpkg_tool_with_runner(
        &BuildVescpkgParams {
            root: root_path.display().to_string(),
            timeout_secs: DEFAULT_BUILD_TIMEOUT_SECS,
        },
        &RealVescToolRunner,
        Some(vesc_tool),
        Some(&fixture_sandbox_roots()),
    );
    if response.ok {
        return response.sha256;
    }
    eprintln!("build_vescpkg failed for {root}: {:?}", response.error);
    None
}

fn assert_vesc_tool_matches_golden(fixture: &str, golden_sha256_rel: Option<&str>) {
    let Some(vesc_tool) = resolve_vesc_tool_for_tests() else {
        eprintln!(
            "skip vesc_tool build test: binary not available (set VESC_TOOL_PATH or install vesc_tool)"
        );
        return;
    };

    let tool_sha = build_sha256(fixture, &vesc_tool).unwrap_or_else(|| {
        panic!("vesc_tool build must succeed for {fixture} when binary is available");
    });

    if let Some(golden_rel) = golden_sha256_rel {
        let golden_hash = fs::read_to_string(fixture_path(golden_rel)).expect("golden sha256");
        let expected = golden_hash
            .split_whitespace()
            .next()
            .expect("hash column")
            .to_ascii_lowercase();
        assert_eq!(
            tool_sha, expected,
            "{fixture}: vesc_tool build must match committed golden sha256"
        );
    }
}

#[test]
fn tool_build_native_lib_minimal_matches_golden_when_vesc_tool_available() {
    assert_vesc_tool_matches_golden(
        "native-lib-minimal",
        Some("golden/native-lib-minimal.sha256"),
    );
}

#[test]
fn tool_build_refloat_minimal_succeeds_when_vesc_tool_available() {
    let Some(vesc_tool) = resolve_vesc_tool_for_tests() else {
        eprintln!("skip: vesc_tool not available");
        return;
    };
    assert!(build_sha256("refloat-minimal", &vesc_tool).is_some());
}
