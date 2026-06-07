use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cli::ConfigCommand;

const CONFIG_ENV: &str = "VOICEPI_CONFIG";
const DEFAULT_PARAKEET_MODEL: &str = "nvidia/parakeet-tdt-0.6b-v3";

#[derive(Debug, Clone, Copy)]
struct RuntimeSetting {
    env: &'static str,
    key: &'static str,
    default: Option<&'static str>,
}

const RUNTIME_SETTINGS: &[RuntimeSetting] = &[
    RuntimeSetting {
        env: "VOICEPI_KEY",
        key: "key",
        default: Some("ctrl_r"),
    },
    RuntimeSetting {
        env: "VOICEPI_MODEL",
        key: "model",
        default: Some("large-v3-turbo"),
    },
    RuntimeSetting {
        env: "VOICEPI_STT_BACKEND",
        key: "stt_backend",
        default: Some("whisper"),
    },
    RuntimeSetting {
        env: "VOICEPI_STT_MODEL",
        key: "stt_model",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_STT_BASE_URL",
        key: "stt_base_url",
        default: Some("https://api.openai.com/v1"),
    },
    RuntimeSetting {
        env: "VOICEPI_STT_TIMEOUT_MS",
        key: "stt_timeout_ms",
        default: Some("30000"),
    },
    RuntimeSetting {
        env: "VOICEPI_PARAKEET_MODEL",
        key: "parakeet_model",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_DEVICE",
        key: "device",
        default: Some("auto"),
    },
    RuntimeSetting {
        env: "VOICEPI_COMPUTE_TYPE",
        key: "compute_type",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_LANG",
        key: "lang",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_XKB_LAYOUT",
        key: "xkb_layout",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_INITIAL_PROMPT",
        key: "initial_prompt",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_INJECT_MODE",
        key: "inject_mode",
        default: Some("auto"),
    },
    RuntimeSetting {
        env: "VOICEPI_FORMAT_COMMANDS",
        key: "format_commands",
        default: Some("off"),
    },
    RuntimeSetting {
        env: "VOICEPI_BEAM_SIZE",
        key: "beam_size",
        default: Some("1"),
    },
    RuntimeSetting {
        env: "VOICEPI_TEMPERATURE",
        key: "temperature",
        default: Some("0.0,0.2"),
    },
    RuntimeSetting {
        env: "VOICEPI_CONTEXT_MIN_SECONDS",
        key: "context_min_seconds",
        default: Some("5"),
    },
    RuntimeSetting {
        env: "VOICEPI_PARAKEET_MIN_SECONDS",
        key: "parakeet_min_seconds",
        default: Some("1.5"),
    },
    RuntimeSetting {
        env: "VOICEPI_RELEASE_TAIL_MS",
        key: "release_tail_ms",
        default: Some("200"),
    },
    RuntimeSetting {
        env: "VOICEPI_VAD_THRESHOLD",
        key: "vad_threshold",
        default: Some("0.3"),
    },
    RuntimeSetting {
        env: "VOICEPI_VAD_MIN_SILENCE_MS",
        key: "vad_min_silence_ms",
        default: Some("600"),
    },
    RuntimeSetting {
        env: "VOICEPI_VAD_SPEECH_PAD_MS",
        key: "vad_speech_pad_ms",
        default: Some("200"),
    },
    RuntimeSetting {
        env: "VOICEPI_TARGET_DBFS",
        key: "target_dbfs",
        default: Some("-20"),
    },
    RuntimeSetting {
        env: "VOICEPI_MIN_INPUT_DBFS",
        key: "min_input_dbfs",
        default: Some("-55"),
    },
    RuntimeSetting {
        env: "VOICEPI_MIN_SNR_DB",
        key: "min_snr_db",
        default: Some("6"),
    },
    RuntimeSetting {
        env: "VOICEPI_AUDIO_DUCKING",
        key: "audio_ducking",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_AUDIO_DUCKING_LEVEL",
        key: "audio_ducking_level",
        default: Some("0.25"),
    },
    RuntimeSetting {
        env: "VOICEPI_DICTIONARY",
        key: "dictionary",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_DICTIONARY_ENABLED",
        key: "dictionary_enabled",
        default: Some("1"),
    },
    RuntimeSetting {
        env: "VOICEPI_DICTIONARY_MAX_TERMS",
        key: "dictionary_max_terms",
        default: Some("80"),
    },
    RuntimeSetting {
        env: "VOICEPI_DICTIONARY_PROMPT_CHARS",
        key: "dictionary_prompt_chars",
        default: Some("1200"),
    },
    RuntimeSetting {
        env: "VOICEPI_JSON",
        key: "json_output",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_METRICS_JSONL",
        key: "metrics_jsonl",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_COMMAND_HOOK",
        key: "command_hook",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_COMMAND_HOOK_TIMEOUT_MS",
        key: "command_hook_timeout_ms",
        default: Some("2000"),
    },
    RuntimeSetting {
        env: "VOICEPI_HISTORY_ENABLED",
        key: "history_enabled",
        default: Some("1"),
    },
    RuntimeSetting {
        env: "VOICEPI_HISTORY_JSONL",
        key: "history_jsonl",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_LOCAL_ONLY",
        key: "local_only",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_POST_PROCESSOR",
        key: "post_processor",
        default: Some("none"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_MODE",
        key: "post_mode",
        default: Some("raw"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_MODEL",
        key: "post_model",
        default: Some("qwen2.5:3b"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_BASE_URL",
        key: "post_base_url",
        default: Some("http://localhost:11434"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_TIMEOUT_MS",
        key: "post_timeout_ms",
        default: Some("2000"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_MAX_INPUT_CHARS",
        key: "post_max_input_chars",
        default: Some("4000"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_MAX_OUTPUT_CHARS",
        key: "post_max_output_chars",
        default: Some("4000"),
    },
    RuntimeSetting {
        env: "VOICEPI_POST_REDACT",
        key: "post_redact",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_POST_REDACT_TERMS",
        key: "post_redact_terms",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_DEBUG",
        key: "debug",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_STT_DEBUG",
        key: "stt_debug",
        default: None,
    },
    RuntimeSetting {
        env: "VOICEPI_QUIT_KEY",
        key: "quit_key",
        default: Some("esc"),
    },
    RuntimeSetting {
        env: "VOICEPI_QUIT_COUNT",
        key: "quit_count",
        default: Some("3"),
    },
    RuntimeSetting {
        env: "VOICEPI_QUIT_WINDOW_MS",
        key: "quit_window_ms",
        default: Some("1500"),
    },
];

const SETTINGS_KEYS: &[&str] = &[
    "key",
    "model",
    "stt_backend",
    "stt_provider",
    "stt_model",
    "stt_base_url",
    "stt_timeout_ms",
    "parakeet_model",
    "device",
    "compute_type",
    "lang",
    "xkb_layout",
    "initial_prompt",
    "inject_mode",
    "format_commands",
    "beam_size",
    "temperature",
    "context_min_seconds",
    "parakeet_min_seconds",
    "release_tail_ms",
    "vad_threshold",
    "vad_min_silence_ms",
    "vad_speech_pad_ms",
    "target_dbfs",
    "min_input_dbfs",
    "min_snr_db",
    "audio_ducking",
    "audio_ducking_level",
    "dictionary",
    "dictionary_enabled",
    "dictionary_max_terms",
    "dictionary_prompt_chars",
    "json_output",
    "metrics_jsonl",
    "command_hook",
    "command_hook_timeout_ms",
    "history_enabled",
    "history_jsonl",
    "local_only",
    "post_processor",
    "post_mode",
    "post_model",
    "post_base_url",
    "post_timeout_ms",
    "post_max_input_chars",
    "post_max_output_chars",
    "post_redact",
    "post_redact_terms",
    "debug",
    "stt_debug",
    "quit_key",
    "quit_count",
    "quit_window_ms",
    "ui_language",
    "ui_log_view",
    "ui_theme",
    "ui_text_scale",
];

const RESTART_KEYS: &[&str] = &[
    "key",
    "model",
    "stt_backend",
    "stt_provider",
    "stt_model",
    "stt_base_url",
    "stt_timeout_ms",
    "parakeet_model",
    "device",
    "compute_type",
    "local_only",
    "quit_key",
    "quit_count",
    "quit_window_ms",
];

pub fn handle_command(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            println!("{}", config_path().display());
            Ok(())
        }
        ConfigCommand::Show => {
            let value = load_raw_config()?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
    }
}

pub fn config_path() -> PathBuf {
    if let Some(raw) = env::var_os(CONFIG_ENV) {
        return PathBuf::from(raw);
    }

    platform_config_dir().join("config.json")
}

pub fn load_raw_config() -> Result<Value> {
    let path = config_path();
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }

    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    Ok(value)
}

pub fn load_settings() -> Result<AppSettings> {
    AppSettings::from_value(load_raw_config()?)
}

pub fn save_settings(settings: &AppSettings) -> Result<PathBuf> {
    save_settings_to_path(settings, config_path())
}

pub fn effective_runtime_env() -> BTreeMap<String, String> {
    let raw_config = load_raw_config().unwrap_or_else(|_| Value::Object(Map::new()));
    let object = raw_config.as_object();
    RUNTIME_SETTINGS
        .iter()
        .filter_map(|setting| {
            runtime_setting_value(*setting, object).map(|value| (setting.env.to_owned(), value))
        })
        .collect()
}

pub fn worker_env_overrides() -> Vec<(String, String)> {
    effective_runtime_env().into_iter().collect()
}

pub fn save_settings_to_path(settings: &AppSettings, path: impl AsRef<Path>) -> Result<PathBuf> {
    settings.validate()?;
    let path = path.as_ref();
    let raw = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let value = if raw.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&raw)?
    };
    let mut object = match value {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    settings.apply_to_object(&mut object);
    path.parent().map(fs::create_dir_all).transpose()?;
    fs::write(
        path,
        serde_json::to_string_pretty(&Value::Object(object))? + "\n",
    )?;
    Ok(path.to_path_buf())
}

pub fn ensure_dictionary_file(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        path.parent().map(fs::create_dir_all).transpose()?;
        fs::write(path, "{\n  \"terms\": [],\n  \"replacements\": {}\n}\n")?;
    }
    Ok(path.to_path_buf())
}

pub fn open_dictionary(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = ensure_dictionary_file(path)?;
    open_path(&path)?;
    Ok(path)
}

pub fn default_history_path() -> PathBuf {
    if cfg!(windows) {
        platform_config_dir().join("history.jsonl")
    } else {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .unwrap_or_else(|| PathBuf::from("."))
            .join("whisper-dictate")
            .join("history.jsonl")
    }
}

pub fn open_existing_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(anyhow!("file does not exist: {}", path.display()));
    }
    open_path(path)?;
    Ok(path.to_path_buf())
}

pub fn restart_required_keys(before: &AppSettings, after: &AppSettings) -> Vec<&'static str> {
    RESTART_KEYS
        .iter()
        .copied()
        .filter(|key| before.setting_value(key) != after.setting_value(key))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub key: String,
    pub model: String,
    pub stt_backend: String,
    pub stt_provider: String,
    pub stt_model: String,
    pub stt_base_url: String,
    pub stt_timeout_ms: String,
    pub parakeet_model: String,
    pub device: String,
    pub compute_type: String,
    pub lang: String,
    pub xkb_layout: String,
    pub initial_prompt: String,
    pub inject_mode: String,
    pub format_commands: String,
    pub beam_size: String,
    pub temperature: String,
    pub context_min_seconds: String,
    pub parakeet_min_seconds: String,
    pub release_tail_ms: String,
    pub vad_threshold: String,
    pub vad_min_silence_ms: String,
    pub vad_speech_pad_ms: String,
    pub target_dbfs: String,
    pub min_input_dbfs: String,
    pub min_snr_db: String,
    pub audio_ducking: bool,
    pub audio_ducking_level: String,
    pub dictionary: String,
    pub dictionary_enabled: bool,
    pub dictionary_max_terms: String,
    pub dictionary_prompt_chars: String,
    pub inject_json: bool,
    pub metrics_jsonl: String,
    pub command_hook: String,
    pub command_hook_timeout_ms: String,
    pub history_enabled: bool,
    pub history_jsonl: String,
    pub local_only: bool,
    pub post_processor: String,
    pub post_mode: String,
    pub post_model: String,
    pub post_base_url: String,
    pub post_timeout_ms: String,
    pub post_max_input_chars: String,
    pub post_max_output_chars: String,
    pub post_redact: bool,
    pub post_redact_terms: String,
    pub debug: bool,
    pub stt_debug: bool,
    pub quit_key: String,
    pub quit_count: String,
    pub quit_window_ms: String,
    pub ui_language: String,
    pub ui_log_view: String,
    pub ui_theme: String,
    pub ui_text_scale: String,
    pub profiles_json: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            key: "ctrl_r".to_owned(),
            model: "large-v3-turbo".to_owned(),
            stt_backend: "whisper".to_owned(),
            stt_provider: "openai".to_owned(),
            stt_model: String::new(),
            stt_base_url: "https://api.openai.com/v1".to_owned(),
            stt_timeout_ms: "30000".to_owned(),
            parakeet_model: DEFAULT_PARAKEET_MODEL.to_owned(),
            device: "auto".to_owned(),
            compute_type: String::new(),
            lang: String::new(),
            xkb_layout: String::new(),
            initial_prompt: String::new(),
            inject_mode: "auto".to_owned(),
            format_commands: "off".to_owned(),
            beam_size: "1".to_owned(),
            temperature: "0.0,0.2".to_owned(),
            context_min_seconds: "5".to_owned(),
            parakeet_min_seconds: "1.5".to_owned(),
            release_tail_ms: "200".to_owned(),
            vad_threshold: "0.3".to_owned(),
            vad_min_silence_ms: "600".to_owned(),
            vad_speech_pad_ms: "200".to_owned(),
            target_dbfs: "-20".to_owned(),
            min_input_dbfs: "-55".to_owned(),
            min_snr_db: "6".to_owned(),
            audio_ducking: false,
            audio_ducking_level: "0.25".to_owned(),
            dictionary: default_dictionary_path().display().to_string(),
            dictionary_enabled: true,
            dictionary_max_terms: "80".to_owned(),
            dictionary_prompt_chars: "1200".to_owned(),
            inject_json: false,
            metrics_jsonl: String::new(),
            command_hook: String::new(),
            command_hook_timeout_ms: "2000".to_owned(),
            history_enabled: true,
            history_jsonl: String::new(),
            local_only: false,
            post_processor: "none".to_owned(),
            post_mode: "raw".to_owned(),
            post_model: "qwen2.5:3b".to_owned(),
            post_base_url: "http://localhost:11434".to_owned(),
            post_timeout_ms: "2000".to_owned(),
            post_max_input_chars: "4000".to_owned(),
            post_max_output_chars: "4000".to_owned(),
            post_redact: false,
            post_redact_terms: String::new(),
            debug: false,
            stt_debug: false,
            quit_key: "esc".to_owned(),
            quit_count: "3".to_owned(),
            quit_window_ms: "1500".to_owned(),
            ui_language: "en".to_owned(),
            ui_log_view: "minimal".to_owned(),
            ui_theme: "dark".to_owned(),
            ui_text_scale: "1.15".to_owned(),
            profiles_json: "[]".to_owned(),
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> Result<()> {
        validate_choice(
            "stt_backend",
            &self.stt_backend,
            &["whisper", "parakeet", "openai"],
        )?;
        validate_choice("stt_provider", &self.stt_provider, &["groq", "openai"])?;
        validate_choice("device", &self.device, &["auto", "cuda", "cpu"])?;
        validate_choice(
            "inject_mode",
            &self.inject_mode,
            &["auto", "type", "paste", "print"],
        )?;
        validate_choice(
            "post_processor",
            &self.post_processor,
            &["none", "ollama", "openai", "groq"],
        )?;
        validate_choice(
            "post_mode",
            &self.post_mode,
            &[
                "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
            ],
        )?;
        validate_choice("ui_theme", &self.ui_theme, &["dark", "light"])?;
        validate_choice("ui_language", &self.ui_language, &["en", "da"])?;
        validate_choice(
            "ui_log_view",
            &self.ui_log_view,
            &["minimal", "diagnostic", "debug"],
        )?;

        if self.stt_backend == "openai" {
            validate_http_url("stt_base_url", &self.stt_base_url)?;
            if self.stt_model.trim().is_empty() {
                return Err(anyhow!("stt_model is required when stt_backend is openai"));
            }
        }
        if matches!(self.post_processor.as_str(), "ollama" | "openai" | "groq") {
            validate_http_url("post_base_url", &self.post_base_url)?;
            if self.post_model.trim().is_empty() {
                return Err(anyhow!(
                    "post_model is required when post_processor is active"
                ));
            }
        }

        validate_u32("stt_timeout_ms", &self.stt_timeout_ms, 100)?;
        validate_u32("beam_size", &self.beam_size, 1)?;
        validate_u32("vad_min_silence_ms", &self.vad_min_silence_ms, 0)?;
        validate_u32("vad_speech_pad_ms", &self.vad_speech_pad_ms, 0)?;
        validate_u32("dictionary_max_terms", &self.dictionary_max_terms, 1)?;
        validate_u32("dictionary_prompt_chars", &self.dictionary_prompt_chars, 1)?;
        validate_u32("post_timeout_ms", &self.post_timeout_ms, 100)?;
        validate_u32("post_max_input_chars", &self.post_max_input_chars, 100)?;
        validate_u32("post_max_output_chars", &self.post_max_output_chars, 100)?;
        validate_u32("quit_count", &self.quit_count, 0)?;
        validate_u32("quit_window_ms", &self.quit_window_ms, 1)?;
        validate_f32("vad_threshold", &self.vad_threshold)?;
        validate_f32("target_dbfs", &self.target_dbfs)?;
        validate_f32("min_input_dbfs", &self.min_input_dbfs)?;
        validate_f32("min_snr_db", &self.min_snr_db)?;
        validate_f32("release_tail_ms", &self.release_tail_ms)?;
        validate_f32("context_min_seconds", &self.context_min_seconds)?;
        validate_f32("parakeet_min_seconds", &self.parakeet_min_seconds)?;
        validate_f32("audio_ducking_level", &self.audio_ducking_level)?;
        validate_f32("ui_text_scale", &self.ui_text_scale)?;
        Ok(())
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let object = value.as_object();
        let defaults = Self::default();
        let mut settings = defaults.clone();
        if let Some(object) = object {
            settings.key = string_value(object, "key", &defaults.key);
            settings.model = string_value(object, "model", &defaults.model);
            settings.stt_backend = string_value(object, "stt_backend", &defaults.stt_backend);
            settings.stt_provider = string_value(object, "stt_provider", "");
            settings.stt_model = string_value(object, "stt_model", "");
            settings.stt_base_url = string_value(object, "stt_base_url", &defaults.stt_base_url);
            if settings.stt_provider.trim().is_empty() {
                settings.stt_provider = if settings
                    .stt_base_url
                    .to_ascii_lowercase()
                    .contains("api.groq.com")
                {
                    "groq".to_owned()
                } else {
                    defaults.stt_provider.clone()
                };
            }
            settings.stt_timeout_ms =
                string_value(object, "stt_timeout_ms", &defaults.stt_timeout_ms);
            settings.parakeet_model =
                string_value(object, "parakeet_model", &defaults.parakeet_model);
            settings.device = string_value(object, "device", &defaults.device);
            settings.compute_type = string_value(object, "compute_type", "");
            settings.lang = string_value(object, "lang", "");
            settings.xkb_layout = string_value(object, "xkb_layout", "");
            settings.initial_prompt = string_value(object, "initial_prompt", "");
            settings.inject_mode = string_value(object, "inject_mode", &defaults.inject_mode);
            settings.format_commands =
                string_value(object, "format_commands", &defaults.format_commands);
            settings.beam_size = string_value(object, "beam_size", &defaults.beam_size);
            settings.temperature = string_value(object, "temperature", &defaults.temperature);
            settings.context_min_seconds =
                string_value(object, "context_min_seconds", &defaults.context_min_seconds);
            settings.parakeet_min_seconds = string_value(
                object,
                "parakeet_min_seconds",
                &defaults.parakeet_min_seconds,
            );
            settings.release_tail_ms =
                string_value(object, "release_tail_ms", &defaults.release_tail_ms);
            settings.vad_threshold = string_value(object, "vad_threshold", &defaults.vad_threshold);
            settings.vad_min_silence_ms =
                string_value(object, "vad_min_silence_ms", &defaults.vad_min_silence_ms);
            settings.vad_speech_pad_ms =
                string_value(object, "vad_speech_pad_ms", &defaults.vad_speech_pad_ms);
            settings.target_dbfs = string_value(object, "target_dbfs", &defaults.target_dbfs);
            settings.min_input_dbfs =
                string_value(object, "min_input_dbfs", &defaults.min_input_dbfs);
            settings.min_snr_db = string_value(object, "min_snr_db", &defaults.min_snr_db);
            settings.audio_ducking = bool_value(object, "audio_ducking", defaults.audio_ducking);
            settings.audio_ducking_level =
                string_value(object, "audio_ducking_level", &defaults.audio_ducking_level);
            settings.dictionary = string_value(object, "dictionary", &defaults.dictionary);
            settings.dictionary_enabled =
                bool_value(object, "dictionary_enabled", defaults.dictionary_enabled);
            settings.dictionary_max_terms = string_value(
                object,
                "dictionary_max_terms",
                &defaults.dictionary_max_terms,
            );
            settings.dictionary_prompt_chars = string_value(
                object,
                "dictionary_prompt_chars",
                &defaults.dictionary_prompt_chars,
            );
            settings.inject_json = bool_value(object, "json_output", defaults.inject_json);
            settings.metrics_jsonl = string_value(object, "metrics_jsonl", "");
            settings.command_hook = string_value(object, "command_hook", "");
            settings.command_hook_timeout_ms = string_value(
                object,
                "command_hook_timeout_ms",
                &defaults.command_hook_timeout_ms,
            );
            settings.history_enabled =
                bool_value(object, "history_enabled", defaults.history_enabled);
            settings.history_jsonl = string_value(object, "history_jsonl", "");
            settings.local_only = bool_value(object, "local_only", defaults.local_only);
            settings.post_processor =
                string_value(object, "post_processor", &defaults.post_processor);
            settings.post_mode = string_value(object, "post_mode", &defaults.post_mode);
            settings.post_model = string_value(object, "post_model", &defaults.post_model);
            settings.post_base_url = string_value(object, "post_base_url", &defaults.post_base_url);
            settings.post_timeout_ms =
                string_value(object, "post_timeout_ms", &defaults.post_timeout_ms);
            settings.post_max_input_chars = string_value(
                object,
                "post_max_input_chars",
                &defaults.post_max_input_chars,
            );
            settings.post_max_output_chars = string_value(
                object,
                "post_max_output_chars",
                &defaults.post_max_output_chars,
            );
            settings.post_redact = bool_value(object, "post_redact", defaults.post_redact);
            settings.post_redact_terms = string_value(object, "post_redact_terms", "");
            settings.debug = bool_value(object, "debug", defaults.debug);
            settings.stt_debug = bool_value(object, "stt_debug", defaults.stt_debug);
            settings.quit_key = string_value(object, "quit_key", &defaults.quit_key);
            settings.quit_count = string_value(object, "quit_count", &defaults.quit_count);
            settings.quit_window_ms =
                string_value(object, "quit_window_ms", &defaults.quit_window_ms);
            settings.ui_theme = string_value(object, "ui_theme", &defaults.ui_theme);
            settings.ui_language = string_value(object, "ui_language", &defaults.ui_language);
            settings.ui_log_view = string_value(object, "ui_log_view", &defaults.ui_log_view);
            settings.ui_text_scale = string_value(object, "ui_text_scale", &defaults.ui_text_scale);
            settings.profiles_json = object
                .get("profiles")
                .map(serde_json::to_string_pretty)
                .transpose()?
                .unwrap_or(defaults.profiles_json);
        }
        Ok(settings)
    }

    fn apply_to_object(&self, object: &mut Map<String, Value>) {
        for key in SETTINGS_KEYS {
            object.remove(*key);
        }
        set_string(object, "key", &self.key);
        set_string(object, "model", &self.model);
        set_string(object, "stt_backend", &self.stt_backend);
        set_string(object, "stt_provider", &self.stt_provider);
        set_string(object, "stt_model", &self.stt_model);
        set_string(object, "stt_base_url", &self.stt_base_url);
        set_string(object, "stt_timeout_ms", &self.stt_timeout_ms);
        if self.parakeet_model != DEFAULT_PARAKEET_MODEL {
            set_string(object, "parakeet_model", &self.parakeet_model);
        }
        set_string(object, "device", &self.device);
        set_string(object, "compute_type", &self.compute_type);
        set_string(object, "lang", &self.lang);
        set_string(object, "xkb_layout", &self.xkb_layout);
        set_string(object, "initial_prompt", &self.initial_prompt);
        set_string(object, "inject_mode", &self.inject_mode);
        set_string(object, "format_commands", &self.format_commands);
        set_string(object, "beam_size", &self.beam_size);
        set_string(object, "temperature", &self.temperature);
        set_string(object, "context_min_seconds", &self.context_min_seconds);
        set_string(object, "parakeet_min_seconds", &self.parakeet_min_seconds);
        set_string(object, "release_tail_ms", &self.release_tail_ms);
        set_string(object, "vad_threshold", &self.vad_threshold);
        set_string(object, "vad_min_silence_ms", &self.vad_min_silence_ms);
        set_string(object, "vad_speech_pad_ms", &self.vad_speech_pad_ms);
        set_string(object, "target_dbfs", &self.target_dbfs);
        set_string(object, "min_input_dbfs", &self.min_input_dbfs);
        set_string(object, "min_snr_db", &self.min_snr_db);
        set_bool(object, "audio_ducking", self.audio_ducking);
        set_string(object, "audio_ducking_level", &self.audio_ducking_level);
        set_string(object, "dictionary", &self.dictionary);
        set_bool(object, "dictionary_enabled", self.dictionary_enabled);
        set_string(object, "dictionary_max_terms", &self.dictionary_max_terms);
        set_string(
            object,
            "dictionary_prompt_chars",
            &self.dictionary_prompt_chars,
        );
        set_bool(object, "json_output", self.inject_json);
        set_string(object, "metrics_jsonl", &self.metrics_jsonl);
        set_string(object, "command_hook", &self.command_hook);
        set_string(
            object,
            "command_hook_timeout_ms",
            &self.command_hook_timeout_ms,
        );
        set_bool(object, "history_enabled", self.history_enabled);
        set_string(object, "history_jsonl", &self.history_jsonl);
        set_bool(object, "local_only", self.local_only);
        set_string(object, "post_processor", &self.post_processor);
        set_string(object, "post_mode", &self.post_mode);
        set_string(object, "post_model", &self.post_model);
        set_string(object, "post_base_url", &self.post_base_url);
        set_string(object, "post_timeout_ms", &self.post_timeout_ms);
        set_string(object, "post_max_input_chars", &self.post_max_input_chars);
        set_string(object, "post_max_output_chars", &self.post_max_output_chars);
        set_bool(object, "post_redact", self.post_redact);
        set_string(object, "post_redact_terms", &self.post_redact_terms);
        set_bool(object, "debug", self.debug);
        set_bool(object, "stt_debug", self.stt_debug);
        set_string(object, "quit_key", &self.quit_key);
        set_string(object, "quit_count", &self.quit_count);
        set_string(object, "quit_window_ms", &self.quit_window_ms);
        set_string(object, "ui_theme", &self.ui_theme);
        set_string(object, "ui_language", &self.ui_language);
        set_string(object, "ui_log_view", &self.ui_log_view);
        set_string(object, "ui_text_scale", &self.ui_text_scale);
        if let Ok(profiles) = serde_json::from_str::<Value>(&self.profiles_json) {
            if !profiles.as_array().is_some_and(Vec::is_empty) {
                object.insert("profiles".to_owned(), profiles);
            } else {
                object.remove("profiles");
            }
        }
    }

    fn setting_value(&self, key: &str) -> Option<&str> {
        match key {
            "key" => Some(&self.key),
            "model" => Some(&self.model),
            "stt_backend" => Some(&self.stt_backend),
            "stt_provider" => Some(&self.stt_provider),
            "stt_model" => Some(&self.stt_model),
            "stt_base_url" => Some(&self.stt_base_url),
            "stt_timeout_ms" => Some(&self.stt_timeout_ms),
            "parakeet_model" => Some(&self.parakeet_model),
            "device" => Some(&self.device),
            "compute_type" => Some(&self.compute_type),
            "local_only" => Some(if self.local_only { "1" } else { "0" }),
            "quit_key" => Some(&self.quit_key),
            "quit_count" => Some(&self.quit_count),
            "quit_window_ms" => Some(&self.quit_window_ms),
            _ => None,
        }
    }
}

fn platform_config_dir() -> PathBuf {
    if cfg!(windows) {
        let base = env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE")
                    .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        return base.join("WhisperDictate");
    }

    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("whisper-dictate")
}

fn default_dictionary_path() -> PathBuf {
    platform_config_dir().join("dictionary.json")
}

fn runtime_setting_value(
    setting: RuntimeSetting,
    object: Option<&Map<String, Value>>,
) -> Option<String> {
    object
        .and_then(|object| object.get(setting.key))
        .and_then(value_to_env_string)
        .or_else(|| env::var(setting.env).ok().filter(|value| !value.is_empty()))
        .or_else(|| setting.default.map(str::to_owned))
}

fn value_to_env_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) if value.is_empty() => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(true) => Some("True".to_owned()),
        Value::Bool(false) => Some("False".to_owned()),
        value => Some(value.to_string()),
    }
}

fn string_value(object: &Map<String, Value>, key: &str, default: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_value(object: &Map<String, Value>, key: &str, default: bool) -> bool {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

fn set_string(object: &mut Map<String, Value>, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        object.remove(key);
    } else {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn set_bool(object: &mut Map<String, Value>, key: &str, value: bool) {
    object.insert(
        key.to_owned(),
        Value::String(if value { "1" } else { "0" }.to_owned()),
    );
}

fn validate_choice(name: &str, value: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(anyhow!(
            "{name} must be one of {}; got {value:?}",
            allowed.join(", ")
        ))
    }
}

fn validate_http_url(name: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(anyhow!("{name} must start with http:// or https://"))
    }
}

fn validate_u32(name: &str, value: &str, minimum: u32) -> Result<()> {
    let parsed = value
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow!("{name} must be an integer"))?;
    if parsed >= minimum {
        Ok(())
    } else {
        Err(anyhow!("{name} must be at least {minimum}"))
    }
}

fn validate_f32(name: &str, value: &str) -> Result<()> {
    value
        .trim()
        .parse::<f32>()
        .map(|_| ())
        .map_err(|_| anyhow!("{name} must be a number"))
}

fn open_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        command
            .args(["/C", "start", "", &path.display().to_string()])
            .creation_flags(0x08000000);
        command.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn config_env_overrides_default_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.json");

        env::set_var(CONFIG_ENV, &path);
        assert_eq!(config_path(), path);
        env::remove_var(CONFIG_ENV);
    }

    #[test]
    fn missing_config_loads_as_empty_object() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");

        env::set_var(CONFIG_ENV, &path);
        assert_eq!(load_raw_config().unwrap(), Value::Object(Map::new()));
        env::remove_var(CONFIG_ENV);
    }

    #[test]
    fn existing_config_loads_json() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"lang":"da"}"#).unwrap();

        env::set_var(CONFIG_ENV, &path);
        assert_eq!(load_raw_config().unwrap()["lang"], "da");
        env::remove_var(CONFIG_ENV);
    }

    #[test]
    fn settings_load_defaults_and_existing_values() {
        let value = serde_json::json!({
            "stt_backend": "openai",
            "stt_provider": "groq",
            "lang": "da",
            "xkb_layout": "dk",
            "quit_key": "f12",
            "dictionary_enabled": "0",
            "json_output": "1",
            "audio_ducking": "1",
            "post_redact": "1",
            "post_redact_terms": "Lars Andersen",
            "ui_theme": "light",
            "ui_language": "da",
            "ui_log_view": "diagnostic",
            "profiles": [{"name": "terminal"}]
        });

        let settings = AppSettings::from_value(value).unwrap();

        assert_eq!(settings.stt_backend, "openai");
        assert_eq!(settings.stt_provider, "groq");
        assert_eq!(settings.lang, "da");
        assert_eq!(settings.xkb_layout, "dk");
        assert_eq!(settings.quit_key, "f12");
        assert!(!settings.dictionary_enabled);
        assert!(settings.inject_json);
        assert!(settings.audio_ducking);
        assert!(settings.post_redact);
        assert_eq!(settings.post_redact_terms, "Lars Andersen");
        assert_eq!(settings.ui_theme, "light");
        assert_eq!(settings.ui_language, "da");
        assert_eq!(settings.ui_log_view, "diagnostic");
        assert!(settings.profiles_json.contains("terminal"));
        assert_eq!(settings.model, "large-v3-turbo");
        assert_eq!(settings.context_min_seconds, "5");
        assert_eq!(settings.ui_text_scale, "1.15");
    }

    #[test]
    fn effective_runtime_env_uses_config_then_env_then_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            serde_json::json!({
                "lang": "da",
                "model": "large-v3",
                "debug": true
            })
            .to_string(),
        )
        .unwrap();

        let old_config = env::var_os(CONFIG_ENV);
        let old_model = env::var_os("VOICEPI_MODEL");
        let old_device = env::var_os("VOICEPI_DEVICE");
        let old_key = env::var_os("VOICEPI_KEY");
        let old_lang = env::var_os("VOICEPI_LANG");
        let old_debug = env::var_os("VOICEPI_DEBUG");

        env::set_var(CONFIG_ENV, &path);
        env::set_var("VOICEPI_MODEL", "env-model");
        env::set_var("VOICEPI_DEVICE", "cuda");
        env::remove_var("VOICEPI_KEY");
        env::set_var("VOICEPI_LANG", "en");
        env::remove_var("VOICEPI_DEBUG");

        let env_values = effective_runtime_env();

        assert_eq!(env_values["VOICEPI_MODEL"], "large-v3");
        assert_eq!(env_values["VOICEPI_LANG"], "da");
        assert_eq!(env_values["VOICEPI_DEVICE"], "cuda");
        assert_eq!(env_values["VOICEPI_KEY"], "ctrl_r");
        assert_eq!(env_values["VOICEPI_CONTEXT_MIN_SECONDS"], "5");
        assert_eq!(env_values["VOICEPI_DEBUG"], "True");

        restore_env(CONFIG_ENV, old_config);
        restore_env("VOICEPI_MODEL", old_model);
        restore_env("VOICEPI_DEVICE", old_device);
        restore_env("VOICEPI_KEY", old_key);
        restore_env("VOICEPI_LANG", old_lang);
        restore_env("VOICEPI_DEBUG", old_debug);
    }

    #[test]
    fn settings_infers_groq_provider_from_existing_base_url() {
        let value = serde_json::json!({
            "stt_backend": "openai",
            "stt_base_url": "https://api.groq.com/openai/v1",
            "stt_model": "whisper-large-v3-turbo"
        });

        let settings = AppSettings::from_value(value).unwrap();

        assert_eq!(settings.stt_provider, "groq");
        assert_eq!(settings.stt_base_url, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn settings_validation_rejects_invalid_backend() {
        let settings = AppSettings {
            stt_backend: "cloud".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("stt_backend"));
    }

    #[test]
    fn settings_validation_rejects_invalid_ui_theme() {
        let settings = AppSettings {
            ui_theme: "solarized".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("ui_theme"));
    }

    #[test]
    fn settings_validation_rejects_invalid_ui_language() {
        let settings = AppSettings {
            ui_language: "dk".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("ui_language"));
    }

    #[test]
    fn settings_validation_rejects_invalid_ui_log_view() {
        let settings = AppSettings {
            ui_log_view: "full".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("ui_log_view"));
    }

    #[test]
    fn settings_validation_rejects_cloud_without_http_url() {
        let settings = AppSettings {
            stt_backend: "openai".to_owned(),
            stt_model: "whisper-large-v3-turbo".to_owned(),
            stt_base_url: "api.groq.com/openai/v1".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("stt_base_url"));
    }

    #[test]
    fn settings_validation_rejects_invalid_numeric_values() {
        let settings = AppSettings {
            beam_size: "fast".to_owned(),
            ..AppSettings::default()
        };

        assert!(settings
            .validate()
            .unwrap_err()
            .to_string()
            .contains("beam_size"));
    }

    #[test]
    fn saving_settings_preserves_unknown_keys_and_removes_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"unknown":"keep","lang":"da","stt_model":"old","profiles":[{"name":"old"}]}"#,
        )
        .unwrap();

        let settings = AppSettings {
            lang: "en".to_owned(),
            xkb_layout: "dk".to_owned(),
            stt_provider: "groq".to_owned(),
            stt_model: String::new(),
            quit_key: "f12".to_owned(),
            audio_ducking: true,
            post_redact: true,
            post_redact_terms: "Lars Andersen".to_owned(),
            ui_theme: "light".to_owned(),
            ui_language: "da".to_owned(),
            ui_log_view: "debug".to_owned(),
            ui_text_scale: "1.3".to_owned(),
            profiles_json: r#"[{"name":"new"}]"#.to_owned(),
            ..AppSettings::default()
        };

        save_settings_to_path(&settings, &path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert_eq!(saved["unknown"], "keep");
        assert_eq!(saved["lang"], "en");
        assert_eq!(saved["xkb_layout"], "dk");
        assert_eq!(saved["stt_provider"], "groq");
        assert_eq!(saved["quit_key"], "f12");
        assert_eq!(saved["audio_ducking"], "1");
        assert_eq!(saved["post_redact"], "1");
        assert_eq!(saved["post_redact_terms"], "Lars Andersen");
        assert_eq!(saved["ui_theme"], "light");
        assert_eq!(saved["ui_language"], "da");
        assert_eq!(saved["ui_log_view"], "debug");
        assert_eq!(saved["ui_text_scale"], "1.3");
        assert!(saved.get("stt_model").is_none());
        assert_eq!(saved["profiles"][0]["name"], "new");
    }

    #[test]
    fn saving_empty_profiles_removes_profiles_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"profiles":[{"name":"old"}]}"#).unwrap();

        save_settings_to_path(&AppSettings::default(), &path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert!(saved.get("profiles").is_none());
    }

    #[test]
    fn restart_required_keys_reports_restart_only_changes() {
        let before = AppSettings::default();
        let after = AppSettings {
            key: "shift_r+ctrl_r".to_owned(),
            lang: "da".to_owned(),
            inject_mode: "print".to_owned(),
            ..AppSettings::default()
        };

        assert_eq!(restart_required_keys(&before, &after), vec!["key"]);

        let after = AppSettings {
            quit_key: "f12".to_owned(),
            ..AppSettings::default()
        };

        assert_eq!(restart_required_keys(&before, &after), vec!["quit_key"]);

        let after = AppSettings {
            ui_theme: "light".to_owned(),
            ui_language: "da".to_owned(),
            ui_log_view: "diagnostic".to_owned(),
            ui_text_scale: "1.3".to_owned(),
            ..AppSettings::default()
        };

        assert!(restart_required_keys(&before, &after).is_empty());
    }

    #[test]
    fn ensure_dictionary_file_creates_empty_json_dictionary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dictionary.json");

        ensure_dictionary_file(&path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        assert_eq!(saved["terms"], serde_json::json!([]));
        assert_eq!(saved["replacements"], serde_json::json!({}));
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            env::set_var(name, value);
        } else {
            env::remove_var(name);
        }
    }
}
