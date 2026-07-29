//! Routing and compatibility parsing for `whisper-dictate run`.
//!
//! Shipping builds route to the in-process Rust runtime by default. The legacy
//! Python worker remains available through `VOICEPI_DICTATE_ENGINE=python`; reduced
//! source builds also select it automatically when native production features are
//! absent.

use std::env;

use anyhow::{anyhow, Result};

use super::dictate_run::DictateRunArgs;
use super::in_process::EngineChoice;
use super::{cloud_api_keys, default_worker_command_with_args, run_foreground};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TerminalRunPlan {
    Help,
    Rust(DictateRunArgs),
    Python(Vec<String>),
    UnknownEnginePython { raw: String, args: Vec<String> },
}

/// Public terminal entry point. Resolve feature availability before building
/// either runtime so reduced Linux source builds keep their documented Python
/// compatibility path, while shipping builds stay Rust-native by default.
pub fn run_terminal(args: Vec<String>) -> Result<()> {
    let raw_engine = env::var(super::in_process::ENGINE_ENV).ok();
    let (effective_engine, reduced_build_fallback) = effective_engine_for_build(
        raw_engine.as_deref(),
        super::dictate_run::production_features_available(),
    );
    if reduced_build_fallback {
        eprintln!(
            "[runtime] native dictation features are not compiled into this reduced build; \
             using the Python compatibility worker"
        );
    }
    dispatch_terminal_run(
        args,
        effective_engine,
        super::dictate_run::handle_dictate_run,
        |args| {
            let mut command = default_worker_command_with_args(args);
            cloud_api_keys::attach_cloud_api_keys(&mut command);
            run_foreground(&command)
        },
    )
}

fn effective_engine_for_build(
    raw_engine: Option<&str>,
    native_features_available: bool,
) -> (Option<&str>, bool) {
    let default_requested = raw_engine.is_none_or(|value| value.trim().is_empty());
    if default_requested && !native_features_available {
        (Some("python"), true)
    } else {
        (raw_engine, false)
    }
}

/// Resolve the engine before either runtime is constructed, then invoke only
/// the selected runner. Keeping construction inside the callbacks is
/// intentional: the default Rust route must not resolve a Python executable,
/// venv, app root, or `WorkerCommand`.
pub(super) fn dispatch_terminal_run<R, P>(
    args: Vec<String>,
    raw_engine: Option<&str>,
    run_rust: R,
    run_python: P,
) -> Result<()>
where
    R: FnOnce(DictateRunArgs) -> Result<()>,
    P: FnOnce(Vec<String>) -> Result<()>,
{
    match plan_terminal_run(args, raw_engine)? {
        TerminalRunPlan::Help => {
            print_native_run_help();
            Ok(())
        }
        TerminalRunPlan::Rust(args) => run_rust(args),
        TerminalRunPlan::Python(args) => run_python(args),
        TerminalRunPlan::UnknownEnginePython { raw, args } => {
            eprintln!(
                "[runtime] warning: unknown VOICEPI_DICTATE_ENGINE={raw:?}; \
                 using the Python safety-valve"
            );
            run_python(args)
        }
    }
}

pub(super) fn plan_terminal_run(
    args: Vec<String>,
    raw_engine: Option<&str>,
) -> Result<TerminalRunPlan> {
    match EngineChoice::from_env_value(raw_engine) {
        EngineChoice::Rust
            if args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--help" | "-h")) =>
        {
            Ok(TerminalRunPlan::Help)
        }
        EngineChoice::Rust => Ok(TerminalRunPlan::Rust(parse_native_run_args(args)?)),
        EngineChoice::Python => Ok(TerminalRunPlan::Python(args)),
        EngineChoice::Unknown(raw) => Ok(TerminalRunPlan::UnknownEnginePython { raw, args }),
    }
}

fn print_native_run_help() {
    println!(
        "Run dictation in the terminal with the native Rust runtime.\n\
         \n\
         Usage: whisper-dictate run [OPTIONS]\n\
         \n\
         Options:\n\
           --key <CHORD>       Push-to-talk key or chord\n\
           --model <MODEL>     Local Whisper model\n\
           --lang <LANG>       Spoken-language hint\n\
           --autodetect        Auto-detect spoken language\n\
           --prompt <TEXT>     Whisper initial-prompt override\n\
           --type|--paste|--no-type\n\
                               Text injection mode\n\
           --json              Emit structured utterance events\n\
           --device <DEVICE>   auto, cuda, or cpu\n\
           --config <PATH>     Config-file override\n\
           -h, --help          Print help"
    );
}

fn parse_native_run_args(args: Vec<String>) -> Result<DictateRunArgs> {
    let mut parsed = DictateRunArgs::default();
    let mut index = 0;
    let mut inject_mode: Option<&'static str> = None;
    let mut autodetect = false;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--autodetect" => autodetect = true,
            "--type" => set_inject_mode(&mut parsed, &mut inject_mode, "type")?,
            "--paste" => set_inject_mode(&mut parsed, &mut inject_mode, "paste")?,
            "--no-type" => set_inject_mode(&mut parsed, &mut inject_mode, "print")?,
            "--json" => {
                parsed.json_events = true;
                set_override(&mut parsed, "VOICEPI_JSON", "True");
            }
            "--foreground" => parsed.foreground = true,
            "--" => {
                let unsupported = args.get(index + 1).map(String::as_str).unwrap_or("--");
                return Err(unsupported_legacy_arg(unsupported));
            }
            _ => {
                if let Some((flag, value)) = split_value_arg(arg) {
                    apply_value_arg(&mut parsed, flag, value)?;
                } else if is_value_flag(arg) {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or_else(|| anyhow!("missing value for `{arg}`"))?;
                    apply_value_arg(&mut parsed, arg, value)?;
                } else {
                    return Err(unsupported_legacy_arg(arg));
                }
            }
        }
        index += 1;
    }

    // Python's legacy parser lets --autodetect win over --lang regardless of
    // argument order; preserve that contract in the native compatibility
    // surface.
    if autodetect {
        set_override(&mut parsed, "VOICEPI_LANG", "");
    }
    Ok(parsed)
}

fn is_value_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--key" | "--model" | "--lang" | "--prompt" | "--device" | "--config"
    )
}

fn split_value_arg(arg: &str) -> Option<(&str, &str)> {
    let (flag, value) = arg.split_once('=')?;
    is_value_flag(flag).then_some((flag, value))
}

fn apply_value_arg(parsed: &mut DictateRunArgs, flag: &str, value: &str) -> Result<()> {
    match flag {
        "--key" => set_override(parsed, "VOICEPI_KEY", value),
        "--model" => set_override(parsed, "VOICEPI_MODEL", value),
        "--lang" => set_override(parsed, "VOICEPI_LANG", value),
        "--prompt" => set_override(parsed, "VOICEPI_INITIAL_PROMPT", value),
        "--device" => {
            if !matches!(value, "auto" | "cuda" | "cpu") {
                return Err(anyhow!(
                    "invalid value `{value}` for `--device`; expected auto, cuda, or cpu"
                ));
            }
            // Backend-aware CUDA validation runs after config + CLI overlays
            // are materialized. Cloud STT legitimately ignores this local-only
            // device hint even in a CPU-only build.
            set_override(parsed, "VOICEPI_DEVICE", value);
        }
        "--config" => parsed.config = Some(value.to_owned()),
        _ => return Err(unsupported_legacy_arg(flag)),
    }
    Ok(())
}

fn set_override(parsed: &mut DictateRunArgs, name: &str, value: &str) {
    if let Some((_, current)) = parsed.env_overrides.iter_mut().find(|(key, _)| key == name) {
        *current = value.to_owned();
    } else {
        parsed
            .env_overrides
            .push((name.to_owned(), value.to_owned()));
    }
}

fn set_inject_mode(
    parsed: &mut DictateRunArgs,
    current: &mut Option<&'static str>,
    requested: &'static str,
) -> Result<()> {
    if let Some(previous) = current {
        return Err(anyhow!(
            "injection mode flags are mutually exclusive (already selected `{previous}`)"
        ));
    }
    *current = Some(requested);
    set_override(parsed, "VOICEPI_INJECT_MODE", requested);
    Ok(())
}

fn unsupported_legacy_arg(arg: &str) -> anyhow::Error {
    anyhow!(
        "`whisper-dictate run` is using the Rust runtime and does not support \
         legacy Python-only argument `{arg}`; use the native top-level \
         subcommand for that operation, or temporarily set \
         VOICEPI_DICTATE_ENGINE=python"
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn default_dispatch_runs_rust_without_touching_python_runner() {
        let rust_called = Cell::new(false);
        let python_called = Cell::new(false);

        dispatch_terminal_run(
            Vec::new(),
            None,
            |args| {
                rust_called.set(true);
                assert_eq!(args, DictateRunArgs::default());
                Ok(())
            },
            |_| {
                python_called.set(true);
                panic!("default route must not construct or run a Python worker");
            },
        )
        .unwrap();

        assert!(rust_called.get());
        assert!(!python_called.get());
    }

    #[test]
    fn explicit_python_preserves_all_passthrough_args() {
        let expected = vec!["--calibrate-mic".to_owned(), "3".to_owned()];
        dispatch_terminal_run(
            expected.clone(),
            Some("python"),
            |_| panic!("explicit Python must not enter the Rust runtime"),
            |args| {
                assert_eq!(args, expected);
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn native_parser_maps_documented_dictation_flags() {
        let plan = plan_terminal_run(
            vec![
                "--key".into(),
                "f9".into(),
                "--lang=da".into(),
                "--model".into(),
                "large-v3-turbo".into(),
                "--device".into(),
                "cpu".into(),
                "--prompt".into(),
                "Kubernetes".into(),
                "--type".into(),
                "--json".into(),
                "--config".into(),
                "custom.json".into(),
            ],
            Some("rust"),
        )
        .unwrap();

        let TerminalRunPlan::Rust(args) = plan else {
            panic!("explicit Rust must produce a Rust plan");
        };
        assert_eq!(args.config.as_deref(), Some("custom.json"));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_KEY".into(), "f9".into())));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_LANG".into(), "da".into())));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_MODEL".into(), "large-v3-turbo".into())));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_DEVICE".into(), "cpu".into())));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_INITIAL_PROMPT".into(), "Kubernetes".into())));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_INJECT_MODE".into(), "type".into())));
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_JSON".into(), "True".into())));
        assert!(args.json_events);
    }

    #[test]
    fn reduced_build_defaults_to_python_but_explicit_rust_stays_explicit() {
        assert_eq!(
            effective_engine_for_build(None, false),
            (Some("python"), true)
        );
        assert_eq!(
            effective_engine_for_build(Some(""), false),
            (Some("python"), true)
        );
        assert_eq!(
            effective_engine_for_build(Some("rust"), false),
            (Some("rust"), false)
        );
        assert_eq!(effective_engine_for_build(None, true), (None, false));
    }

    #[test]
    fn native_parser_wires_paste_to_the_clipboard_backend() {
        let TerminalRunPlan::Rust(args) =
            plan_terminal_run(vec!["--paste".into()], Some("rust")).unwrap()
        else {
            panic!("explicit Rust must produce a Rust plan");
        };
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_INJECT_MODE".into(), "paste".into())));
    }
    #[test]
    fn native_parser_defers_cuda_validation_until_backend_is_known() {
        let TerminalRunPlan::Rust(args) =
            plan_terminal_run(vec!["--device".into(), "cuda".into()], Some("rust")).unwrap()
        else {
            panic!("explicit Rust must produce a Rust plan");
        };
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_DEVICE".into(), "cuda".into())));
    }

    #[test]
    fn unsupported_python_only_flag_fails_instead_of_falling_back() {
        let err = plan_terminal_run(vec!["--calibrate-mic".into()], None).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("--calibrate-mic"));
        assert!(message.contains("VOICEPI_DICTATE_ENGINE=python"));
    }

    #[test]
    fn autodetect_overrides_saved_language_with_empty_value() {
        let TerminalRunPlan::Rust(args) = plan_terminal_run(
            vec!["--lang".into(), "da".into(), "--autodetect".into()],
            None,
        )
        .unwrap() else {
            panic!("default route must be Rust");
        };
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_LANG".into(), String::new())));
    }
}
