//! Integration tests for the MCP resource URI scheme and registry.

use vesc_mcp_core::resources::{
    CatalogResourceUri, FixtureManifestUri, ManifestResourceUri, ParsedResourceUri, ResourceMeta,
    ResourceRegistry, parse_resource_uri,
};

#[test]
fn resource_registry_parses_valid_uris() {
    let cases = [
        (
            "vesc://catalog/build-recipe/refloat-vesc-tool",
            ParsedResourceUri::Catalog(CatalogResourceUri {
                kind: "build-recipe".into(),
                id: "refloat-vesc-tool".into(),
            }),
        ),
        (
            "vesc://catalog/doc/topic/pkgdesc_dialects",
            ParsedResourceUri::Catalog(CatalogResourceUri {
                kind: "doc".into(),
                id: "topic/pkgdesc_dialects".into(),
            }),
        ),
        (
            "vescpkg://fixture/refloat-minimal/manifest",
            ParsedResourceUri::FixtureManifest(FixtureManifestUri {
                name: "refloat-minimal".into(),
            }),
        ),
        (
            "vescpkg://manifest/tests/fixtures/refloat-minimal/pkgdesc.qml",
            ParsedResourceUri::DynamicManifest(ManifestResourceUri {
                path: "tests/fixtures/refloat-minimal/pkgdesc.qml".into(),
            }),
        ),
    ];

    for (uri, expected) in &cases {
        let parsed = parse_resource_uri(uri).unwrap_or_else(|err| {
            panic!("expected valid URI {uri:?}, got {err}");
        });
        assert_eq!(&parsed, expected, "uri: {uri}");
    }

    let registry = ResourceRegistry::new();
    for (uri, _) in &cases {
        registry
            .validate_uri(uri)
            .unwrap_or_else(|err| panic!("registry rejected valid URI {uri:?}: {err}"));
    }
}

#[test]
fn resource_registry_rejects_malformed_uri() {
    let malformed = [
        "",
        "not-a-uri",
        "http://catalog/build-recipe/refloat-vesc-tool",
        "vesc://wrong/build-recipe/refloat-vesc-tool",
        "vesc://catalog/only-kind",
        "vescpkg://fixture/refloat-minimal",
        "vescpkg://fixture/refloat-minimal/extra/manifest",
        "vescpkg://manifest/",
        "vescpkg://unknown/refloat-minimal/manifest",
    ];

    for uri in malformed {
        let err = parse_resource_uri(uri).unwrap_err();
        assert!(!err.reason.is_empty(), "uri {uri:?}: {err:?}");
        assert!(
            ResourceRegistry::new().validate_uri(uri).is_err(),
            "registry should reject {uri:?}"
        );
    }
}

#[test]
fn resource_registry_lists_registered_static_resources() {
    let mut registry = ResourceRegistry::new();
    registry
        .register(ResourceMeta {
            uri: "vesc://catalog/build-recipe/refloat-vesc-tool".into(),
            name: "refloat-vesc-tool build recipe".into(),
            description: Some("Build Refloat packages with vesc_tool".into()),
            mime_type: "text/markdown".into(),
        })
        .expect("register catalog resource");

    let listed = registry.list_static();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].uri,
        "vesc://catalog/build-recipe/refloat-vesc-tool"
    );
    assert_eq!(listed[0].mime_type, "text/markdown");

    let mcp = registry.list_mcp_resources();
    assert_eq!(mcp.len(), 1);
    assert_eq!(mcp[0].uri, "vesc://catalog/build-recipe/refloat-vesc-tool");
    assert_eq!(mcp[0].mime_type.as_deref(), Some("text/markdown"));
}
