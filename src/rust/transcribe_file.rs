//! Public, Python-free file transcription command.
//!
//! The decoder intentionally accepts only 16 kHz mono WAV. Keeping decoding
//! inside the existing `hound` path avoids adding an ffmpeg runtime dependency
//! to the desktop app; errors include the exact conversion command for other
//! formats. Backend construction reuses the same local whisper.cpp and
//! OpenAI-compatible cloud implementations as live Rust dictation.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::dictate::backends::cloud_transcribe::{
    cloud_backend_local_only_checked, CloudTranscribeConfig, STT_BACKEND_CLOUD, STT_BACKEND_ENV,
};
use crate::dictate::provenance::ENGINE_RUST_IN_PROCESS;
use crate::dictate::{CloudTranscribeBackend, TranscribeBackend, TranscribeResult};
use crate::dictionary::{ReplacementChange, SessionDictionary};
use crate::postprocess::{postprocess_text, PostprocessSettings, RedactionSummary};

const WAV_CONVERSION_HINT: &str =
    "convert it first with: ffmpeg -i INPUT -ac 1 -ar 16000 OUTPUT.wav";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfiguredBackend {
    Whisper,
    Cloud,
}

impl ConfiguredBackend {
    pub(crate) fn from_value(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "whisper" => Ok(Self::Whisper),
            STT_BACKEND_CLOUD => Ok(Self::Cloud),
            other => Err(anyhow!(
                "unsupported stt_backend {other:?}; expected whisper or openai"
            )),
        }
    }

    fn from_env() -> Result<Self> {
        Self::from_value(&std::env::var(STT_BACKEND_ENV).unwrap_or_else(|_| "whisper".to_owned()))
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::Cloud => STT_BACKEND_CLOUD,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TranscribeFileReport {
    pub ts: f64,
    pub event: &'static str,
    pub engine: &'static str,
    pub stt_backend: &'static str,
    pub stt_impl: String,
    pub stt_accel: String,
    pub text: String,
    pub text_preview: String,
    pub text_chars: usize,
    pub raw_text: String,
    pub dictionary_text: String,
    pub dictionary_terms: Vec<String>,
    pub dictionary_replacements: Vec<ReplacementChange>,
    pub source_file: String,
    pub recording_s: f64,
    pub audio_duration_s: f64,
    pub post_boost_dbfs: Option<f64>,
    pub audio_raw_dbfs: Option<f64>,
    pub audio_peak: Option<f64>,
    pub audio_gain: Option<f64>,
    pub audio_noise_dbfs: Option<f64>,
    pub audio_snr_db: Option<f64>,
    pub audio_input_status: Option<String>,
    pub compute_s: f64,
    pub real_time_factor: f64,
    pub language: String,
    pub language_probability: f64,
    pub gate: Option<String>,
    pub model: String,
    pub device: String,
    pub compute_type: String,
    pub segments: Vec<serde_json::Value>,
    pub post_processor: String,
    pub post_mode: String,
    pub post_model: String,
    pub post_changed: bool,
    pub post_fallback: bool,
    pub post_error: Option<String>,
    pub post_latency_ms: u64,
    pub post_redacted: bool,
    pub post_redactions: Vec<RedactionSummary>,
}

/// CLI entry point. Configuration is materialised into the current process so
/// the existing backend constructors see the same values as the managed live
/// runtime, including credentials saved through Settings.
pub fn handle(path: &Path, json: bool) -> Result<()> {
    materialize_runtime_environment();
    let configured = ConfiguredBackend::from_env()?;
    let dictionary = crate::dictionary::load_session_dictionary();
    let backend = build_backend(configured, &dictionary)?;
    let mut post_settings = crate::postprocess::settings_from_env();
    let report = transcribe_path(
        path,
        configured,
        backend.as_ref(),
        &dictionary,
        &mut post_settings,
    )?;
    write_report(&mut std::io::stdout().lock(), &report, json)
}

fn materialize_runtime_environment() {
    for (name, value) in crate::config::worker_env_overrides() {
        std::env::set_var(name, value);
    }
    crate::runtime::cloud_api_keys::attach_cloud_api_keys_to_current_process();
}

fn build_backend(
    configured: ConfiguredBackend,
    dictionary: &SessionDictionary,
) -> Result<Box<dyn TranscribeBackend>> {
    match configured {
        ConfiguredBackend::Cloud => {
            let config = CloudTranscribeConfig::from_env();
            let local_only = crate::whisper::model_manager::is_local_only();
            let backend = build_cloud_backend(config, dictionary, local_only)?;
            Ok(Box::new(backend))
        }
        ConfiguredBackend::Whisper => build_local_backend(dictionary),
    }
}

pub(crate) fn build_cloud_backend(
    mut config: CloudTranscribeConfig,
    dictionary: &SessionDictionary,
    local_only: bool,
) -> Result<CloudTranscribeBackend> {
    if config.model.trim().is_empty() {
        return Err(anyhow!(
            "cloud transcription requires a configured stt_model"
        ));
    }
    if config.api_key.trim().is_empty() {
        return Err(anyhow!(
            "cloud transcription requires a saved API key or \
             VOICEPI_STT_API_KEY/GROQ_API_KEY/OPENAI_API_KEY"
        ));
    }
    config.prompt = prompt_for(dictionary, config.prompt.as_deref());
    cloud_backend_local_only_checked(local_only, config)
        .map_err(|error| anyhow!("cloud backend rejected: {error}"))
}

#[cfg(feature = "whisper-rs-local")]
fn build_local_backend(dictionary: &SessionDictionary) -> Result<Box<dyn TranscribeBackend>> {
    use crate::dictate::backends::whisper_local::WhisperBackendConfig;
    use crate::dictate::backends::WhisperLocalTranscribeBackend;
    use crate::whisper::{parse_idle_timeout_from_env, IdleUnloadingModel};

    let model_path = crate::whisper::dispatch::resolve_model_path_from_env()
        .context("resolve local Whisper model")?;
    let idle = parse_idle_timeout_from_env().context("parse Whisper idle timeout")?;
    let model = IdleUnloadingModel::for_local_whisper(model_path, idle);
    let language = nonempty_env("VOICEPI_LANG");
    let mut initial_prompt = nonempty_env("VOICEPI_INITIAL_PROMPT");
    initial_prompt = prompt_for(dictionary, initial_prompt.as_deref());
    let config = WhisperBackendConfig {
        language,
        initial_prompt,
    };
    Ok(Box::new(WhisperLocalTranscribeBackend::new(model, config)))
}

#[cfg(not(feature = "whisper-rs-local"))]
fn build_local_backend(_dictionary: &SessionDictionary) -> Result<Box<dyn TranscribeBackend>> {
    Err(anyhow!(
        "local file transcription requires a shipping build with local Whisper support \
         (cargo feature whisper-rs-local); this command will not fall back to Python"
    ))
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn validate_input_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(anyhow!("input file does not exist: {}", path.display()));
    }
    if !path.is_file() {
        return Err(anyhow!("input path is not a file: {}", path.display()));
    }
    let is_wav = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"));
    if !is_wav {
        return Err(anyhow!(
            "unsupported audio format for {}: only 16 kHz mono WAV is supported; {WAV_CONVERSION_HINT}",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn prompt_for(
    dictionary: &SessionDictionary,
    base_prompt: Option<&str>,
) -> Option<String> {
    dictionary.initial_prompt(base_prompt)
}

pub(crate) fn transcribe_path(
    path: &Path,
    configured: ConfiguredBackend,
    backend: &dyn TranscribeBackend,
    dictionary: &SessionDictionary,
    post_settings: &mut PostprocessSettings,
) -> Result<TranscribeFileReport> {
    validate_input_path(path)?;
    let pcm = crate::whisper::decode_wav_16k_mono(path).map_err(|error| {
        anyhow!(
            "cannot decode {}: {error:#}; supported input is 16 kHz mono WAV; \
             {WAV_CONVERSION_HINT}",
            path.display()
        )
    })?;
    let result = backend
        .transcribe(&pcm, crate::whisper::WHISPER_SAMPLE_RATE_HZ)
        .map_err(|error| anyhow!("{error}"))?;
    build_report(path, configured, result, dictionary, post_settings)
}

fn build_report(
    path: &Path,
    configured: ConfiguredBackend,
    result: TranscribeResult,
    dictionary: &SessionDictionary,
    post_settings: &mut PostprocessSettings,
) -> Result<TranscribeFileReport> {
    let raw_text = if result.raw_text.is_empty() {
        result.text.clone()
    } else {
        result.raw_text.clone()
    };
    let (dictionary_text, dictionary_replacements) = dictionary
        .dictionary
        .apply_replacements(&result.text)
        .context("apply dictionary replacements")?;
    if !result.language.trim().is_empty() {
        post_settings.lang.clone_from(&result.language);
    }
    let post = postprocess_text(&dictionary_text, post_settings);
    let text_preview = compact_text(&post.text, 240);
    let text_chars = post.text.chars().count();
    let compute_s = result.latency_ms as f64 / 1_000.0;
    let real_time_factor = if result.duration_s > 0.0 {
        compute_s / result.duration_s
    } else {
        0.0
    };
    let model = match configured {
        ConfiguredBackend::Whisper => nonempty_env("VOICEPI_MODEL"),
        ConfiguredBackend::Cloud => nonempty_env("VOICEPI_STT_MODEL"),
    }
    .unwrap_or_default();
    let dictionary_terms = dictionary
        .dictionary
        .prompt_terms(dictionary.max_terms, dictionary.max_chars);
    Ok(TranscribeFileReport {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
        event: "file_transcription",
        engine: ENGINE_RUST_IN_PROCESS,
        stt_backend: configured.as_str(),
        stt_impl: result.stt_impl,
        stt_accel: result.stt_accel,
        text: post.text,
        text_preview,
        text_chars,
        raw_text,
        dictionary_text,
        dictionary_terms,
        dictionary_replacements,
        source_file: path.display().to_string(),
        recording_s: result.duration_s,
        audio_duration_s: result.duration_s,
        post_boost_dbfs: None,
        audio_raw_dbfs: None,
        audio_peak: None,
        audio_gain: None,
        audio_noise_dbfs: None,
        audio_snr_db: None,
        audio_input_status: None,
        compute_s,
        real_time_factor,
        language: result.language,
        language_probability: result.language_probability,
        gate: result.gate,
        model,
        device: nonempty_env("VOICEPI_DEVICE").unwrap_or_default(),
        compute_type: nonempty_env("VOICEPI_COMPUTE_TYPE").unwrap_or_default(),
        segments: Vec::new(),
        post_processor: post.provider,
        post_mode: post.mode,
        post_model: post.model,
        post_changed: post.changed,
        post_fallback: post.fallback,
        post_error: (!post.error.is_empty()).then_some(post.error),
        post_latency_ms: post.latency_ms,
        post_redacted: post.redacted,
        post_redactions: post.redactions,
    })
}

fn compact_text(text: &str, limit: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let keep = limit.saturating_sub(3);
    format!("{}...", compact.chars().take(keep).collect::<String>())
}

pub(crate) fn write_report(
    writer: &mut dyn Write,
    report: &TranscribeFileReport,
    json: bool,
) -> Result<()> {
    if json {
        serde_json::to_writer(&mut *writer, report).context("serialize transcription report")?;
        writeln!(writer).context("write transcription report")?;
    } else {
        writeln!(writer, "{}", report.text).context("write transcript")?;
    }
    Ok(())
}
