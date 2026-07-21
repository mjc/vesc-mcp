use vesc_mcp_core::VescMcpService;

#[test]
fn http_service_exposes_only_safe_shared_tools() {
    let service = VescMcpService::new().http_service();
    let mut names = service.list_tool_names();
    names.sort();

    for required in [
        "ping",
        "replay_vesc_knowledge_correction",
        "search_vesc_knowledge",
        "set_current_repository",
    ] {
        assert!(names.iter().any(|name| name == required));
    }
    for unsafe_tool in [
        "build_vescpkg",
        "correct_vesc_knowledge",
        "inspect_pkgdesc",
        "inspect_vescpkg",
        "run_package_checks",
        "submit_vesc_knowledge_feedback",
        "validate_package_layout",
    ] {
        assert!(!names.iter().any(|name| name == unsafe_tool));
    }
}
