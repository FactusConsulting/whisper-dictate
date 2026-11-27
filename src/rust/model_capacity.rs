use std::process::Command;

use anyhow::Result;
use serde::Serialize;

const MIB: u64 = 1 << 20;
const WHISPER_TURBO_F16_MIB: u32 = 2816;
const WHISPER_LARGE_F16_MIB: u32 = 4352;
const WHISPER_VULKAN_SOURCE: &str = "whisper-vulkan";
const NVIDIA_SMI_SOURCE: &str = "nvidia-smi";

/// Capacity for a single adapter. `index` is Whisper's GPU ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpeechGpu {
    pub index: i32,
    pub total_mib: u32,
    pub free_mib: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "Not enough free GPU memory: need {required_mib} MiB, have {free_mib} MiB. Try a smaller local model or a hosted speech provider."
)]
pub struct GpuMemoryShortfall {
    pub required_mib: u32,
    pub free_mib: u32,
}

pub(crate) mod start;

pub(crate) fn best_free_mib(
    capacities: impl Iterator<Item = (u32, u32)>,
    required_mib: u32,
) -> Option<u32> {
    capacities
        .filter(|&(total, _)| total >= required_mib)
        .map(|(total, free)| free.min(total))
        .max()
}

/// Estimate the space for weights plus compute, state, and driver buffers
/// from the actual GGML file length, not its filename or selected preset.
pub(crate) fn whisper_required_mib(file_bytes: u64) -> u32 {
    let weights_mib = file_bytes.div_ceil(MIB);
    let required_mib = match file_bytes {
        // Small models require at least 1 GiB. Stock and Edda Turbo F16,
        // including the ~1.62 GB stock file, reach the 2.75 GiB ceiling.
        ..=1_650_000_000 => (weights_mib * 2).clamp(1024, u64::from(WHISPER_TURBO_F16_MIB)),
        // Stock and Edda Large F16 reach the 4.25 GiB ceiling.
        1_650_000_001..=3_150_000_000 => (weights_mib + 1536).min(u64::from(WHISPER_LARGE_F16_MIB)),
        // Don't cap the headroom for larger custom files.
        _ => weights_mib + weights_mib.div_ceil(2).max(1536),
    };
    u32::try_from(required_mib).unwrap_or(u32::MAX)
}

/// Don't split models between GPUs. Prefer the adapter with the most
/// usable headroom, and use the lowest ordinal to break a tie.
#[cfg(any(feature = "whisper-rs-local", test))]
pub(crate) fn select_whisper_gpu(gpus: &[SpeechGpu], required_mib: u32) -> Option<i32> {
    gpus.iter()
        .filter(|gpu| gpu.total_mib >= required_mib && gpu.free_mib >= required_mib)
        .max_by_key(|gpu| (gpu.free_mib, std::cmp::Reverse(gpu.index)))
        .map(|gpu| gpu.index)
}

#[cfg(any(feature = "whisper-rs-local", test))]
pub(crate) fn whisper_gpus() -> Vec<SpeechGpu> {
    #[cfg(feature = "whisper-rs-vulkan")]
    {
        vulkan::Adapters::probe().whisper_gpus()
    }
    #[cfg(not(feature = "whisper-rs-vulkan"))]
    {
        Vec::new()
    }
}

pub(crate) fn whisper_gpus_for_model_load() -> Vec<SpeechGpu> {
    #[cfg(feature = "whisper-rs-vulkan")]
    {
        vulkan::Adapters::probe_for_model_load(query_gpus).whisper_gpus()
    }
    #[cfg(not(feature = "whisper-rs-vulkan"))]
    {
        Vec::new()
    }
}

mod hviske_cuda;
pub(crate) use hviske_cuda::hviske_cuda_total_mib;

#[cfg(any(feature = "whisper-rs-vulkan", feature = "nemotron-local"))]
pub(crate) mod vulkan;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub total_mb: u32,
    pub free_mb: u32,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelProfile {
    pub name: &'static str,
    pub category: &'static str,
    pub required_free_mb: u32,
    pub setting_hint: &'static str,
    pub note: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFit {
    pub profile: ModelProfile,
    pub status: &'static str,
    pub detail: String,
}

const MODEL_PROFILES: &[ModelProfile] = &[
    ModelProfile {
        name: "Whisper large-v3-turbo",
        category: "stt",
        required_free_mb: WHISPER_TURBO_F16_MIB,
        setting_hint:
            "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3-turbo; VOICEPI_WHISPER_GPU=vulkan",
        note: "Stock and Edda Turbo F16 weights, plus Vulkan working memory.",
    },
    ModelProfile {
        name: "Whisper large-v3",
        category: "stt",
        required_free_mb: WHISPER_LARGE_F16_MIB,
        setting_hint:
            "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3; VOICEPI_WHISPER_GPU=vulkan",
        note: "Stock and Edda Large F16 weights, plus Vulkan working memory.",
    },
    ModelProfile {
        name: "Ollama Qwen2.5 3B",
        category: "post",
        required_free_mb: 4500,
        setting_hint: "VOICEPI_POST_PROCESSOR=ollama; VOICEPI_POST_MODEL=qwen2.5:3b",
        note: "Small local text cleanup model; practical alongside STT on many GPUs.",
    },
    ModelProfile {
        name: "Ollama Qwen2.5 7B Q4",
        category: "post",
        required_free_mb: 8000,
        setting_hint: "VOICEPI_POST_PROCESSOR=ollama; VOICEPI_POST_MODEL=qwen2.5:7b",
        note: "Better text cleanup if GPU has headroom; may spill to CPU otherwise.",
    },
    ModelProfile {
        name: "Ollama Qwen2.5 14B Q4",
        category: "post",
        required_free_mb: 14000,
        setting_hint: "VOICEPI_POST_PROCESSOR=ollama; VOICEPI_POST_MODEL=qwen2.5:14b",
        note: "Higher-quality local rewrite; usually not for concurrent STT on small GPUs.",
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapacityReport {
    pub gpus: Vec<GpuInfo>,
    pub models: Vec<ModelFitReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelFitReport {
    pub name: &'static str,
    pub category: &'static str,
    pub required_free_mb: u32,
    pub status: &'static str,
    pub setting_hint: &'static str,
    pub detail: String,
    pub note: &'static str,
}

pub fn handle_command(json: bool) -> Result<()> {
    let gpus = query_gpus();
    #[cfg(feature = "whisper-rs-vulkan")]
    let gpus = {
        let mut gpus = gpus;
        let mut adapters = vulkan::Adapters::probe();
        adapters.limit_nvidia_free_mib(&gpus);
        gpus.extend(adapters.whisper_gpus().into_iter().map(|gpu| GpuInfo {
            index: gpu.index as usize,
            name: "Whisper Vulkan adapter".to_owned(),
            total_mb: gpu.total_mib,
            free_mb: gpu.free_mib,
            source: WHISPER_VULKAN_SOURCE,
        }));
        gpus
    };
    if json {
        println!("{}", serde_json::to_string(&capacity_report_json(&gpus))?);
    } else {
        println!("{}", capacity_report(&gpus));
    }
    Ok(())
}

pub fn query_gpus() -> Vec<GpuInfo> {
    let output = nvidia_smi("--query-gpu=index,name,memory.total,memory.free");
    let Some(output) = output else {
        return Vec::new();
    };
    parse_nvidia_smi_csv(&String::from_utf8_lossy(&output))
}

fn nvidia_smi(fields: &str) -> Option<Vec<u8>> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt as _;

    let mut command = Command::new("nvidia-smi");
    command.args([fields, "--format=csv,noheader,nounits"]);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

pub fn estimate_model_fits(gpus: &[GpuInfo]) -> Vec<ModelFit> {
    MODEL_PROFILES
        .iter()
        .cloned()
        .map(|profile| {
            let required = profile.required_free_mb;
            let source = if profile.category == "stt" {
                WHISPER_VULKAN_SOURCE
            } else {
                NVIDIA_SMI_SOURCE
            };
            let (best_total, best_free) = gpus.iter().filter(|gpu| gpu.source == source).fold(
                (0, 0),
                |(total, free), gpu| {
                    let eligible_free = if gpu.total_mb >= required { gpu.free_mb } else { 0 };
                    (total.max(gpu.total_mb), free.max(eligible_free))
                },
            );
            let (status, detail) = if best_free >= required {
                (
                    "ok",
                    format!("fits now; needs about {required} MiB free VRAM"),
                )
            } else if best_total >= required {
                (
                    "free-vram",
                    format!(
                        "GPU is large enough, but only {best_free} MiB is free; stop other GPU processes to reach about {required} MiB"
                    ),
                )
            } else {
                (
                    "too-small",
                    format!(
                        "needs about {required} MiB free VRAM; largest compatible GPU has {best_total} MiB total"
                    ),
                )
            };
            ModelFit {
                profile,
                status,
                detail,
            }
        })
        .collect()
}

pub fn capacity_report(gpus: &[GpuInfo]) -> String {
    let mut lines = Vec::new();
    if gpus.is_empty() {
        lines.push(
            "GPU capacity: no compatible Whisper Vulkan or NVIDIA reporting adapter found"
                .to_owned(),
        );
    } else {
        lines.push("GPU capacity:".to_owned());
        for gpu in gpus {
            lines.push(format!(
                "  [{}] {}: {} MiB free / {} MiB total ({})",
                gpu.index, gpu.name, gpu.free_mb, gpu.total_mb, gpu.source
            ));
        }
    }
    lines.push(String::new());
    lines.push("Local model fit:".to_owned());
    for fit in estimate_model_fits(gpus) {
        let marker = match fit.status {
            "ok" => "OK",
            "free-vram" => "FREE VRAM",
            _ => "NO",
        };
        lines.push(format!(
            "  {marker:<9} {:<34} ~{} MiB  {}",
            fit.profile.name, fit.profile.required_free_mb, fit.profile.setting_hint
        ));
        lines.push(format!("            {}", fit.detail));
    }
    lines.push(String::new());
    lines.push(
        "Use free VRAM for the current decision; stop whisper-dictate or other GPU apps before benchmarking."
            .to_owned(),
    );
    lines.join("\n")
}

pub fn capacity_report_json(gpus: &[GpuInfo]) -> CapacityReport {
    CapacityReport {
        gpus: gpus.to_vec(),
        models: estimate_model_fits(gpus)
            .into_iter()
            .map(|fit| ModelFitReport {
                name: fit.profile.name,
                category: fit.profile.category,
                required_free_mb: fit.profile.required_free_mb,
                status: fit.status,
                setting_hint: fit.profile.setting_hint,
                detail: fit.detail,
                note: fit.profile.note,
            })
            .collect(),
    }
}

fn parse_nvidia_smi_csv(raw: &str) -> Vec<GpuInfo> {
    raw.lines()
        .filter_map(parse_nvidia_smi_row)
        .collect::<Vec<_>>()
}

fn parse_nvidia_smi_row(row: &str) -> Option<GpuInfo> {
    let parts = row.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 4 {
        return None;
    }
    Some(GpuInfo {
        index: parts[0].parse().ok()?,
        name: parts[1].to_owned(),
        total_mb: parse_mb(parts[2])?,
        free_mb: parse_mb(parts[3])?,
        source: NVIDIA_SMI_SOURCE,
    })
}

fn parse_mb(value: &str) -> Option<u32> {
    value
        .replace("MiB", "")
        .replace("MB", "")
        .trim()
        .parse::<f32>()
        .ok()
        .map(|value| value as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_requirements_scale_from_measured_weight_bytes() {
        assert_eq!(whisper_required_mib(76_000_000), 1024);
        assert_eq!(whisper_required_mib(1_500_000_000), 2816);
        assert_eq!(whisper_required_mib(1_600_000_000), 2816);
        assert_eq!(whisper_required_mib(1_624_555_275), 2816);
        assert_eq!(whisper_required_mib(3_095_033_483), 4352);
        assert_eq!(whisper_required_mib(3_100_000_000), 4352);
        assert!(whisper_required_mib(4_000_000_000) > 4352);
        assert_eq!(whisper_required_mib(u64::MAX), u32::MAX);
    }

    #[test]
    fn whisper_requires_both_total_capacity_and_free_memory() {
        let gpus = [
            SpeechGpu {
                index: 0,
                total_mib: 8192,
                free_mib: 2048,
            },
            SpeechGpu {
                index: 1,
                total_mib: 2048,
                free_mib: 4096,
            },
            SpeechGpu {
                index: 2,
                total_mib: 4096,
                free_mib: 4096,
            },
        ];
        assert_eq!(select_whisper_gpu(&gpus[..2], 4096), None);
        assert_eq!(select_whisper_gpu(&gpus, 4096), Some(2));
    }

    #[test]
    fn whisper_selects_best_free_adapter_without_pooling() {
        let gpus = [
            SpeechGpu {
                index: 5,
                total_mib: 8192,
                free_mib: 4096,
            },
            SpeechGpu {
                index: 3,
                total_mib: 8192,
                free_mib: 6144,
            },
            SpeechGpu {
                index: 1,
                total_mib: 6144,
                free_mib: 6144,
            },
        ];
        assert_eq!(select_whisper_gpu(&gpus, 4096), Some(1));
        assert_eq!(select_whisper_gpu(&gpus, 6145), None);
    }

    #[cfg(not(feature = "whisper-rs-vulkan"))]
    #[test]
    fn whisper_without_vulkan_cannot_offer_a_gpu() {
        let gpus = whisper_gpus();
        assert!(gpus.is_empty());
        assert_eq!(
            select_whisper_gpu(&gpus, whisper_required_mib(76_000_000)),
            None
        );
    }

    #[test]
    fn parses_nvidia_smi_csv_rows() {
        let gpus = parse_nvidia_smi_csv("0, NVIDIA RTX 5060 Ti, 16376, 12000\nbad\n");

        assert_eq!(
            gpus,
            vec![GpuInfo {
                index: 0,
                name: "NVIDIA RTX 5060 Ti".to_owned(),
                total_mb: 16376,
                free_mb: 12000,
                source: "nvidia-smi",
            }]
        );
    }

    #[test]
    fn estimates_whisper_from_vulkan_and_ollama_from_nvidia_diagnostics() {
        let mut gpus = vec![GpuInfo {
            index: 0,
            name: "NVIDIA GPU".to_owned(),
            total_mb: 10_000,
            free_mb: 5000,
            source: NVIDIA_SMI_SOURCE,
        }];
        let fits = estimate_model_fits(&gpus);
        assert_eq!(fits[0].status, "too-small");
        assert_eq!(fits[1].status, "too-small");
        assert_eq!(fits[2].status, "ok");

        gpus.push(GpuInfo {
            index: 0,
            name: "Whisper Vulkan adapter".to_owned(),
            total_mb: 8192,
            free_mb: 4096,
            source: WHISPER_VULKAN_SOURCE,
        });
        let fits = estimate_model_fits(&gpus);
        assert_eq!(fits[0].status, "ok");
        assert_eq!(fits[1].status, "free-vram");
        assert_eq!(fits[3].status, "free-vram");
        assert_eq!(fits[4].status, "too-small");
    }

    #[test]
    fn model_profiles_no_longer_include_parakeet() {
        // The NVIDIA Parakeet STT profiles are gone. Pin this
        // so a future contributor that re-adds them notices.
        for profile in MODEL_PROFILES {
            assert!(
                !profile.name.to_lowercase().contains("parakeet"),
                "stale Parakeet profile: {}",
                profile.name,
            );
            assert!(
                !profile.setting_hint.contains("parakeet"),
                "stale Parakeet hint: {}",
                profile.setting_hint,
            );
        }
    }

    #[test]
    fn model_profiles_only_recommend_supported_native_devices() {
        for profile in MODEL_PROFILES {
            assert!(
                !profile.setting_hint.contains("VOICEPI_DEVICE=cuda")
                    && !profile.note.to_ascii_lowercase().contains("cuda"),
                "retired CUDA hint remains in {}: {} / {}",
                profile.name,
                profile.setting_hint,
                profile.note
            );
        }
    }

    #[test]
    fn capacity_report_json_shape_contract() {
        let gpus = vec![GpuInfo {
            index: 0,
            name: "RTX Test".to_owned(),
            total_mb: 16_384,
            free_mb: 12_000,
            source: WHISPER_VULKAN_SOURCE,
        }];

        let report = capacity_report_json(&gpus);

        assert_eq!(report.gpus[0].name, "RTX Test");
        assert_eq!(report.models[0].name, "Whisper large-v3-turbo");
        assert_eq!(report.models[0].status, "ok");
    }
}
