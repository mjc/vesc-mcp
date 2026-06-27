//! Regenerate `tests/fixtures/golden/poc-minimal.vescpkg` from `build_package_from_root`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use vesc_mcp_adapters::build_package_from_root;

fn main() -> Result<()> {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let root = fixtures.join("poc-native-lib-minimal");
    let golden_dir = fixtures.join("golden");

    let built = build_package_from_root(&root).context("build poc-native-lib-minimal golden")?;
    let bytes = fs::read(&built.artifact_path)
        .with_context(|| format!("read built package at {}", built.artifact_path.display()))?;

    fs::create_dir_all(&golden_dir).context("create golden directory")?;
    let out_path = golden_dir.join("poc-minimal.vescpkg");
    fs::write(&out_path, &bytes).context("write golden vescpkg")?;
    fs::write(
        golden_dir.join("poc-minimal.sha256"),
        format!("{}  poc-minimal.vescpkg\n", built.sha256),
    )
    .context("write golden sha256 sidecar")?;

    let _ = fs::remove_file(&built.artifact_path);

    println!(
        "wrote {} ({} bytes)\nsha256 {}",
        out_path.display(),
        bytes.len(),
        built.sha256
    );
    Ok(())
}
