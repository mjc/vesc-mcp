//! Narrow local semantic defaults for hardware combinations measured in this workspace.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const JINA_CODE_MODEL_ID: &str = "jinaai/jina-embeddings-v2-base-code";
pub const JINA_CODE_MODEL_REVISION: &str = "516f4baf13dec4ddddda8631e019b5737c8bc250";
pub const JINA_CODE_FP16_SHA256: &str =
    "1aafc4fcd63d2e6899e88402ff731e7c646c2e435048294a3cbc908a40d45d7c";
pub const JINA_CODE_INT8_SHA256: &str =
    "ed45870251c9f0cf656e78aab0d37a23489066df8a222bb1c8caf8a45f2cb16d";
pub const JINA_CODE_MAX_LENGTH: usize = 512;
pub const JINA_CODE_INGEST_MAX_LENGTH: usize = 64;
pub const JINA_CODE_INGEST_BATCH_SIZE: usize = 64;

/// Platform-neutral CPU query side of the pinned Jina split profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JinaCodeQueryProfile {
    pub query_model_dir: PathBuf,
    pub artifact_dir: PathBuf,
}

impl JinaCodeQueryProfile {
    #[must_use]
    pub fn detect(workspace_root: &Path) -> Option<Self> {
        let profile = Self {
            query_model_dir: workspace_root
                .join("target/models/jina-embeddings-v2-base-code-quantized"),
            artifact_dir: workspace_root.join("target/knowledge-artifacts-jina-code-fp16-rx5700xt"),
        };
        profile
            .artifact_dir
            .join("active.json")
            .is_file()
            .then_some(())?;
        model_matches(
            &profile.query_model_dir.join("model.onnx"),
            161_895_621,
            JINA_CODE_INT8_SHA256,
        )
        .then_some(profile)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rx5700Xt8600gProfile {
    pub ingestion_model_dir: PathBuf,
    pub query_model_dir: PathBuf,
    pub artifact_dir: PathBuf,
}

impl Rx5700Xt8600gProfile {
    #[must_use]
    pub fn detect(workspace_root: &Path) -> Option<Self> {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
        if !cpuinfo.contains("AMD Ryzen 5 8600G") || !has_amd_pci_device("0x731f") {
            return None;
        }

        let profile = Self {
            ingestion_model_dir: workspace_root
                .join("target/models/jina-embeddings-v2-base-code-fp16"),
            query_model_dir: workspace_root
                .join("target/models/jina-embeddings-v2-base-code-quantized"),
            artifact_dir: workspace_root.join("target/knowledge-artifacts-jina-code-fp16-rx5700xt"),
        };
        model_matches(
            &profile.ingestion_model_dir.join("model.onnx"),
            321_072_580,
            JINA_CODE_FP16_SHA256,
        )
        .then_some(())?;
        model_matches(
            &profile.query_model_dir.join("model.onnx"),
            161_895_621,
            JINA_CODE_INT8_SHA256,
        )
        .then_some(())?;
        Some(profile)
    }
}

fn has_amd_pci_device(expected_device: &str) -> bool {
    let Ok(devices) = fs::read_dir("/sys/bus/pci/devices") else {
        return false;
    };
    devices.flatten().any(|entry| {
        fs::read_to_string(entry.path().join("vendor"))
            .is_ok_and(|vendor| vendor.trim() == "0x1002")
            && fs::read_to_string(entry.path().join("device"))
                .is_ok_and(|device| device.trim() == expected_device)
    })
}

fn model_matches(path: &Path, expected_bytes: u64, expected_sha256: &str) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.len() == expected_bytes)
        && sha256_file(path).is_ok_and(|digest| digest == expected_sha256)
}

/// Hash a file incrementally without loading it into memory.
///
/// # Errors
///
/// Returns an I/O error when the file cannot be opened or read.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_file_hashes_incrementally() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        fs::write(temp.path(), b"model").expect("write model");
        assert_eq!(
            sha256_file(temp.path()).expect("hash model"),
            "9372c470eeadd5ecd9c3c74c2b3cb633f8e2f2fad799250a0f70d652b6b825e4"
        );
    }
}
