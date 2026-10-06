//! CLI entry point (`wd.exe`). Console subsystem on every
//! platform — every CLI verb prints to stdout/stderr as expected when invoked
//! from PowerShell/cmd/a script. The tray UI lives in the sibling
//! `wd-gui.exe` binary (windows-subsystem on Windows) so a
//! double-click from Explorer never flashes a cmd window. Both binaries
//! delegate to the shared library crate (`whisper_dictate_app`) — this file
//! is dispatch-only.

use std::process::ExitCode;

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command, DevicesCommand};
use whisper_dictate_app::{
    benchmark, calibration, cloud_api, command_hook, config, corpus_record, dictate, dictionary,
    doctor, entrypoint, formatting, health, history, hotkey, injection, model_capacity, platform,
    postprocess, privacy, profiles, redaction, runtime, setup, telemetry, transcribe_file, ui,
    whisper,
};

pub fn main() -> ExitCode {
    // `_with_teardown`, never the bare shell: the finite rdev-driven verbs
    // (`self-test hotkey-boot`, `hotkey capture --for-secs ...`) queue
    // diagnostics on the async writer thread, and a bare return from `main`
    // would kill that thread with the tail of the capture still unwritten.
    // See `entrypoint::error_exit_shell_with_teardown`.
    entrypoint::error_exit_shell_with_teardown("error", std::io::stderr(), run)
}

fn run() -> anyhow::Result<()> {
    if runtime::hotkey_probe_child_requested() {
        return runtime::run_hotkey_probe_child();
    }
    let cli = Cli::parse();
    if cli.version {
        println!("wd {}", runtime::version());
        return Ok(());
    }

    match cli.command.unwrap_or(Command::Ui) {
        Command::Ui | Command::Settings => ui::run(),
        Command::Run { args } => runtime::run_terminal(args),
        Command::ListWindows => platform::window_enumeration::handle_list_windows(),
        Command::TranscribeFile { path, json } => {
            transcribe_file::handle(std::path::Path::new(&path), json)
        }
        Command::CalibrateMic {
            seconds,
            device,
            json,
        } => calibration::handle_microphone(seconds, device.as_deref(), json),
        Command::CalibrateFile { path, json } => {
            calibration::handle_file(std::path::Path::new(&path), json)
        }
        Command::Doctor { json, config } => doctor::handle_doctor(json, config.as_deref()),
        Command::Bench => benchmark::handle_bench(),
        Command::CorpusRecord { id } => corpus_record::handle_corpus_record(&id),
        Command::SimulateSession { wav, json, repeat } => {
            dictate::simulate::handle_simulate_session(&wav, json, repeat)
        }
        Command::DictateMic {
            device,
            seconds,
            json,
        } => handle_dictate_mic(&device, seconds, json),
        Command::SetupUbuntu => runtime::setup_ubuntu(),
        Command::Setup => setup::handle_setup(),
        Command::ExportConfig { include_secrets } => setup::handle_export(include_secrets),
        Command::ModelCapacity { json } => model_capacity::handle_command(json),
        Command::Config { command } => config::handle_command(command),
        Command::Dictionary { command } => dictionary::handle_command(command),
        Command::DictionaryRuntime => dictionary::handle_runtime(),
        Command::DictateOps => dictate::ops::handle_ops(),
        Command::History { command } => history::handle_history_command(command),
        args @ Command::InjectText { .. } => dispatch_inject_text(args),
        Command::FormatText { text, command_set } => {
            formatting::handle_format_text(&text, &command_set)
        }
        Command::CloudTranscribe {
            base_url,
            api_key,
            model,
            audio_wav_path,
            language,
            prompt,
            timeout_ms,
        } => cloud_api::handle_cloud_transcribe(
            &base_url,
            &cloud_api::resolve_api_key(&api_key, &base_url),
            &model,
            audio_wav_path.as_ref(),
            (!language.trim().is_empty()).then_some(language.as_str()),
            (!prompt.trim().is_empty()).then_some(prompt.as_str()),
            timeout_ms,
        ),
        Command::AppendJsonl { path } => {
            telemetry::handle_append_jsonl(std::path::Path::new(&path))
        }
        Command::AppendHistory { path } => {
            telemetry::handle_append_history(std::path::Path::new(&path))
        }
        Command::AppendRecordSinks => telemetry::handle_append_record_sinks(),
        Command::WorkerEvent => telemetry::handle_worker_event(),
        Command::CommandHook => command_hook::handle_command_hook(),
        Command::RedactText => redaction::handle_redact_text(),
        Command::ApplyProfile => profiles::handle_apply_profile(),
        Command::Privacy => privacy::handle_privacy(),
        Command::Postprocess => postprocess::handle_postprocess(),
        Command::ExternalApi => cloud_api::handle_external_api(),
        Command::Health => health::handle_health(),
        Command::TranscribeWav { probe } => handle_transcribe_wav(probe),
        Command::TranscribeServer => handle_transcribe_server(),
        Command::Inject => injection::handle_inject(),
        Command::Devices { command } => match command {
            None => handle_devices_command(),
            Some(DevicesCommand::Test { name }) => handle_devices_test(&name),
        },
        Command::Models { command } => whisper::models_cli::handle(command),
        Command::Hotkey { command } => hotkey::capture::handle_hotkey_command(command),
        Command::SelfTest { command } => handle_self_test(command),
        Command::DictateRun {
            config,
            json_events,
            foreground,
        } => runtime::dictate_run::handle_dictate_run(runtime::dictate_run::DictateRunArgs {
            config,
            json_events,
            foreground,
            env_overrides: Vec::new(),
        }),
    }
}

mod main_media;
mod main_self_test;

use main_media::{handle_dictate_mic, handle_transcribe_server, handle_transcribe_wav};
use main_self_test::handle_self_test;

/// Route the `inject-text` subcommand to either the legacy hidden helper
/// (`--mode {type|paste}`) or the public dry-run/inject
/// verb (`inject-text <TEXT> [--dry-run|--do-it] [--backend NAME] [--json]`).
///
/// Selection rules (kept simple so the shape is unit-testable):
///
/// * `mode` non-empty → legacy path via [`injection::handle_inject_text`].
///   Preserves the legacy on-disk contract without a shim.
/// * `text_arg` some → public path via
///   [`injection::handle_public_inject_text`].
/// * neither → error: the user didn't tell us what to inject. Prints a hint
///   at both invocation shapes so they know both exist.
fn dispatch_inject_text(cmd: Command) -> anyhow::Result<()> {
    // Destructuring the enum variant here keeps clippy's too-many-arguments
    // check happy while still giving us named locals for each field.
    let Command::InjectText {
        text_arg,
        dry_run,
        do_it,
        backend,
        json,
        mode,
        text,
        xkb_layout,
        target_title,
        target_process,
    } = cmd
    else {
        unreachable!("dispatch_inject_text called with non-InjectText variant")
    };
    if !mode.is_empty() {
        // Legacy hidden-helper path: honour --mode + --text + --xkb-layout.
        // The public flags are ignored on this path.
        return injection::handle_inject_text(
            &mode,
            &text,
            &xkb_layout,
            &target_title,
            &target_process,
        );
    }
    let Some(text_positional) = text_arg else {
        return Err(anyhow::anyhow!(
            "inject-text: pass TEXT as a positional argument \
             (e.g. `wd inject-text \"smoke test\"`) or use the \
             legacy `--mode {{type|paste}} --text ...` helper form"
        ));
    };
    injection::handle_public_inject_text(
        &text_positional,
        &backend,
        dry_run,
        do_it,
        json,
        &target_title,
        &target_process,
    )
}

#[cfg(feature = "audio-capture")]
fn handle_devices_command() -> anyhow::Result<()> {
    whisper_dictate_app::devices::handle_devices()
}

#[cfg(not(feature = "audio-capture"))]
fn handle_devices_command() -> anyhow::Result<()> {
    // Stable, machine-readable refusal so callers can detect
    // "not built with cpal" without parsing a free-form error message.
    // Exits non-zero so the caller's returncode check trips its fallback
    // path.
    println!(
        "{{\"error\":\"devices_unavailable\",\"reason\":\"binary built without audio-capture feature\"}}"
    );
    std::process::exit(2);
}

/// Handle `wd devices test <NAME>`.
///
/// On `audio-capture` builds (the shipping binary) this dispatches to the
/// native cpal probe in [`audio::device_probe`] and prints the single-line
/// JSON envelope the UI parser in `ui::device_test` expects.
///
/// On stock builds (no `audio-capture`) the subcommand is unavailable: the
/// binary emits a clear "rebuild with --features audio-capture" refusal on
/// stderr and exits non-zero. The shipping binary always ships with
/// `audio-capture`; only dev builds hit this path.
#[cfg(feature = "audio-capture")]
fn handle_devices_test(name: &str) -> anyhow::Result<()> {
    let result = whisper_dictate_app::audio::device_probe::probe_device(name);
    println!("{}", result.to_json_line());
    Ok(())
}

#[cfg(not(feature = "audio-capture"))]
fn handle_devices_test(_name: &str) -> anyhow::Result<()> {
    // Emit a machine-readable refusal on stderr and exit non-zero so the
    // caller can distinguish "not built with the native probe" from an
    // actual probe failure.
    eprintln!(
        "devices test is unavailable: this binary was built without the \
         `audio-capture` feature. Rebuild with `cargo build --features audio-capture`."
    );
    std::process::exit(2);
}
