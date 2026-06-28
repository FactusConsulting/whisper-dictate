#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;

use whisper_dictate_app::cli::{Cli, Command};
use whisper_dictate_app::{
    benchmark, cloud_api, command_hook, config, corpus_record, dictate, dictionary, formatting,
    health, injection, model_capacity, postprocess, privacy, profiles, redaction, runtime,
    telemetry, ui, whisper,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Env var that opts the binary into the issue #327 single-instance
/// gate. Left OFF by default so this PR ships the machinery + tests
/// without changing default startup behaviour; the follow-up work that
/// wires forwarded commands into the running instance's event loop
/// flips the default. Kept next to the dispatch it guards so the
/// rollout switch is easy to find.
const SINGLE_INSTANCE_ENV: &str = "VOICEPI_SINGLE_INSTANCE";

fn single_instance_enabled() -> bool {
    matches!(
        std::env::var(SINGLE_INSTANCE_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.version {
        println!("whisper-dictate {}", runtime::version());
        return Ok(());
    }

    // Issue #326: forward `--toggle-recording` / `--start-recording` /
    // `--stop-recording` / `--cancel-recording` to the running daemon
    // BEFORE any subcommand dispatch (or the single-instance gate). The
    // flags are mutually exclusive with each other AND with the subcommand
    // path (enforced by clap), so this branch always wins over the UI
    // fallback when set. Exits 0 on success, non-zero with a clear message
    // when no daemon is running (so the user's wm keybinding shows a
    // helpful error instead of silently failing).
    if let Some(cmd) = cli.external_command() {
        return runtime::external_toggle::forward_command(cmd);
    }

    // Issue #327 single-instance gate. Runs BEFORE dispatch so a second
    // invocation short-circuits without touching any subcommand handler
    // (which would otherwise fight for the same hotkey / tray slot).
    // Only applies to the long-lived UI/foreground modes — one-shot
    // helpers (`config path`, hidden worker RPCs, model catalogue, …)
    // must never gate on it because the user regularly runs them
    // alongside the daemon.
    let cmd = cli.command.unwrap_or(Command::Ui);
    if single_instance_enabled() && command_is_long_running(&cmd) {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        match runtime::single_instance::try_acquire(argv)? {
            runtime::single_instance::AcquireOutcome::Forwarded => return Ok(()),
            runtime::single_instance::AcquireOutcome::Acquired(guard) => {
                // Leak the guard for the lifetime of the process — the
                // running instance holds the lock until exit. Dropping
                // it at the end of `run()` would race with the UI
                // shutting itself down and lose forwarded commands
                // that arrive during teardown.
                std::mem::forget(guard);
            }
        }
    }

    match cmd {
        Command::Ui | Command::Settings => ui::run(),
        Command::Run { args } => runtime::run_terminal(args),
        Command::Doctor => runtime::doctor(),
        Command::Bench => benchmark::handle_bench(),
        Command::CorpusRecord { id } => corpus_record::handle_corpus_record(&id),
        Command::Install => runtime::install(),
        Command::SetupUbuntu => runtime::setup_ubuntu(),
        Command::ModelCapacity { json } => model_capacity::handle_command(json),
        Command::Config { command } => config::handle_command(command),
        Command::Dictionary { command } => dictionary::handle_command(command),
        Command::DictionaryRuntime => dictionary::handle_runtime(),
        Command::DictionaryOps => dictionary::handle_ops(),
        Command::DictateOps => dictate::ops::handle_ops(),
        Command::History { command } => telemetry::handle_history_command(command),
        Command::InjectText {
            mode,
            text,
            xkb_layout,
            target_title,
            target_process,
        } => {
            injection::handle_inject_text(&mode, &text, &xkb_layout, &target_title, &target_process)
        }
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
            &api_key,
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
        Command::Devices => handle_devices_command(),
        Command::Models { command } => whisper::models_cli::handle(command),
        Command::SingleInstanceProbe {
            serve_ms,
            forward_args,
        } => handle_single_instance_probe(serve_ms, forward_args),
        Command::WorkerRust { stdin_only } => runtime::worker_rust::handle_worker_rust(stdin_only),
    }
}

/// Hidden helper backing the `single-instance-probe` subcommand — see
/// its doc-comment in `cli.rs` for the two modes. Kept small on
/// purpose: the underlying machinery is tested through the module's
/// unit tests, so this handler is essentially a thin CLI wrapper the
/// two-process integration test drives.
fn handle_single_instance_probe(
    serve_ms: Option<u64>,
    forward_args: Vec<String>,
) -> anyhow::Result<()> {
    use std::time::{Duration, Instant};
    match runtime::single_instance::try_acquire(forward_args)? {
        runtime::single_instance::AcquireOutcome::Forwarded => {
            println!("[forwarded]");
            Ok(())
        }
        runtime::single_instance::AcquireOutcome::Acquired(guard) => {
            if let Some(ms) = serve_ms {
                // Advertise readiness so the driving test can wait for
                // the port to be bound before spawning the client.
                println!(
                    "[acquired] port={} pid={}",
                    guard.port(),
                    std::process::id()
                );
                let deadline = Instant::now() + Duration::from_millis(ms);
                while Instant::now() < deadline {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if let Some(cmd) = guard.recv_timeout(remaining.min(Duration::from_millis(50)))
                    {
                        let json = serde_json::to_string(&cmd.argv).unwrap_or_default();
                        println!("[forwarded] {json}");
                    }
                }
            } else {
                println!("[acquired]");
            }
            Ok(())
        }
    }
}

/// Which sub-commands the single-instance gate applies to. Only the
/// long-lived foreground modes — the desktop UI, terminal supervisor,
/// setup helpers — can collide over the hotkey / tray slot. One-shot
/// helpers (`config path`, hidden RPC helpers used by the Python
/// worker, model-catalogue queries, …) MUST bypass the gate: the user
/// regularly runs those alongside the daemon.
fn command_is_long_running(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Ui
            | Command::Settings
            | Command::Run { .. }
            | Command::SetupUbuntu
            | Command::Install
    )
}

/// Dispatch the hidden `transcribe-wav` sub-command.
///
/// Real implementation lives in `whisper::dispatch` and is only compiled in
/// behind the `whisper-rs-local` feature (which pulls in whisper.cpp + CMake).
/// In a stock build the binary still exposes the sub-command - keeping the
/// CLI surface stable across feature builds - but exits non-zero with a
/// clear "feature not compiled in" message so the Python caller knows to
/// fall back to its in-process path.
///
/// `--probe` short-circuits before reading stdin or the model env var: it
/// exits 0 on a feature-enabled build and non-zero on a stock build, so the
/// Python wiring can cheaply check whether shelling out to this binary will
/// actually do whisper inference before committing to it for a dictation.
/// Note: ASCII-only strings here so the stderr message renders cleanly under
/// PowerShell / cmd.exe / hidden launchers and Rust UI subprocess logs
/// (AGENTS.md Windows-output rule).
#[cfg(feature = "whisper-rs-local")]
fn handle_transcribe_wav(probe: bool) -> anyhow::Result<()> {
    if probe {
        // Feature compiled in - probe succeeds without doing any work.
        return Ok(());
    }
    whisper_dictate_app::whisper::handle_transcribe_wav()
}

#[cfg(not(feature = "whisper-rs-local"))]
fn handle_transcribe_wav(_probe: bool) -> anyhow::Result<()> {
    // Same error for probe and real call: the Python caller treats any
    // non-zero exit as "Rust backend unavailable, fall back to in-process".
    Err(anyhow::anyhow!(
        "this build of whisper-dictate was compiled without the \
         `whisper-rs-local` feature; the Rust transcription backend is \
         unavailable - unset VOICEPI_TRANSCRIBE_BACKEND or install a build \
         with the feature enabled"
    ))
}

/// Wave 8-A: long-running in-process Whisper worker. See
/// [`whisper::dispatch::handle_transcribe_server`] for the wire protocol
/// and the per-request error contract; the stock-build fallback mirrors
/// `handle_transcribe_wav` above so the Python wrapper sees the same
/// "backend unavailable" exit code for either subcommand.
#[cfg(feature = "whisper-rs-local")]
fn handle_transcribe_server() -> anyhow::Result<()> {
    whisper_dictate_app::whisper::handle_transcribe_server()
}

#[cfg(not(feature = "whisper-rs-local"))]
fn handle_transcribe_server() -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "this build of whisper-dictate was compiled without the \
         `whisper-rs-local` feature; the long-running transcribe-server \
         is unavailable - install a build with the feature enabled"
    ))
}

#[cfg(feature = "audio-in-rust")]
fn handle_devices_command() -> anyhow::Result<()> {
    whisper_dictate_app::devices::handle_devices()
}

#[cfg(not(feature = "audio-in-rust"))]
fn handle_devices_command() -> anyhow::Result<()> {
    // Stable, machine-readable refusal so the Python shell-out can detect
    // "not built with cpal" and fall back to its own enumeration without
    // parsing a free-form error message. Exits non-zero so subprocess.run's
    // returncode check trips the fallback path in vp_devices.
    println!("{{\"error\":\"devices_unavailable\",\"reason\":\"binary built without audio-in-rust feature\"}}");
    std::process::exit(2);
}
