mod common;

use std::fs;

use vesc_domain::wire::{FIELD_SPINE, VescPackageBuildInput, build_vescpkg_bytes, package_fields};
use vesc_domain::{LispImport, parse_lisp_imports};

use common::fixture_path;

/// POC `package_format.rs` — `lisp_imports_embed_native_payload_bytes` (L337).
#[test]
fn characterization_lisp_imports_embed_native_payload_bytes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).expect("src dir");
    fs::write(src_dir.join("package_lib.bin"), [0, 1, 2, 3, 0xff]).expect("native payload");

    let package = build_vescpkg_bytes(&VescPackageBuildInput {
        name: "test",
        description_md: "",
        lisp_source: "(import \"src/package_lib.bin\" 'package-lib)\n(load-native-lib package-lib)\n",
        lisp_editor_path: root,
        qml_file: "",
        pkg_desc_qml: "",
        qml_is_fullscreen: false,
    })
    .expect("package");

    let fields = package_fields(&package).expect("wire fields");
    let lisp_data = fields
        .iter()
        .find(|field| field.key == "lispData")
        .map(|field| field.value.as_slice())
        .expect("lispData field");
    let (code, imports) = parse_lisp_imports(lisp_data).expect("imports");

    assert_eq!(
        code,
        "(import \"src/package_lib.bin\" 'package-lib)\n(load-native-lib package-lib)\n"
    );
    assert_eq!(
        imports,
        vec![LispImport {
            tag: "package-lib".to_owned(),
            offset: 100,
            size: 6,
            payload: vec![0, 1, 2, 3, 0xff, 0],
        }]
    );
    assert_eq!(imports[0].offset % 4, 0);
}

/// POC `package_format.rs` — `package_uses_the_vesc_tool_field_spine` (L408).
#[test]
fn characterization_package_uses_vesc_tool_field_spine() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(root.join("src/package_lib.bin"), [0xaa]).expect("native payload");

    let package = build_vescpkg_bytes(&VescPackageBuildInput {
        name: "test",
        description_md: "markdown",
        lisp_source: "(import \"src/package_lib.bin\" 'package-lib)\n",
        lisp_editor_path: root,
        qml_file: "qml",
        pkg_desc_qml: "descriptor",
        qml_is_fullscreen: false,
    })
    .expect("package");

    let fields = package_fields(&package).expect("fields");
    assert_eq!(
        fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<Vec<_>>(),
        FIELD_SPINE.as_slice()
    );
    assert_eq!(fields[0].value, b"test");
    assert_eq!(fields[1].value, b"markdown");
    assert_eq!(fields[3].value, b"qml");
    assert_eq!(fields[4].value, b"descriptor");
    assert_eq!(fields[5].value, [0]);
}

/// Built artifact matches committed golden SHA-256.
#[test]
fn characterization_matches_golden_sha256() {
    use vesc_mcp_adapters::build_package_from_root;

    let root = fixture_path("poc-native-lib-minimal");
    let built = build_package_from_root(&root).expect("build");
    let golden_hash =
        fs::read_to_string(fixture_path("golden/poc-minimal.sha256")).expect("golden sha256");
    let expected = golden_hash
        .split_whitespace()
        .next()
        .expect("hash column")
        .to_ascii_lowercase();
    assert_eq!(built.sha256, expected);
}
