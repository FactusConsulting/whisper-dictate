//! Native parsing and dispatch for `whisper-dictate run`.

use std::env;

use anyhow::{anyhow, Result};

use super::dictate_run::DictateRunArgs;
use super::supervisor::validate_engine_selection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TerminalRunPlan {
    Help,
    Rust(DictateRunArgs),
}

/// Public terminal entry point. Reduced builds now fail with an actionable
/// rebuild message; they never launch a different runtime.
pub fn run_terminal(args: Vec<String>) -> Result<()> {
    let raw_engine = env::var(super::in_process::ENGINE_ENV).ok();
    match plan_terminal_run(args, raw_engine.as_deref())? {
        TerminalRunPlan::Help => {
            print_native_run_help();
            Ok(())
        }
        TerminalRunPlan::Rust(args) => {
            if !super::dictate_run::production_features_available() {
                return Err(anyhow!(
                    "native dictation features are not compiled into this build; rebuild with \
                     `rust-hotkeys,rust-injection,audio-in-rust,whisper-rs-local`"
                ));
            }
            super::dictate_run::handle_dictate_run(args)
        }
    }
}

pub(super) fn plan_terminal_run(
    args: Vec<String>,
    raw_engine: Option<&str>,
) -> Result<TerminalRunPlan> {
    validate_engine_selection(raw_engine)?;
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        Ok(TerminalRunPlan::Help)
    } else {
        Ok(TerminalRunPlan::Rust(parse_native_run_args(args)?))
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
           --device <DEVICE>   auto, vulkan, or cpu\n\
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
            let value = crate::whisper::device_options::canonicalize_device_value(value);
            if !matches!(value.as_str(), "auto" | "vulkan" | "cpu") {
                return Err(anyhow!(
                    "invalid value `{value}` for `--device`; expected auto, vulkan, or cpu"
                ));
            }
            // Backend-aware Vulkan validation runs after config + CLI overlays
            // are materialized. Cloud STT legitimately ignores this local-only
            // device hint even in a CPU-only build.
            set_override(parsed, "VOICEPI_DEVICE", &value);
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
         retired argument `{arg}`; use the native top-level subcommand for \
         that operation"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plan_runs_rust() {
        assert_eq!(
            plan_terminal_run(Vec::new(), None).unwrap(),
            TerminalRunPlan::Rust(DictateRunArgs::default())
        );
    }

    #[test]
    fn explicit_python_is_a_migration_error() {
        let error = plan_terminal_run(Vec::new(), Some("python")).unwrap_err();
        assert!(error.to_string().contains("no longer supported"));
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
    fn native_parser_defers_vulkan_validation_until_backend_is_known() {
        let TerminalRunPlan::Rust(args) =
            plan_terminal_run(vec!["--device".into(), "vulkan".into()], Some("rust")).unwrap()
        else {
            panic!("explicit Rust must produce a Rust plan");
        };
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_DEVICE".into(), "vulkan".into())));
    }

    #[test]
    fn native_parser_migrates_legacy_cuda_alias_to_vulkan() {
        let TerminalRunPlan::Rust(args) =
            plan_terminal_run(vec!["--device=cuda".into()], Some("rust")).unwrap()
        else {
            panic!("explicit Rust must produce a Rust plan");
        };
        assert!(args
            .env_overrides
            .contains(&("VOICEPI_DEVICE".into(), "vulkan".into())));
    }

    #[test]
    fn unsupported_python_only_flag_fails_instead_of_falling_back() {
        let err = plan_terminal_run(vec!["--calibrate-mic".into()], None).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("--calibrate-mic"));
        assert!(!message.contains("fallback"));
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
