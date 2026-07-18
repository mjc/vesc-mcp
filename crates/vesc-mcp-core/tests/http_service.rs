use vesc_mcp_core::VescMcpService;

#[test]
fn http_service_exposes_only_safe_shared_tools() {
    let service = VescMcpService::new().http_service();
    let mut names = service.list_tool_names();
    names.sort();

    assert_eq!(
        names,
        [
            "ping",
            "replay_vesc_knowledge_correction",
            "search_vesc_knowledge"
        ]
    );
}
