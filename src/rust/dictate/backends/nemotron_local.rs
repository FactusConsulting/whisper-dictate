//! In-process Nemotron 3.5 backend backed by the NeMo-Speech.cpp C ABI.
//!
//! This is deliberately separate from [`super::cloud_transcribe`].  A local
//! GGUF model is decoded in this process and never needs an API key, Docker,
//! NIM, or an HTTP/gRPC listener.  The same speech gate, dictionary reload,
//! hallucination filter, and profile language overrides used by local Whisper
//! are retained here so switching execution mode does not change the session
//! contract.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};

use super::hallucination::{finalize_transcript, TranscriptionGuards};
use super::nemotron_assets::{
    custom_model_size, ensure_model_path_while, ensure_verified_runtime, model_path_for_request,
    Accelerator,
};
use super::nemotron_ffi::{NativeRecognizer, NativeRuntime};
use super::profile_overrides::ProfileOverrideSlots;
use crate::dictate::{TranscribeBackend, TranscribeError, TranscribeResult};
use crate::model_capacity::{
    best_free_mib, query_gpus, vulkan::Adapters, GpuInfo, GpuMemoryShortfall,
};
use crate::whisper::device_options::nemotron_required_mib;
use crate::whisper::IdleUnloadingModel;

/// Settings needed to plan a local model and resolve a pinned GPU runtime
/// when its recognizer is loaded or reloaded after an idle unload.
#[derive(Debug, Clone)]
pub struct NemotronLocalBackendConfig {
    pub model_path: PathBuf,
    pub language: Option<String>,
    pub initial_prompt: Option<String>,
    pub(crate) local_only: bool,
    pub(crate) model_request: String,
    pub(crate) library_override: Option<String>,
    device: DeviceRequest,
    /// The runtime supervisor owns this gate and clears it before teardown.
    /// First-run asset downloads use it as a cancellation token.
    pub(crate) runtime_active: Arc<AtomicBool>,
}

/// Production Nemotron backend.  `NativeRecognizer` is created lazily on the
/// first utterance and can be unloaded after the shared Whisper idle timeout.
pub struct NemotronLocalTranscribeBackend {
    model: Arc<IdleUnloadingModel<NativeRecognizer>>,
    config: NemotronLocalBackendConfig,
    resolved_accel: Arc<Mutex<&'static str>>,
    /// Per-utterance profile overrides, live-reloading prompt and
    /// session-bound guards -- see [`ProfileOverrideSlots`]. The resident
    /// GGUF is fixed at construction, so a `model` override only prints
    /// the one-shot restart diagnostic.
    overrides: ProfileOverrideSlots,
    static_prompt_terms: Vec<String>,
}

impl NemotronLocalTranscribeBackend {
    pub fn new(
        config: NemotronLocalBackendConfig,
        idle_timeout: Option<std::time::Duration>,
    ) -> Self {
        let config_for_loader = config.clone();
        let resolved_accel = Arc::new(Mutex::new("unknown"));
        let resolved_accel_for_loader = Arc::clone(&resolved_accel);
        let model = IdleUnloadingModel::new(
            move || {
                let (recognizer, accel) = load_native_recognizer(&config_for_loader)?;
                *resolved_accel_for_loader
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = accel;
                Ok(recognizer)
            },
            idle_timeout,
        );
        Self {
            model: Arc::new(model),
            config,
            resolved_accel,
            overrides: ProfileOverrideSlots::new(
                "the resident Nemotron model cannot swap mid-session",
            ),
            static_prompt_terms: Vec::new(),
        }
    }

    /// Probe the native runtime and model without recording.  The C ABI loads
    /// the GGUF weights during recognizer creation, so a successful return is
    /// a meaningful local equivalent of a cloud API check.
    pub fn check_configuration(config: &NemotronLocalBackendConfig) -> Result<()> {
        let (_recognizer, _accel) = load_native_recognizer(config)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn with_reloading_prompt_settings(
        mut self,
        settings: crate::dictionary::RuntimeDictionarySettings,
    ) -> Self {
        self.overrides
            .set_prompt_reload(crate::dictionary::ReloadingDictionary::from_settings(
                settings,
            ));
        self
    }

    /// Attach bounded dictionary terms for one-shot callers such as benchmark
    /// and file transcription, which already own a loaded session dictionary.
    /// NeMo receives each term as an individual speech-context phrase.
    #[allow(dead_code)]
    pub(crate) fn with_static_prompt_terms(mut self, terms: Vec<String>) -> Self {
        self.static_prompt_terms = terms;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn with_transcription_guards(self, guards: TranscriptionGuards) -> Self {
        self.overrides.set_guards(guards);
        self
    }

    fn effective_guards(&self) -> TranscriptionGuards {
        self.overrides.guards()
    }

    fn effective_language(&self) -> Option<String> {
        let configured = self
            .overrides
            .language_or(self.config.language.clone())
            .unwrap_or_default();
        Some(language_for_model(&configured, &self.config.model_path))
    }

    fn effective_prompt(&self) -> (Option<String>, Vec<String>) {
        self.overrides.prompt(
            self.config.initial_prompt.as_deref(),
            &self.static_prompt_terms,
        )
    }

    pub fn config(&self) -> &NemotronLocalBackendConfig {
        &self.config
    }
}

impl TranscribeBackend for NemotronLocalTranscribeBackend {
    fn transcribe(
        &self,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<TranscribeResult, TranscribeError> {
        let guards = self.effective_guards();
        let (audio, duration_s, metrics) =
            match crate::audio_dsp::prepare_for_transcription(pcm, sample_rate, &guards.thresholds)
            {
                crate::audio_dsp::PreparedAudio::Reject {
                    reason,
                    duration_s,
                    metrics,
                } => {
                    return Ok(TranscribeResult {
                        text: String::new(),
                        gate: Some(reason),
                        duration_s,
                        audio_metrics: Some(metrics),
                        ..Default::default()
                    });
                }
                crate::audio_dsp::PreparedAudio::Decode {
                    audio,
                    duration_s,
                    metrics,
                } => (audio, duration_s, metrics),
            };
        let language = self.effective_language();
        let request_language = language.as_deref().unwrap_or("auto");
        let (prompt, dictionary_terms) = self.effective_prompt();
        let prompt = prompt.as_deref().filter(|value| !value.trim().is_empty());
        let start = Instant::now();
        let result = self
            .model
            .with_model(|recognizer| {
                recognizer.recognize(
                    &audio,
                    sample_rate,
                    request_language,
                    prompt,
                    &dictionary_terms,
                )
            })
            .map_err(|error| {
                TranscribeError::from_backend_error(error).with_audio(metrics.clone())
            })?;
        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (text, is_hallucination) =
            finalize_transcript(&result.text, duration_s, guards.max_chars_per_second);
        let stt_accel = self
            .resolved_accel
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .to_owned();
        Ok(TranscribeResult {
            raw_text: result.text,
            dictionary_terms: (!dictionary_terms.is_empty())
                .then(|| dictionary_terms.into_boxed_slice()),
            text,
            is_hallucination,
            latency_ms,
            duration_s,
            audio_metrics: Some(metrics),
            language: result
                .language
                .or_else(|| language.and_then(|value| language_result_label(&value)))
                .unwrap_or_default(),
            stt_impl: crate::dictate::provenance::STT_IMPL_NEMOTRON_LOCAL.to_owned(),
            stt_accel,
            ..Default::default()
        })
    }

    fn apply_profile_overrides(&self, settings: &std::collections::BTreeMap<String, String>) {
        self.overrides.apply(settings);
    }
}

/// Map the compact values persisted by the settings UI to the regional codes
/// expected by the multilingual Nemotron checkpoint.  `auto` is explicit so
/// the model performs language identification rather than inheriting a stale
/// process-level locale.
fn language_for_model(language: &str, model_path: &Path) -> String {
    let raw = language.trim().replace('_', "-");
    if is_english_model_path(model_path)
        && !raw.is_empty()
        && !raw.eq_ignore_ascii_case("auto")
        && !raw.eq_ignore_ascii_case("multi")
        && !raw.eq_ignore_ascii_case("en")
        && !raw.to_ascii_lowercase().starts_with("en-")
    {
        return "en-US".to_owned();
    }
    if raw.is_empty() || raw.eq_ignore_ascii_case("auto") || raw.eq_ignore_ascii_case("multi") {
        return if is_english_model_path(model_path) {
            "en-US".to_owned()
        } else {
            "auto".to_owned()
        };
    }
    if raw.contains('-') {
        return raw;
    }
    crate::cloud_api::grpc_transcribe::normalize_riva_locale(&raw)
}

fn language_result_label(language: &str) -> Option<String> {
    let compact = language.split('-').next().unwrap_or(language).trim();
    (!compact.is_empty()
        && !compact.eq_ignore_ascii_case("auto")
        && !compact.eq_ignore_ascii_case("multi"))
    .then(|| compact.to_owned())
}

fn is_english_model_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase().contains("speech-streaming-en"))
        .unwrap_or(false)
}

/// Build the local configuration from the existing Nemotron Speech-tab
/// fields. `stt_model` accepts an official model id (resolved to the verified
/// per-user cache) or an existing GGUF path in `inproc://nemotron` mode; cloud
/// Nemotron continues to use its model id unchanged.
pub(crate) fn config_from_settings(
    model: &str,
    device: &str,
    language: Option<String>,
    initial_prompt: Option<String>,
    library_override: Option<&str>,
    local_only: bool,
    runtime_active: Arc<AtomicBool>,
) -> Result<NemotronLocalBackendConfig> {
    let device = DeviceRequest::parse(device)?;
    let model_request = model.trim().to_owned();
    let library_override = library_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let model_path = model_path_for_request(&model_request)?;
    Ok(NemotronLocalBackendConfig {
        model_path,
        language,
        initial_prompt,
        local_only,
        model_request,
        library_override,
        device,
        runtime_active,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceRequest {
    Auto,
    Explicit(Accelerator),
}

impl DeviceRequest {
    fn parse(value: &str) -> Result<Self> {
        let device = value.trim();
        let request = match device.to_ascii_lowercase().as_str() {
            "auto" => return Ok(Self::Auto),
            "vulkan" => Accelerator::Vulkan,
            "cuda" => Accelerator::Cuda,
            "cpu" => anyhow::bail!(
                "CPU inference is not allowed for local Nemotron; choose a GPU or a hosted speech provider"
            ),
            _ => anyhow::bail!("device value {device:?} is not supported by local Nemotron"),
        };
        if !request.available() {
            anyhow::bail!(
                "device value {device:?} is not supported by local Nemotron on this platform"
            );
        }
        Ok(Self::Explicit(request))
    }

    fn candidates(self) -> [Option<Accelerator>; 2] {
        match self {
            Self::Auto => [Some(Accelerator::Vulkan), Some(Accelerator::Cuda)],
            Self::Explicit(accelerator) => [Some(accelerator), None],
        }
    }
}

pub(crate) fn gpu_memory(model: &str, device: &str) -> Result<(u32, Option<u32>)> {
    gpu_memory_with(model, device, probe_free_mib)
}

fn gpu_memory_with(
    model: &str,
    device: &str,
    probe: impl FnOnce(DeviceRequest, u32) -> Option<u32>,
) -> Result<(u32, Option<u32>)> {
    let device = DeviceRequest::parse(device)?;
    let model = model.trim();
    let path = model_path_for_request(model)?;
    let required_mib = nemotron_required_mib(custom_model_size(model, &path)?);
    Ok((required_mib, probe(device, required_mib)))
}

fn probe_free_mib(device: DeviceRequest, required_mib: u32) -> Option<u32> {
    probe_free_mib_with(
        device,
        required_mib,
        driver_enabled,
        Adapters::probe,
        query_gpus,
    )
}

fn probe_free_mib_with(
    device: DeviceRequest,
    required_mib: u32,
    enabled: impl Fn(Accelerator) -> bool,
    probe_vulkan: impl Fn() -> Adapters,
    query_nvidia: impl Fn() -> Vec<GpuInfo>,
) -> Option<u32> {
    let nvidia = std::cell::OnceCell::new();
    let query = || nvidia.get_or_init(&query_nvidia);
    device
        .candidates()
        .into_iter()
        .flatten()
        .filter(|&accel| accel.available() && enabled(accel))
        .filter_map(|accel| match accel {
            Accelerator::Vulkan => probe_vulkan()
                .with_nvidia_free_limit(query)
                .best_free_mib(required_mib),
            Accelerator::Cuda => cuda_best_free_mib(query(), required_mib),
        })
        .max()
}

#[cfg(native_session)]
pub(crate) fn check_gpu_capacity(config: &NemotronLocalBackendConfig) -> Result<()> {
    check_gpu_capacity_with(config, probe_free_mib)
}

#[cfg(any(native_session, test))]
fn check_gpu_capacity_with(
    config: &NemotronLocalBackendConfig,
    probe: impl FnOnce(DeviceRequest, u32) -> Option<u32>,
) -> Result<()> {
    let required_mib = nemotron_required_mib(custom_model_size(
        &config.model_request,
        &config.model_path,
    )?);
    let free_mib = probe(config.device, required_mib);
    match free_mib {
        Some(free_mib) if free_mib >= required_mib => Ok(()),
        Some(free_mib) => Err(GpuMemoryShortfall { required_mib, free_mib }.into()),
        None => anyhow::bail!(
            "Local Nemotron needs a supported GPU with at least {required_mib} MiB. Choose a smaller local model or a hosted speech provider."
        ),
    }
}

enum GpuCapacity {
    Vulkan(Adapters),
    Cuda(Vec<GpuInfo>),
}

impl GpuCapacity {
    fn probe(accelerator: Accelerator) -> Self {
        match accelerator {
            Accelerator::Vulkan => Self::Vulkan(Adapters::probe_for_model_load(query_gpus)),
            Accelerator::Cuda => Self::Cuda(query_gpus()),
        }
    }

    fn best_free_mib(&self, required_mib: u32) -> Option<u32> {
        match self {
            Self::Vulkan(adapters) => adapters.best_free_mib(required_mib),
            Self::Cuda(gpus) => cuda_best_free_mib(gpus, required_mib),
        }
    }

    fn capacity(&self, description: &std::ffi::CStr) -> Option<(u32, u32)> {
        match self {
            Self::Vulkan(adapters) => adapters.capacity(description, None),
            Self::Cuda(gpus) => cuda_capacity(gpus, description.to_bytes()),
        }
    }
}

fn cuda_best_free_mib(gpus: &[GpuInfo], required_mib: u32) -> Option<u32> {
    best_free_mib(
        gpus.iter().map(|gpu| (gpu.total_mb, gpu.free_mb)),
        required_mib,
    )
}

/// Maximum total memory among devices the pinned local runtime can try.
pub(crate) fn local_gpu_total_mib(adapters: &Adapters) -> Option<u32> {
    max_local_gpu_total_mib(driver_enabled, || adapters.max_total_mib(), query_gpus)
}

fn max_local_gpu_total_mib(
    enabled: impl Fn(Accelerator) -> bool,
    vulkan_total: impl FnOnce() -> u32,
    cuda_gpus: impl FnOnce() -> Vec<GpuInfo>,
) -> Option<u32> {
    let vulkan = Accelerator::Vulkan;
    let vulkan_mib = if vulkan.available() && enabled(vulkan) {
        vulkan_total()
    } else {
        0
    };
    let cuda = Accelerator::Cuda;
    let cuda_mib = if cuda.available() && enabled(cuda) {
        cuda_gpus()
            .into_iter()
            .map(|gpu| gpu.total_mb)
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let total = vulkan_mib.max(cuda_mib);
    (total > 0).then_some(total)
}

fn driver_enabled(accelerator: Accelerator) -> bool {
    driver_enabled_with(accelerator, std::env::var_os)
}

fn driver_enabled_with(
    accelerator: Accelerator,
    lookup: impl FnOnce(&'static str) -> Option<std::ffi::OsString>,
) -> bool {
    let variable = match accelerator {
        Accelerator::Vulkan => "GGML_VK_VISIBLE_DEVICES",
        Accelerator::Cuda => "CUDA_VISIBLE_DEVICES",
    };
    !lookup(variable).is_some_and(|value| {
        value.is_empty() || (accelerator == Accelerator::Cuda && value == "-1")
    })
}

fn cuda_capacity(gpus: &[GpuInfo], description: &[u8]) -> Option<(u32, u32)> {
    // NVML's nvidia-smi index is not guaranteed to equal a CUDA ordinal.
    // When multiple GPUs share a description, every possible match must fit.
    gpus.iter()
        .filter(|gpu| gpu.name.as_bytes() == description)
        .map(|gpu| (gpu.total_mb, gpu.free_mb))
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
}

fn load_native_recognizer(
    config: &NemotronLocalBackendConfig,
) -> Result<(NativeRecognizer, &'static str)> {
    load_recognizer_with(
        config,
        driver_enabled,
        GpuCapacity::probe,
        |accelerator| {
            let verified = ensure_verified_runtime(
                config.library_override.as_deref(),
                accelerator,
                config.local_only,
                config.runtime_active.as_ref(),
            )?;
            NativeRuntime::load(&verified)
        },
        |runtime, required_mib, capacity| {
            runtime.select_gpu(required_mib, |name| capacity.capacity(name))
        },
        NativeRuntime::recognize_with,
    )
}

fn load_recognizer_with<R, T>(
    config: &NemotronLocalBackendConfig,
    enabled: impl Fn(Accelerator) -> bool,
    mut probe: impl FnMut(Accelerator) -> GpuCapacity,
    mut load_runtime: impl FnMut(Accelerator) -> Result<R>,
    select_gpu: impl Fn(&R, u32, &GpuCapacity) -> Result<i32>,
    mut create: impl FnMut(R, &Path, i32) -> Result<T>,
) -> Result<(T, &'static str)> {
    let planned_model = model_path_for_request(&config.model_request)?;
    let required_mib =
        nemotron_required_mib(custom_model_size(&config.model_request, &planned_model)?);
    let mut failure = None;
    let mut best_free = None;
    for accelerator in config.device.candidates().into_iter().flatten() {
        if !config.runtime_active.load(Ordering::Acquire) {
            anyhow::bail!("loading local Nemotron was cancelled because the runtime stopped");
        }
        if !accelerator.available() || !enabled(accelerator) {
            continue;
        }
        let capacity = probe(accelerator);
        let Some(free_mib) = capacity.best_free_mib(required_mib) else {
            continue;
        };
        best_free = best_free.max(Some(free_mib));
        if free_mib < required_mib {
            continue;
        }
        let runtime = load_runtime(accelerator).and_then(|runtime| {
            select_gpu(&runtime, required_mib, &capacity)?;
            Ok(runtime)
        });
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                failure = Some(error);
                continue;
            }
        };
        let model_path = ensure_model_path_while(
            &config.model_request,
            config.local_only,
            config.runtime_active.as_ref(),
        )?;
        if !config.runtime_active.load(Ordering::Acquire) {
            anyhow::bail!("loading local Nemotron was cancelled because the runtime stopped");
        }
        let required_mib =
            nemotron_required_mib(custom_model_size(&config.model_request, &model_path)?);
        let capacity = probe(accelerator);
        if let Some(free_mib) = capacity
            .best_free_mib(required_mib)
            .filter(|&free| free < required_mib)
        {
            failure = Some(
                GpuMemoryShortfall {
                    required_mib,
                    free_mib,
                }
                .into(),
            );
            continue;
        }
        let recognizer = select_gpu(&runtime, required_mib, &capacity).and_then(|gpu| {
            create(runtime, &model_path, gpu).with_context(|| {
                format!(
                    "initialize local Nemotron on {} GPU {gpu}",
                    accelerator.label()
                )
            })
        });
        match recognizer {
            Ok(recognizer) => return Ok((recognizer, accelerator.label())),
            Err(error) => failure = Some(error),
        }
    }
    let message = || {
        format!(
            "local Nemotron requires a verified GPU runtime and one GPU with at least {required_mib} MiB total and free VRAM; choose a hosted speech provider if no suitable GPU is available"
        )
    };
    match (failure, best_free) {
        (Some(error), _) => Err(error).with_context(message),
        (None, Some(free_mib)) if free_mib < required_mib => Err(GpuMemoryShortfall {
            required_mib,
            free_mib,
        }
        .into()),
        (None, _) => Err(anyhow::anyhow!(message())),
    }
}

#[cfg(test)]
#[path = "nemotron_local_tests.rs"]
mod tests;
