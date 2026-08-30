//! Pinned upstream assets used by the in-process Nemotron backend.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ModelAsset {
    pub(super) filename: &'static str,
    pub(super) url: &'static str,
    pub(super) sha256: &'static str,
    pub(super) size_bytes: u64,
}

pub(super) const RUNTIME_VERSION: &str = "0.1.0";
const MODEL_MULTI_FILE: &str = "nemotron-3.5-asr-streaming-0.6b.q8_0.gguf";
const MODEL_MULTI_URL: &str = "https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/resolve/main/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf";
const MODEL_MULTI_SHA256: &str = "a5c435f294eea8f88ce68dd27b8c3bfea7f777cb2fbba04fcd30eaa555f429ae";
const MODEL_MULTI_SIZE: u64 = 742_000_000;
const MODEL_ENGLISH_FILE: &str = "nemotron-speech-streaming-en-0.6b.q8_0.gguf";
const MODEL_ENGLISH_URL: &str = "https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b/resolve/main/nemotron-speech-streaming-en-0.6b.q8_0.gguf";
const MODEL_ENGLISH_SHA256: &str =
    "d9a01898d2a611c8764e23a1c2f45e70bbd5a425dc4de93692ac951dd603812d";
const MODEL_ENGLISH_SIZE: u64 = 700_000_000;

pub(super) const MULTI_MODEL: ModelAsset = ModelAsset {
    filename: MODEL_MULTI_FILE,
    url: MODEL_MULTI_URL,
    sha256: MODEL_MULTI_SHA256,
    size_bytes: MODEL_MULTI_SIZE,
};
pub(super) const ENGLISH_MODEL: ModelAsset = ModelAsset {
    filename: MODEL_ENGLISH_FILE,
    url: MODEL_ENGLISH_URL,
    sha256: MODEL_ENGLISH_SHA256,
    size_bytes: MODEL_ENGLISH_SIZE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RuntimeAsset {
    pub(super) filename: &'static str,
    pub(super) url: &'static str,
    pub(super) sha256: &'static str,
    pub(super) library_filename: &'static str,
}

#[cfg(windows)]
pub(super) const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-windows-x86_64-cpu.zip",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cpu.zip",
    sha256: "5e4ea81046012edcd77fd8848de8eefb5a4ba38cc26f52eb544ab184695a75d6",
    library_filename: "nemo_speech_asr_c.dll",
};
#[cfg(windows)]
pub(super) const RUNTIME_VULKAN: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-windows-x86_64-vulkan.zip",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-vulkan.zip",
    sha256: "b5e7b04a637da4eb25a60253e2db65774998e8dfb48c08b4db763009b82ac7ac",
    library_filename: "nemo_speech_asr_c.dll",
};
#[cfg(windows)]
pub(super) const RUNTIME_CUDA: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-windows-x86_64-cuda.zip",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cuda.zip",
    sha256: "ba024204e76ca2fa4eefa8787506c3c49e418147f627f60cf9206a582b60089c",
    library_filename: "nemo_speech_asr_c.dll",
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-x86_64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-cpu.tar.gz",
    sha256: "0f74131d631ad2c694cf0ec53490866bb6461147959589a69fb6fc231944065b",
    library_filename: "libnemo_speech_asr_c.so",
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) const RUNTIME_VULKAN: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-x86_64-vulkan.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-vulkan.tar.gz",
    sha256: "ce7b7c3c8771cb7450b26e6d4bd8fb2c5e35bcd9fe0076387f35052e9b9523ae",
    library_filename: "libnemo_speech_asr_c.so",
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(super) const RUNTIME_CUDA: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-x86_64-cuda.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-x86_64-cuda.tar.gz",
    sha256: "e68628f396489c98fb353e070efaea5bc4977409ae7734fce56c251a79e29147",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(super) const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-aarch64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-aarch64-cpu.tar.gz",
    sha256: "0e4112255d566de7bdd142f239e984995c4447103ba8feb41f2bb5c559d561d3",
    library_filename: "libnemo_speech_asr_c.so",
};
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(super) const RUNTIME_VULKAN: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-linux-aarch64-vulkan.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-linux-aarch64-vulkan.tar.gz",
    sha256: "4ccc58a2e850dbc4b5d7ce8b44a74b54a7074d936b215548d469798af646f62f",
    library_filename: "libnemo_speech_asr_c.so",
};

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(super) const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-macos-x86_64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-x86_64-cpu.tar.gz",
    sha256: "042a4612e07460fab6a39b5d862aa1e39d0ac3eaedfdb979f3f5fc12de510c20",
    library_filename: "libnemo_speech_asr_c.dylib",
};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(super) const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "nemo-speech-0.1.0-macos-aarch64-cpu.tar.gz",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-macos-aarch64-cpu.tar.gz",
    sha256: "971661d38d4bf97a63c528d13041a964316d25068d8df045e5b4839848092f25",
    library_filename: "libnemo_speech_asr_c.dylib",
};
#[cfg(target_os = "macos")]
pub(super) const RUNTIME_VULKAN: RuntimeAsset = RUNTIME_CPU;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(super) const RUNTIME_CPU: RuntimeAsset = RuntimeAsset {
    filename: "unsupported-platform",
    url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/tag/v0.1.0",
    sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    library_filename: "libnemo_speech_asr_c.so",
};
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(super) const RUNTIME_VULKAN: RuntimeAsset = RUNTIME_CPU;

pub(super) fn runtime_asset(device: &str) -> RuntimeAsset {
    let requested = device.trim().to_ascii_lowercase();
    if requested == "cpu" {
        return RUNTIME_CPU;
    }
    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    if requested == "cuda" {
        return RUNTIME_CUDA;
    }
    #[cfg(any(windows, target_os = "linux"))]
    if requested == "auto" || requested == "vulkan" {
        return RUNTIME_VULKAN;
    }
    RUNTIME_CPU
}

#[cfg(test)]
mod tests {
    use super::{runtime_asset, ENGLISH_MODEL, MULTI_MODEL, RUNTIME_VERSION};

    #[test]
    fn catalog_keeps_pinned_model_and_runtime_identity() {
        assert_eq!(RUNTIME_VERSION, "0.1.0");
        assert!(MULTI_MODEL.filename.ends_with(".gguf"));
        assert!(ENGLISH_MODEL.filename.ends_with(".gguf"));
        assert_ne!(MULTI_MODEL.filename, ENGLISH_MODEL.filename);
        assert!(runtime_asset("cpu").filename.contains("cpu"));
        #[cfg(any(windows, target_os = "linux"))]
        assert!(runtime_asset("auto").filename.contains("vulkan"));
        #[cfg(not(any(windows, target_os = "linux")))]
        assert!(runtime_asset("auto").filename.contains("cpu"));
    }
}
