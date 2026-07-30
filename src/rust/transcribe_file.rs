//! Public, Python-free file transcription command.
//!
//! The decoder intentionally accepts only 16 kHz mono WAV. Keeping decoding
//! inside the existing `hound` path avoids adding an ffmpeg runtime dependency
//! to the desktop app; errors include the exact conversion command for other
//! formats. Backend construction reuses the same local whisper.cpp and
//! OpenAI-compatible cloud implementations as live Rust dictation.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::dictate::backends::cloud_transcribe::{
    cloud_backend_local_only_checked, CloudTranscribeConfig, STT_BACKEND_CLOUD, STT_BACKEND_ENV,
};
use crate::dictate::provenance::ENGINE_RUST_IN_PROCESS;
use crate::dictate::{TranscribeBackend, TranscribeResult};
use crate::dictionary::{ReplacementChange, SessionDictionary};
use crate::postprocess::{postprocess_text, PostprocessSettings};

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
    pub event: &'static str,
    pub engine: &'static str,
    pub backend: &'static str,
    pub stt_impl: String,
    pub stt_accel: String,
    pub text: String,
    pub raw_text: String,
    pub dictionary_text: String,
    pub dictionary_replacements: Vec<ReplacementChange>,
    pub source_file: String,
    pub duration_s: f64,
    pub latency_ms: u64,
    pub language: String,
    pub language_probability: f64,
    pub gate: Option<String>,
    pub is_hallucination: bool,
    pub post_processor: String,
    pub post_mode: String,
    pub post_changed: bool,
    pub post_fallback: bool,
    pub post_error: String,
    pub post_latency_ms: u64,
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
            let mut config = CloudTranscribeConfig::from_env();
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
            let local_only = crate::whisper::model_manager::is_local_only();
            let backend = cloud_backend_local_only_checked(local_only, config)
                .map_err(|error| anyhow!("cloud backend rejected: {error}"))?;
            Ok(Box::new(backend))
        }
        ConfiguredBackend::Whisper => build_local_backend(dictionary),
    }
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

#[cfg(feature = "whisper-rs-local")]
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
    Ok(TranscribeFileReport {
        event: "file_transcription",
        engine: ENGINE_RUST_IN_PROCESS,
        backend: configured.as_str(),
        stt_impl: result.stt_impl,
        stt_accel: result.stt_accel,
        text: post.text,
        raw_text,
        dictionary_text,
        dictionary_replacements,
        source_file: path.display().to_string(),
        duration_s: result.duration_s,
        latency_ms: result.latency_ms,
        language: result.language,
        language_probability: result.language_probability,
        gate: result.gate,
        is_hallucination: result.is_hallucination,
        post_processor: post.provider,
        post_mode: post.mode,
        post_changed: post.changed,
        post_fallback: post.fallback,
        post_error: post.error,
        post_latency_ms: post.latency_ms,
    })
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
