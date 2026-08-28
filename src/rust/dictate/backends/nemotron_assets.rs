//! First-run asset management for the in-process Nemotron backend.
//!
//! The application binary contains the adapter, but NVIDIA's model weights and
//! native runtime remain separately licensed upstream artifacts.  This module
//! makes that distinction invisible to normal users: selecting the in-process
//! Nemotron provider resolves the official model id to a per-user cache and
//! downloads/verifies the model and NeMo-Speech.cpp runtime on first use.  An
//! explicit local path or library override still wins for developers and
//! air-gapped installations.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::cloud_api::http::USER_AGENT;
use crate::dictate::backends::cloud_transcribe::{
    is_nemotron_english_model, is_nemotron_model_alias,
};
#[cfg(test)]
use crate::dictate::backends::cloud_transcribe::{NEMOTRON_ENGLISH_MODEL, NEMOTRON_MULTI_MODEL};
use crate::os_cache::{replace_atomic, user_cache_dir};

const RUNTIME_VERSION: &str = "0.1.0";
const MODEL_MULTI_FILE: &str = "nemotron-3.5-asr-streaming-0.6b.q8_0.gguf";
const MODEL_MULTI_URL: &str = "https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/resolve/main/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf";
const MODEL_MULTI_SHA256: &str = "a5c435f294eea8f88ce68dd27b8c3bfea7f777cb2fbba04fcd30eaa555f429ae";
const MODEL_MULTI_SIZE: u64 = 742_000_000;
const MODEL_ENGLISH_FILE: &str = "nemotron-speech-streaming-en-0.6b.q8_0.gguf";
const MODEL_ENGLISH_URL: &str = "https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b/resolve/main/nemotron-speech-streaming-en-0.6b.q8_0.gguf";
const MODEL_ENGLISH_SHA256: &str =
    "d9a01898d2a611c8764e23a1c2f45e70bbd5a425dc4de93692ac951dd603812d";
const MODEL_ENGLISH_SIZE: u64 = 700_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModelAsset {
    filename: &'static str,
    url: &'static str,
    sha256: &'static str,
    size_bytes: u64,
}

const MULTI_MODEL: ModelAsset = ModelAsset {
    filename: MODEL_MULTI_FILE,
    url: MODEL_MULTI_URL,
    sha256: MODEL_MULTI_SHA256,
    size_bytes: MODEL_MULTI_SIZE,
};

const ENGLISH_MODEL: ModelAsset = ModelAsset {
    filename: MODEL_ENGLISH_FILE,
    url: MODEL_ENGLISH_URL,
    sha256: MODEL_ENGLISH_SHA256,
    size_bytes: MODEL_ENGLISH_SIZE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeAsset {
    filename: &'static str,
    url: &'static str,
    sha256: &'static str,
    library_filename: &'static str,
}

#[cfg(windows)]
const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-windows-x86_64-cpu.zip",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cpu.zip",
    sha256: "5e4ea81046012edcd77fd8848de8eefb5a4ba38cc26f52eb544ab184695a75d6",
    library_filename: "nemo_speech_asr_c.dll",
};

#[cfg(windows)]
const RUNTIME_VULKAN: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-windows-x86_64-vulkan.zip",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-vulkan.zip",
    sha256: "b5e7b04a637da4eb25a60253e2db65774998e8dfb48c08b4db763009b82ac7ac",
    library_filename: "nemo_speech_asr_c.dll",
};

#[cfg(windows)]
const RUNTIME_CUDA: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-windows-x86_64-cuda.zip",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cuda.zip",
    sha256: "ba024204e76ca2fa4eefa8787506c3c49e418147f627f60cf9206a582b60089c",
    library_filename: "nemo_speech_asr_c.dll",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-x86_64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-cpu.tar.gz",
    sha256: "0f74131d631ad2c694cf0ec53490866bb6461147959589a69fb6fc231944065b",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUNTIME_VULKAN: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-x86_64-vulkan.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-vulkan.tar.gz",
    sha256: "ce7b7c3c8771cb7450b26e6d4bd8fb2c5e35bcd9fe0076387f35052e9b9523ae",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RUNTIME_CUDA: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-x86_64-cuda.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-cuda.tar.gz",
    sha256: "e68628f396489c98fb353e070efaea5bc4977409ae7734fce56c251a79e29147",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-aarch64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-aarch64-cpu.tar.gz",
    sha256: "0e4112255d566de7bdd142f239e984995c4447103ba8feb41f2bb5c559d561d3",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const RUNTIME_VULKAN: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-aarch64-vulkan.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-aarch64-vulkan.tar.gz",
    sha256: "4ccc58a2e850dbc4b5d7ce8b44a74b54a7074d936b215548d469798af646f62f",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-macos-x86_64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-x86_64-cpu.tar.gz",
    sha256: "042a4612e07460fab6a39b5d862aa1e39d0ac3eaedfdb979f3f5fc12de510c20",
    library_filename: "libnemo_speech_asr_c.dylib",
};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-macos-aarch64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-aarch64-cpu.tar.gz",
    sha256: "971661d38d4bf97a63c528d13041a964316d25068d8df045e5b4839848092f25",
    library_filename: "libnemo_speech_asr_c.dylib",
};

#[cfg(target_os = "macos")]
const RUNTIME_VULKAN: RuntimeAsset = RUNTIME_CPU;

// Keep the module buildable on an unsupported target so the rest of the
// application still produces its normal feature-gated diagnostic instead of
// failing with an undefined platform constant.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "unsupported-platform",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/tag/v0.1.0",
    sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
const RUNTIME_VULKAN: RuntimeAsset = RUNTIME_CPU;

fn cache_root() -> Result<PathBuf> {
    user_cache_dir()
        .map(|path| {
            path.join("whisper-dictate")
                .join("nemotron")
                .join(RUNTIME_VERSION)
        })
        .ok_or_else(|| anyhow!("could not resolve the user cache directory for Nemotron assets"))
}

fn model_asset(requested: &str) -> Option<ModelAsset> {
    let trimmed = requested.trim();
    if trimmed.is_empty()
        || (is_nemotron_model_alias(trimmed) && !is_nemotron_english_model(trimmed))
    {
        return Some(MULTI_MODEL);
    }
    if is_nemotron_english_model(trimmed) {
        return Some(ENGLISH_MODEL);
    }
    let filename = Path::new(trimmed)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    if filename == MODEL_MULTI_FILE {
        Some(MULTI_MODEL)
    } else if filename == MODEL_ENGLISH_FILE {
        Some(ENGLISH_MODEL)
    } else {
        None
    }
}

/// Resolve a configured Nemotron model id/path. Official model ids and their
/// well-known GGUF filenames are downloaded into the user cache when missing;
/// arbitrary paths remain explicit user-managed assets.
pub(crate) fn ensure_model_path(requested: &str) -> Result<PathBuf> {
    let Some(asset) = model_asset(requested) else {
        let path = PathBuf::from(requested.trim());
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "Nemotron model file does not exist: {} (use an official Nemotron model id or an existing .gguf path)",
            path.display()
        ));
    };
    let target = cache_root()?.join("models").join(asset.filename);
    if target.is_file() {
        match verify_sha256(&target, asset.sha256) {
            Ok(()) => return Ok(target),
            Err(error) => {
                // A cancelled/interrupted first-run download must not strand
                // the user with a permanently broken cache entry.  Keep the
                // failure visible in diagnostics, then fetch a clean copy.
                crate::diag::log!(
                    "[nemotron] cached model failed verification: {error}; redownloading"
                );
                fs::remove_file(&target).with_context(|| {
                    format!("remove invalid cached Nemotron model {}", target.display())
                })?;
            }
        }
    }
    if crate::whisper::model_manager::is_local_only() {
        return Err(anyhow!(
            "Nemotron assets are missing and local-only mode blocks downloads; disable local_only or install the model manually at {}",
            target.display()
        ));
    }
    download_verified(asset.url, asset.sha256, asset.size_bytes, &target, "model")?;
    Ok(target)
}

/// Locate the native runtime beside the executable or in the cache. If no
/// override was supplied, download and extract NVIDIA's verified archive.
pub(crate) fn ensure_library_path(explicit: Option<&str>, device: &str) -> Result<PathBuf> {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "NeMo-Speech.cpp ASR library override does not exist: {}",
            path.display()
        ));
    }
    if let Ok(path) = super::nemotron_ffi::resolve_library_path(None) {
        if path.is_file() {
            return Ok(path);
        }
    }
    let asset = runtime_asset(device)?;
    let root = cache_root()?.join("runtime");
    let library = root.join("bin").join(asset.library_filename);
    if library.is_file() {
        return Ok(library);
    }
    if crate::whisper::model_manager::is_local_only() {
        return Err(anyhow!(
            "NeMo-Speech.cpp runtime is missing and local-only mode blocks downloads; install the {} archive beside wd.exe",
            asset.filename
        ));
    }
    let archive = root.with_extension("archive");
    download_verified(asset.url, asset.sha256, 0, &archive, "runtime archive")?;
    extract_runtime(&archive, &root, asset.library_filename)?;
    let _ = fs::remove_file(&archive);
    if !library.is_file() {
        return Err(anyhow!(
            "NeMo-Speech.cpp archive did not contain {}",
            asset.library_filename
        ));
    }
    Ok(library)
}

fn runtime_asset(device: &str) -> Result<RuntimeAsset> {
    let requested = device.trim().to_ascii_lowercase();
    if requested == "cpu" {
        return Ok(RUNTIME_CPU);
    }
    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    if requested == "cuda" {
        return Ok(RUNTIME_CUDA);
    }
    #[cfg(any(windows, target_os = "linux"))]
    if requested == "auto" || requested == "vulkan" {
        return Ok(RUNTIME_VULKAN);
    }
    Ok(RUNTIME_CPU)
}

fn download_verified(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    target: &Path,
    label: &str,
) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("Nemotron {label} has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create Nemotron asset directory {}", parent.display()))?;
    let partial = target.with_extension("partial");
    crate::diag::log!(
        "[nemotron] downloading {label} ({:.0} MB) from {url}",
        expected_size as f64 / 1_000_000.0
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(21_600)))
        .build()
        .into();
    let response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| anyhow!("download Nemotron {label} failed: {error}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(anyhow!("download Nemotron {label} failed: HTTP {status}"));
    }
    let (_, body) = response.into_parts();
    let mut reader = body.into_reader();
    let mut file = File::create(&partial)
        .with_context(|| format!("create Nemotron partial file {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    let result = (|| -> Result<()> {
        loop {
            let count = reader
                .read(&mut buffer)
                .with_context(|| format!("read Nemotron {label}"))?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])
                .with_context(|| format!("write Nemotron {label}"))?;
            hasher.update(&buffer[..count]);
            bytes = bytes.saturating_add(count as u64);
        }
        file.flush().context("flush Nemotron asset")?;
        let actual = hex_lower(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(anyhow!(
                "Nemotron {label} SHA-256 mismatch: expected {expected_sha256}, got {actual}"
            ));
        }
        replace_atomic(&partial, target)
            .with_context(|| format!("publish Nemotron {label} {}", target.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result?;
    crate::diag::log!("[nemotron] downloaded {label} bytes={bytes}");
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = hex_lower(&hasher.finalize());
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(anyhow!(
            "Nemotron cached asset SHA-256 mismatch: expected {expected}, got {actual}"
        ))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn extract_runtime(archive: &Path, destination: &Path, library_filename: &str) -> Result<()> {
    let staging = destination.with_extension("partial");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("remove {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("create {}", staging.display()))?;
    let status = if cfg!(windows) {
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force",
            ])
            .arg(archive)
            .arg(&staging)
            .status()
            .context("start PowerShell to extract the Nemotron runtime")?
    } else {
        Command::new("tar")
            .args(["-xzf"])
            .arg(archive)
            .args(["-C"])
            .arg(&staging)
            .status()
            .context("start tar to extract the Nemotron runtime")?
    };
    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        return Err(anyhow!(
            "failed to extract Nemotron runtime archive (exit {})",
            status
        ));
    }
    let extracted = find_named_file(&staging, library_filename).ok_or_else(|| {
        anyhow!(
            "Nemotron runtime archive did not contain {}",
            library_filename
        )
    })?;
    if destination.exists() {
        fs::remove_dir_all(destination)
            .with_context(|| format!("replace old Nemotron runtime {}", destination.display()))?;
    }
    replace_dir(&staging, destination)?;
    let published = find_named_file(destination, library_filename).ok_or_else(|| {
        anyhow!(
            "Nemotron runtime extraction lost {} (source was {})",
            library_filename,
            extracted.display()
        )
    })?;
    if !published.is_file() {
        return Err(anyhow!("published Nemotron library is not a file"));
    }
    Ok(())
}

fn find_named_file(root: &Path, filename: &str) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let entries = fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_file() && child.file_name().and_then(|n| n.to_str()) == Some(filename) {
                return Some(child);
            }
            if child.is_dir() {
                pending.push(child);
            }
        }
    }
    None
}

fn replace_dir(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(source, destination).with_context(|| {
        format!(
            "publish Nemotron runtime {} -> {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "nemotron_assets_tests.rs"]
mod tests;
