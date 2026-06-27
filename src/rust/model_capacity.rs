use std::process::Command;

use anyhow::Result;
use serde::Serialize;

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
        required_free_mb: 1800,
        setting_hint: "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3-turbo; VOICEPI_COMPUTE_TYPE=int8_float16",
        note: "Fast default; best fit for small GPUs and CPU fallback.",
    },
    ModelProfile {
        name: "Whisper large-v3 quantized",
        category: "stt",
        required_free_mb: 3200,
        setting_hint: "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3; VOICEPI_COMPUTE_TYPE=int8_float16",
        note: "Full Whisper model with quantized GPU compute.",
    },
    ModelProfile {
        name: "Whisper large-v3 float16",
        category: "stt",
        required_free_mb: 5000,
        setting_hint: "VOICEPI_STT_BACKEND=whisper; VOICEPI_MODEL=large-v3; VOICEPI_COMPUTE_TYPE=float16",
        note: "Higher-quality Whisper path for GPUs with enough headroom.",
    },
    ModelProfile {
        name: "Whisper large-v3 float16 high beam",
        category: "stt",
        required_free_mb: 8000,
        setting_hint: "VOICEPI_MODEL=large-v3; VOICEPI_COMPUTE_TYPE=float16; VOICEPI_BEAM_SIZE=10",
        note: "Useful for hard audio; beam past 16 has diminishing returns.",
    },
    // Wave 8 of #348 removed the NVIDIA Parakeet STT entries from this
    // table along with the backend itself; only Whisper STT and Ollama
    // post-processing profiles remain.
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
    if json {
        println!("{}", serde_json::to_string(&capacity_report_json(&gpus))?);
    } else {
        println!("{}", capacity_report(&gpus));
    }
    Ok(())
}

pub fn query_gpus() -> Vec<GpuInfo> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=index,name,memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_nvidia_smi_csv(&String::from_utf8_lossy(&output.stdout))
}

pub fn estimate_model_fits(gpus: &[GpuInfo]) -> Vec<ModelFit> {
    let best_total = gpus.iter().map(|gpu| gpu.total_mb).max().unwrap_or(0);
    let best_free = gpus.iter().map(|gpu| gpu.free_mb).max().unwrap_or(0);
    MODEL_PROFILES
        .iter()
        .cloned()
        .map(|profile| {
            let required = profile.required_free_mb;
            let (status, detail) = if best_free >= required {
                (
                    "ok",
                    format!("fits now; needs about {required} MB free VRAM"),
                )
            } else if best_total >= required {
                (
                    "free-vram",
                    format!(
                        "GPU is large enough, but only {best_free} MB is free; stop other GPU processes to reach about {required} MB"
                    ),
                )
            } else {
                (
                    "too-small",
                    format!(
                        "needs about {required} MB free VRAM; largest GPU has {best_total} MB total"
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
        lines.push("GPU capacity: no NVIDIA CUDA GPU detected".to_owned());
    } else {
        lines.push("GPU capacity:".to_owned());
        for gpu in gpus {
            lines.push(format!(
                "  [{}] {}: {} MB free / {} MB total ({})",
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
            "  {marker:<9} {:<34} ~{} MB  {}",
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
        source: "nvidia-smi",
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
    fn estimates_model_fit_from_free_and_total_vram() {
        // Profiles after the Wave 8 #348 Parakeet removal, in declaration order
        // with a 10 000 MB GPU that has 4 000 MB free:
        //   0: Whisper large-v3-turbo        (1.8 GB) — fits free  → "ok"
        //   1: Whisper large-v3 quantized    (3.2 GB) — fits free  → "ok"
        //   2: Whisper large-v3 float16      (5.0 GB) — fits total → "free-vram"
        //   3: Whisper large-v3 fp16 hi-beam (8.0 GB) — fits total → "free-vram"
        //   4: Ollama Qwen2.5 3B             (4.5 GB) — fits total → "free-vram"
        //   5: Ollama Qwen2.5 7B Q4          (8.0 GB) — fits total → "free-vram"
        //   6: Ollama Qwen2.5 14B Q4         (14  GB) — too small  → "too-small"
        let gpus = vec![GpuInfo {
            index: 0,
            name: "GPU".to_owned(),
            total_mb: 10_000,
            free_mb: 4_000,
            source: "test",
        }];

        let fits = estimate_model_fits(&gpus);

        assert_eq!(fits[0].status, "ok");
        assert_eq!(fits[1].status, "ok");
        assert_eq!(fits[2].status, "free-vram");
        assert_eq!(fits[6].status, "too-small");
    }

    #[test]
    fn model_profiles_no_longer_include_parakeet() {
        // Wave 8 of #348 dropped the NVIDIA Parakeet STT profiles. Pin this
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
    fn capacity_report_json_shape_matches_python_contract() {
        let gpus = vec![GpuInfo {
            index: 0,
            name: "RTX Test".to_owned(),
            total_mb: 16_384,
            free_mb: 12_000,
            source: "test",
        }];

        let report = capacity_report_json(&gpus);

        assert_eq!(report.gpus[0].name, "RTX Test");
        assert_eq!(report.models[0].name, "Whisper large-v3-turbo");
        assert_eq!(report.models[0].status, "ok");
    }
}
