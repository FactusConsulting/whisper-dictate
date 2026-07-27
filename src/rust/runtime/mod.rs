//! Runtime module: supervises the Python worker child process, owns
//! command construction and installation planning, wires the Rust
//! hotkey / audio-bridge sinks, and exposes the CLI-level entry points
//! (`run_terminal`, `doctor`, `install`, `setup_ubuntu`, `version`,
//! `cleanup_stale_desktop_processes`).
//!
//! Historically all of the above lived in a single 2200-LOC
//! `runtime.rs`. That file was split into the submodules below as part
//! of the 500-LOC modularity refactor (docs/architecture-audit
//! 2026-07-16). The `pub use` re-exports at the bottom preserve every
//! symbol's canonical `crate::runtime::Foo` path so no caller needed
//! to move.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};

// ---------------------------------------------------------------------------
// Submodule declarations. Existing sibling files (audio_spawn, the
// rust_session_* group, and the test-only submodules) survived the
// split unchanged; the new post-refactor files
// (supervisor / control / audio_bridge / hotkey_install /
// worker_command / install_plan / process) hold the code that used to
// live inline here.
// ---------------------------------------------------------------------------

pub mod audio_spawn;

// Audit item 5 Phase A step 1: the `whisper-dictate dictate-run` CLI verb —
// foreground driver that installs the Rust dictation runtime end-to-end. Not
// wired into the Python entrypoint yet; a follow-up PR (Phase A step 2)
// adds the `VOICEPI_DICTATE_ENGINE=rust` dispatch branch in
// `runtime.py::_run_session` that shells out to it. Kept as a top-level
// module (not `pub(crate)`) so `main.rs::dispatch_dictate_run` can call the
// handler without an extra re-export.
pub mod dictate_run;

// Audit item 5 Phase B step 1: in-process Rust dictation dispatch. When the
// operator opts in via `VOICEPI_DICTATE_ENGINE=rust`, the supervisor
// installs the Rust runtime inside the UI process instead of spawning a
// Python worker child — removing the Phase A subprocess layer from the
// runtime supervision ladder. See `docs/design/item5-phase-b-inprocess.md`.
pub(crate) mod in_process;

mod control;
pub(crate) mod hotkey_install;
pub(crate) mod install_plan;
pub(crate) mod process;
pub(crate) mod supervisor;
pub(crate) mod worker_command;

// Feature-gated: the impl block for the audio-bridge ready-watch and
// error-loop methods on `RuntimeSupervisor`. Only compiles into the
// crate when `--features audio-in-rust` is on; the module file itself
// carries a matching `#![cfg]`.
#[cfg(feature = "audio-in-rust")]
mod audio_bridge;

// Wave 5 PR 4 of #348: opt-in (`VOICEPI_DICTATE_BACKEND=rust-session`)
// wiring that drives a `DictateSession` from the hotkey coordinator's
// action sink. Stays out of the production path until PR 6 flips the
// default. Lives in its own file so this module does not grow past
// the 500-LOC modularity guideline.
pub(crate) mod rust_session_sink;

// Wave 5 PR 5 of #348: real-backend constructor for the session sink.
// Gated on `whisper-rs-local + rust-injection` so default builds compile
// zero new code from this PR. The sink in `rust_session_sink::build_production_sink`
// calls into this module to construct a `DictateSession<WhisperLocalTranscribeBackend,
// ProductionInjectBackend>`; on feature absence OR model-resolution failure it
// falls back to the PR 4 stub session so the wire-up still installs.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) mod rust_session_real_backends;

// Codex P1 #608 rust_session_real_backends.rs:372 -- the in-process
// preview sink that routes preview events onto the `RuntimeEvent`
// channel (the pre-fix sink wrote them to stderr, invisible to the
// in-process UI). Split into its own module so the parent stays
// under the AGENTS.md 500-LOC modularity limit.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) mod rust_session_preview;

// Wave 5 PR 5 of #348 round 2 (Codex P2 #423 finding 4): production
// `InjectBackend` wrapper that honors `VOICEPI_INJECT_MODE=print`
// (stdout-only dry-run). Modifier release lives inside
// `dictate/backends/inject.rs::EnigoInjectBackend` itself (Codex P2
// #417 inject.rs:110 follow-up in PR #419) so the wrapper just
// delegates for the Enigo arm. Gated on the same feature pair the
// real-backend module requires; without whisper-rs-local nothing
// constructs the wrapper and its items would dead-code.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) mod rust_session_inject;

// Wave 5 PR 5 of #348 round 2 (Codex P1 #423 finding 1): audio-pump
// that forwards `AudioPipeline` frames into the real
// `DictateSession`'s `push_frame`. Without this the rust-session path
// captured no audio and every stop hit the `no_audio` early-return.
// Gated on all three features the full real-backend path requires.
#[cfg(all(
    feature = "whisper-rs-local",
    feature = "rust-injection",
    feature = "audio-in-rust"
))]
pub(crate) mod rust_session_audio;

// ---------------------------------------------------------------------------
// Test submodules — same layout as before the 500-LOC split; their
// `use super::*;` still resolves against the re-exports below.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod app_root_tests;
#[cfg(test)]
mod audio_backend_tests;
#[cfg(test)]
mod audio_spawn_tests;
#[cfg(all(test, feature = "audio-in-rust"))]
mod bridge_terminal_tests;
#[cfg(test)]
mod desktop_entry_tests;
#[cfg(test)]
mod hotkey_supervisor_tests;
#[cfg(test)]
mod install_plan_tests;
// Sibling tests for `in_process` (Phase B step 1). Moved out of the
// module body in the review-response round so the production module
// stays under the AGENTS.md 500-LOC modularity limit (Codex P2 PR
// #519 in_process.rs:444).
#[cfg(test)]
mod in_process_tests;
#[cfg(test)]
mod process_capture_tests;
// Sibling tests for `rust_session_sink` (Wave 5 PR 4 of #348). Split
// across three files to keep each under the ~500-LOC modularity
// guideline (AGENTS.md "Review guidelines", Codex P2 PR #421):
// - `rust_session_sink_tests`: pure helpers + EventForwarder framing.
// - `rust_session_sink_coverage_tests`: Sonar gate-uplift targets.
// - `rust_session_sink_e2e_tests`: synthetic Press/Release/Cancel
//   integration tests through the coordinator + session.
#[cfg(test)]
mod rust_session_sink_coverage_tests;
#[cfg(test)]
mod rust_session_sink_e2e_tests;
#[cfg(test)]
mod rust_session_sink_tests;
// rust_session_audio / rust_session_inject / rust_session_real_backends
// declare their own `#[cfg(test)] mod tests;` (with a `#[path]` attribute
// pointing at the sibling `*_tests.rs`) inside the module file itself,
// so mod.rs does not repeat those declarations.
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod ubuntu_setup_tests;
#[cfg(test)]
mod windows_process_tests;
#[cfg(test)]
mod worker_command_tests;
#[cfg(test)]
mod worker_event_tests;

// ---------------------------------------------------------------------------
// Public API re-exports. Preserves `crate::runtime::Foo` for every
// item that used to live directly in `runtime.rs`.
// ---------------------------------------------------------------------------

pub use hotkey_install::{
    disable_python_hotkey, maybe_install_rust_hotkey, rust_hotkey_backend_active,
    rust_hotkey_backend_requested,
};
// Test-visible re-exports (submodule tests do `use super::*;` to
// reach the pure helpers by their bare names). Not needed in prod
// builds — every non-test caller inside the runtime module imports
// these directly from `super::hotkey_install::...`, hence the
// #[allow(unused_imports)] on the pub(crate) re-export block.
#[allow(unused_imports)]
pub(crate) use hotkey_install::{
    extract_hotkey_key_names, install_rust_hotkey_from_command,
    normalise_hotkey_aliases_for_python, normalise_hotkey_chord_for_python, parse_toggle_value,
    restart_hotkey_decision, RestartHotkeyDecision,
};
pub use install_plan::install;
pub use process::{
    decode_capped_output, run_capture, run_foreground, WorkerOutput, CAPTURE_OUTPUT_MAX_CHARS,
};
pub use supervisor::{RepaintNotifier, RuntimeEvent, RuntimeState, RuntimeSupervisor, WorkerEvent};
pub use worker_command::{
    audio_devices_command, audio_pipeline_available, audio_pipeline_requested, cli_exe_path,
    default_worker_command, default_worker_command_with_args, doctor_command, install_command,
    install_command_from_exe, resource_app_root, windows_command, worker_command,
    worker_command_with_args, PlannedCommand, WorkerCommand, AUDIO_BACKEND_ENV,
};

// ---------------------------------------------------------------------------
// Test-visible re-exports. The runtime submodule tests do
// `use super::*;` (i.e. glob-import every item that appears at
// `crate::runtime::`) so every private helper they poke at needs a
// crate-visible re-export from this module. Grouped by source
// submodule so the glue-back is easy to follow.
// ---------------------------------------------------------------------------

#[allow(unused_imports)]
pub(crate) use install_plan::{requirements_path, InstallPlan};
#[allow(unused_imports)]
pub(crate) use process::{
    parse_worker_event, PYTHON_IO_ENCODING_ENV, PYTHON_UTF8_ENV, WORKER_EVENT_PREFIX,
};
#[allow(unused_imports)]
pub(crate) use worker_command::{
    app_root_from_exe_path, cli_exe_from, default_python_name, default_venv_dir,
    propagate_rust_devices_backend, source_root, venv_python_path, windows_venv_dir, Platform,
    APP_ROOT_ENV, PYTHONPATH_ENV, PYTHON_ENV,
};

// ---------------------------------------------------------------------------
// CLI entry points that still live in this module: run_terminal /
// doctor / setup_ubuntu / version, plus the Linux desktop-entry
// installers and the Windows stale-process sweep. Kept here (rather
// than in a submodule) because they are the file-scoped glue tying
// the CLI to the rest of the runtime submodules.
// ---------------------------------------------------------------------------

pub fn run_terminal(args: Vec<String>) -> Result<()> {
    let mut command = default_worker_command_with_args(args);
    attach_cloud_api_keys(&mut command);
    run_foreground(&command)
}

/// Give the worker the cloud API keys the user already saved in Settings.
///
/// Until this existed only the UI could read the credential store, so it was
/// the only entry point that could start a cloud-configured worker. A bare
/// `whisper-dictate run` -- including the terminal test documented in
/// `docs/testing-rust-engine-v1.22.md` -- died at startup with
/// "openai API requires OPENAI_API_KEY, GROQ_API_KEY, or
/// VOICEPI_STT_API_KEY/VOICEPI_POST_API_KEY" on a machine where the key was
/// saved and working in the UI.
///
/// The key travels in the child's ENVIRONMENT, never argv: a command line is
/// readable by other local users (the leak fixed in #588).
fn attach_cloud_api_keys(command: &mut WorkerCommand) {
    let settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        // No readable config: nothing to resolve a provider from. The worker
        // reports the missing key itself, which is a better message than
        // anything invented here.
        Err(_) => return,
    };

    // Classify the credential against the endpoint AND the effective mode the
    // WORKER will actually run in, not the raw config values.
    // `worker_env_overrides()` has already baked env-var overrides into
    // `command.env` (env > config > default), so resolving against
    // `command.env` is what keeps the credential lookup aligned with the
    // transcribe layer. Ignoring the endpoint override leads to
    // `VOICEPI_STT_BASE_URL=https://api.openai.com/v1 whisper-dictate run`
    // reaching for the Groq key saved for the config value; ignoring the
    // BACKEND override (Codex P1 #615: `VOICEPI_STT_BACKEND=openai` /
    // `VOICEPI_POST_PROCESSOR=groq` set only in the shell) makes the gates in
    // `stt_credential_for` / `post_credential_for` short-circuit against the
    // saved `whisper` / `none` defaults and never read the store at all --
    // the worker then starts without the key that was saved through Settings.
    let stt_endpoint =
        effective_endpoint(&command.env, "VOICEPI_STT_BASE_URL", &settings.stt_base_url);
    let post_endpoint = effective_endpoint(
        &command.env,
        "VOICEPI_POST_BASE_URL",
        &settings.post_base_url,
    );
    let stt_backend = effective_setting(
        &command.env,
        crate::dictate::backends::cloud_transcribe::STT_BACKEND_ENV,
        &settings.stt_backend,
    );
    let post_processor = effective_setting(
        &command.env,
        crate::postprocess::POST_PROCESSOR_ENV,
        &settings.post_processor,
    );

    let additions = cloud_api_key_env_additions(
        &command.env,
        |name| std::env::var(name).ok(),
        stt_credential_for(&stt_backend, &stt_endpoint),
        post_credential_for(&post_processor, &post_endpoint),
    );
    command.env.extend(additions);
}

/// The base URL the worker will resolve to, given the env the spawner has
/// already assembled and the config's own value. Split from
/// [`attach_cloud_api_keys`] so the precedence is unit-testable without a
/// config file or a credential store.
fn effective_endpoint(env: &[(String, String)], name: &str, config_value: &str) -> String {
    effective_setting(env, name, config_value)
}

/// Generalised env-first setting resolver: prefer a non-blank value already
/// in `command.env` (the spawner has already applied env > config > default
/// via [`crate::config::schema::worker_env_overrides`]), otherwise fall back
/// to the raw config value. Kept as a separate helper so the credential
/// wiring can look up ANY effective mode (backend, processor, base URL) with
/// the same precedence rule -- Codex P1 #615.
fn effective_setting(env: &[(String, String)], name: &str, config_value: &str) -> String {
    env.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| config_value.to_owned())
}

/// Only fetch an STT credential when a cloud backend is actually active. A
/// local-Whisper user has nothing to look up, and skipping the read avoids
/// gratuitous keyring prompts on some Windows setups. Kept exactly aligned
/// with the schema's `stt_backend` values: `whisper` (local) vs. anything
/// cloud-shaped -- currently only `openai`.
fn stt_credential_for(stt_backend: &str, endpoint: &str) -> Option<String> {
    (stt_backend == "openai")
        .then(|| crate::credentials::resolve_stt_api_key(endpoint))
        .flatten()
}

/// Only fetch a post-processing credential when a cloud post-processor is
/// active. `none` and `ollama` are both local (no cloud endpoint), so the
/// credential lookup is skipped. Matches the schema's `post_processor`
/// values: `none` / `ollama` / `openai` / `groq`.
fn post_credential_for(post_processor: &str, endpoint: &str) -> Option<String> {
    matches!(post_processor, "openai" | "groq")
        .then(|| crate::credentials::resolve_post_api_key(endpoint))
        .flatten()
}

/// Which key variables to add to the worker's env, given what is already
/// there. Split from [`attach_cloud_api_keys`] so the PRECEDENCE of the
/// wiring is unit-testable without a config file, a credential store, or a
/// spawned process -- the resolver having correct precedence says nothing
/// about whether the caller wired it up correctly, and it was the wiring that
/// was missing entirely.
///
/// An existing value always wins, whether it came from the caller-built
/// command or the ambient environment, so
/// `VOICEPI_STT_API_KEY=... whisper-dictate run` still overrides the store.
fn cloud_api_key_env_additions<E>(
    existing: &[(String, String)],
    env_lookup: E,
    stt: Option<String>,
    post: Option<String>,
) -> Vec<(String, String)>
where
    E: Fn(&str) -> Option<String>,
{
    let mut out = Vec::new();
    for (name, resolved) in [("VOICEPI_STT_API_KEY", stt), ("VOICEPI_POST_API_KEY", post)] {
        if existing.iter().any(|(k, _)| k == name) {
            continue;
        }
        if env_lookup(name).is_some_and(|v| !v.trim().is_empty()) {
            continue;
        }
        if let Some(value) = resolved {
            out.push((name.to_owned(), value));
        }
    }
    out
}

pub fn doctor() -> Result<()> {
    run_foreground(&doctor_command())
}

pub fn setup_ubuntu() -> Result<()> {
    let root = resource_app_root();
    let script = ubuntu_setup_script_path(&root);
    if !script.exists() {
        return Err(anyhow!(
            "Ubuntu setup script not found at {}",
            script.display()
        ));
    }
    let status = Command::new("bash")
        .arg(&script)
        .env("VOICEPI_RUST_OWNS_DESKTOP", "1")
        .status()?;
    if status.success() {
        install_linux_desktop_entries()?;
        start_linux_ui_detached()?;
        Ok(())
    } else {
        Err(anyhow!(
            "Ubuntu setup failed with exit code {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

fn ubuntu_setup_script_path(root: &Path) -> PathBuf {
    let packaging_path = root
        .join("packaging")
        .join("linux")
        .join("ubuntu26.04")
        .join("setup.sh");
    if packaging_path.exists() {
        return packaging_path;
    }
    root.join("ubuntu26.04").join("setup.sh")
}

fn install_linux_desktop_entries() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let home = home_dir().ok_or_else(|| anyhow!("HOME is not set"))?;
    let applications = home.join(".local/share/applications");
    let autostart = home.join(".config/autostart");
    std::fs::create_dir_all(&applications)?;
    std::fs::create_dir_all(&autostart)?;

    let icon = linux_app_icon_path(&home);
    let exec = linux_desktop_exec_command();
    let desktop = linux_desktop_entry(false, &exec, &icon);
    let autostart_desktop = linux_desktop_entry(true, &exec, &icon);
    let app_path = applications.join("whisper-dictate.desktop");
    let autostart_path = autostart.join("whisper-dictate.desktop");
    std::fs::write(&app_path, desktop)?;
    std::fs::write(&autostart_path, autostart_desktop)?;
    install_linux_app_icon(&home)?;

    let _ = Command::new("update-desktop-database")
        .arg(&applications)
        .status();
    println!("Desktop launcher: {}", app_path.display());
    println!("Autostart entry: {}", autostart_path.display());
    Ok(())
}

fn install_linux_app_icon(home: &Path) -> Result<()> {
    let icon_dir = linux_app_icon_path(home)
        .parent()
        .ok_or_else(|| anyhow!("invalid Linux app icon path"))?
        .to_path_buf();
    std::fs::create_dir_all(&icon_dir)?;
    std::fs::write(
        linux_app_icon_path(home),
        include_str!("../../../assets/whisper-dictate-logo.svg"),
    )?;
    Ok(())
}

fn linux_app_icon_path(home: &Path) -> PathBuf {
    home.join(".local/share/icons/hicolor/scalable/apps/whisper-dictate.svg")
}

fn linux_desktop_exec_command() -> String {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate"));
    format!("{} ui", desktop_exec_token(&exe))
}

fn desktop_exec_token(path: &Path) -> String {
    let raw = path.display().to_string();
    if raw
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'))
    {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        raw
    }
}

fn linux_desktop_entry(autostart: bool, exec: &str, icon: &Path) -> String {
    let icon = icon.display();
    let mut entry = format!(
        "[Desktop Entry]\n\
Name=Whisper Dictate\n\
Comment=Push-to-talk dictation settings and runtime control\n\
Exec={exec}\n\
Icon={icon}\n\
Terminal=false\n\
Type=Application\n\
Categories=Utility;AudioVideo;Audio;\n\
StartupNotify=true\n",
    );
    entry.push_str("StartupWMClass=whisper-dictate\n");
    if autostart {
        entry.push_str("X-GNOME-Autostart-enabled=true\n");
    }
    entry
}

fn start_linux_ui_detached() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("whisper-dictate"));
    if command_exists("gtk-launch") {
        Command::new("gtk-launch").arg("whisper-dictate").spawn()?;
        println!("Started Whisper Dictate UI via app launcher.");
    } else if command_exists("setsid") {
        Command::new("setsid").arg(&exe).arg("ui").spawn()?;
        println!("Started Whisper Dictate UI.");
    } else {
        Command::new(&exe).arg("ui").spawn()?;
        println!("Started Whisper Dictate UI.");
    }
    Ok(())
}

fn command_exists(program: &str) -> bool {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join(program).exists()))
        .unwrap_or(false)
}

// Shared with the worker-command builders: both need HOME/USERPROFILE
// to find the default install location. Kept private to this module
// and re-defined here so mod.rs does not depend on
// `worker_command::home_dir` (which is `pub(super)` to allow
// install_plan / worker_command to share).
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(windows)]
pub fn cleanup_stale_desktop_processes() {
    if let Err(err) = cleanup_stale_desktop_processes_windows() {
        eprintln!("warning: could not clean stale whisper-dictate processes: {err}");
    }
}

#[cfg(not(windows))]
pub fn cleanup_stale_desktop_processes() {}

#[cfg(windows)]
fn cleanup_stale_desktop_processes_windows() -> Result<()> {
    let current_pid = std::process::id();
    let exe = env::current_exe()
        .ok()
        .unwrap_or_else(|| PathBuf::from("whisper-dictate.exe"));
    let app_root = resource_app_root();
    let script = stale_process_cleanup_script(current_pid, &exe, &app_root);

    let mut command = Command::new(windows_shell_program());
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &script,
    ]);
    process::configure_background_process(&mut command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("stale process cleanup exited with {status}"))
    }
}

#[cfg(windows)]
fn stale_process_cleanup_script(current_pid: u32, exe: &Path, app_root: &Path) -> String {
    let exe = escape_powershell_single_quoted(&exe.display().to_string());
    let app_root = escape_powershell_single_quoted(&app_root.display().to_string());
    format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
$currentPid = {current_pid}
$cleanupPid = $PID
$exe = '{exe}'
$root = '{app_root}'
Get-CimInstance Win32_Process |
  Where-Object {{
    $_.ProcessId -ne $currentPid -and $_.ProcessId -ne $cleanupPid -and (
      ($_.ExecutablePath -eq $exe) -or
      ($_.CommandLine -like "*whisper_dictate.runtime*" -and $_.CommandLine -like "*$root*")
    )
  }} |
  ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}
"#
    )
}

#[cfg(windows)]
fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(windows)]
fn windows_shell_program() -> &'static str {
    if env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join("pwsh.exe").exists()))
        .unwrap_or(false)
    {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}

pub fn version() -> String {
    let root = resource_app_root();
    if let Ok(raw) = std::fs::read_to_string(root.join("VERSION")) {
        let version = raw.trim().trim_start_matches('v');
        if !version.is_empty() {
            return version.to_owned();
        }
    }

    if let Ok(output) = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .current_dir(&root)
        .output()
    {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim().trim_start_matches('v');
            if !version.is_empty() {
                return version.to_owned();
            }
        }
    }

    env!("CARGO_PKG_VERSION").to_owned()
}

#[cfg(test)]
mod cloud_api_key_wiring_tests {
    use super::{
        cloud_api_key_env_additions, effective_endpoint, post_credential_for, stt_credential_for,
    };

    fn none(_: &str) -> Option<String> {
        None
    }

    fn names(v: &[(String, String)]) -> Vec<&str> {
        v.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn store_keys_are_added_when_nothing_is_set() {
        // The actual bug: key saved in the UI, no env exported, worker
        // started without it and died at startup.
        let got = cloud_api_key_env_additions(
            &[],
            none,
            Some("stt-from-store".to_owned()),
            Some("post-from-store".to_owned()),
        );
        assert_eq!(
            names(&got),
            vec!["VOICEPI_STT_API_KEY", "VOICEPI_POST_API_KEY"]
        );
        assert_eq!(got[0].1, "stt-from-store");
    }

    #[test]
    fn ambient_environment_wins_over_the_store() {
        let got = cloud_api_key_env_additions(
            &[],
            |name| (name == "VOICEPI_STT_API_KEY").then(|| "from-env".to_owned()),
            Some("from-store".to_owned()),
            None,
        );
        assert!(
            got.is_empty(),
            "an exported key must not be overridden by the store: {got:?}"
        );
    }

    #[test]
    fn blank_ambient_value_does_not_block_the_store() {
        // `export VOICEPI_STT_API_KEY=` is a leftover, not a choice.
        let got = cloud_api_key_env_additions(
            &[],
            |name| (name == "VOICEPI_STT_API_KEY").then(|| "   ".to_owned()),
            Some("from-store".to_owned()),
            None,
        );
        assert_eq!(names(&got), vec!["VOICEPI_STT_API_KEY"]);
    }

    #[test]
    fn a_key_already_on_the_command_is_left_alone() {
        let existing = vec![("VOICEPI_STT_API_KEY".to_owned(), "caller".to_owned())];
        let got = cloud_api_key_env_additions(&existing, none, Some("from-store".to_owned()), None);
        assert!(
            got.is_empty(),
            "must not duplicate an existing entry: {got:?}"
        );
    }

    #[test]
    fn unresolvable_keys_add_nothing() {
        // A local-Whisper user has no cloud key at all; the worker must not
        // be handed an empty variable that looks configured.
        assert!(cloud_api_key_env_additions(&[], none, None, None).is_empty());
    }

    #[test]
    fn the_two_keys_are_decided_independently() {
        // STT exported, post only in the store: exactly one addition.
        let got = cloud_api_key_env_additions(
            &[],
            |name| (name == "VOICEPI_STT_API_KEY").then(|| "from-env".to_owned()),
            Some("stt-store".to_owned()),
            Some("post-store".to_owned()),
        );
        assert_eq!(names(&got), vec!["VOICEPI_POST_API_KEY"]);
    }

    fn env(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn effective_endpoint_prefers_the_command_env_over_the_config() {
        // The regression the P1 review flagged: the schema materialises
        // `VOICEPI_STT_BASE_URL` into `command.env` (env > config > default),
        // and the credential lookup must honour that -- otherwise a runtime
        // env override sends the worker to one provider while we hand it
        // another provider's saved key.
        let e = env(&[("VOICEPI_STT_BASE_URL", "https://api.openai.com/v1")]);
        let got = effective_endpoint(&e, "VOICEPI_STT_BASE_URL", "https://api.groq.com/openai/v1");
        assert_eq!(got, "https://api.openai.com/v1");
    }

    #[test]
    fn effective_endpoint_falls_back_to_the_config_when_env_missing_or_blank() {
        // Nothing in command.env -> settings value wins.
        let got = effective_endpoint(
            &env(&[]),
            "VOICEPI_STT_BASE_URL",
            "https://api.groq.com/openai/v1",
        );
        assert_eq!(got, "https://api.groq.com/openai/v1");
        // Whitespace-only env value is a leftover, not an override.
        let got = effective_endpoint(
            &env(&[("VOICEPI_STT_BASE_URL", "   ")]),
            "VOICEPI_STT_BASE_URL",
            "https://api.groq.com/openai/v1",
        );
        assert_eq!(got, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn env_override_of_endpoint_reclassifies_the_provider() {
        // The end-to-end shape of the P1 finding: `Provider::from_base_url`
        // must be applied AFTER `effective_endpoint`, so the credential is
        // looked up against the endpoint the worker will actually reach.
        // Two config-vs-env combinations map to two different stored
        // accounts; the assertion is on the classification, which is what
        // decides which account is read.
        use crate::credentials::Provider;
        let e = env(&[("VOICEPI_STT_BASE_URL", "https://api.openai.com/v1")]);
        let endpoint =
            effective_endpoint(&e, "VOICEPI_STT_BASE_URL", "https://api.groq.com/openai/v1");
        assert_eq!(Provider::from_base_url(&endpoint), Provider::OpenAi);
        // And without the env override, we would have gone to Groq -- proving
        // the two branches actually diverge.
        let cfg_only = effective_endpoint(
            &env(&[]),
            "VOICEPI_STT_BASE_URL",
            "https://api.groq.com/openai/v1",
        );
        assert_eq!(Provider::from_base_url(&cfg_only), Provider::Groq);
    }

    #[test]
    fn stt_credential_skipped_for_local_whisper_backend() {
        // Local Whisper has no cloud key. Even if the store WOULD return
        // something, `stt_credential_for` must not consult it -- the wiring
        // stays out of the credential store entirely.
        assert!(stt_credential_for("whisper", "https://api.groq.com/openai/v1").is_none());
        // Sanity: an unknown backend also skips (fail-closed).
        assert!(stt_credential_for("mystery", "https://api.groq.com/openai/v1").is_none());
    }

    #[test]
    fn post_credential_skipped_for_local_post_processors() {
        // `none` and `ollama` are both local -- no cloud endpoint, no key.
        assert!(post_credential_for("none", "https://api.openai.com/v1").is_none());
        assert!(post_credential_for("ollama", "http://localhost:11434").is_none());
    }

    #[test]
    fn effective_setting_prefers_the_command_env_over_the_config() {
        // Codex P1 #615: `attach_cloud_api_keys` must derive the effective
        // stt_backend / post_processor from `command.env` (the schema has
        // already applied env > config > default), not the raw saved
        // settings -- otherwise the credential-lookup gates in
        // `stt_credential_for` / `post_credential_for` short-circuit against
        // the config's `whisper` / `none` defaults and never touch the store.
        let e = env(&[("VOICEPI_STT_BACKEND", "openai")]);
        assert_eq!(
            super::effective_setting(&e, "VOICEPI_STT_BACKEND", "whisper"),
            "openai",
            "env override must win"
        );
        let e = env(&[("VOICEPI_POST_PROCESSOR", "groq")]);
        assert_eq!(
            super::effective_setting(&e, "VOICEPI_POST_PROCESSOR", "none"),
            "groq"
        );
        // No env override -> fall back to the raw settings value.
        assert_eq!(
            super::effective_setting(&env(&[]), "VOICEPI_STT_BACKEND", "openai"),
            "openai"
        );
        // Whitespace-only env value is a leftover; the config wins.
        let e = env(&[("VOICEPI_STT_BACKEND", "   ")]);
        assert_eq!(
            super::effective_setting(&e, "VOICEPI_STT_BACKEND", "whisper"),
            "whisper"
        );
    }

    #[test]
    fn env_override_of_backend_activates_the_credential_gate() {
        // End-to-end shape of the P1 finding: the config still says
        // `stt_backend=whisper`, but the launcher sees
        // `VOICEPI_STT_BACKEND=openai` in the effective command env. The
        // effective backend must be `openai` so `stt_credential_for` opens
        // the store; using the raw settings value would keep it closed and
        // start the worker without the saved key.
        let e = env(&[("VOICEPI_STT_BACKEND", "openai")]);
        let effective = super::effective_setting(&e, "VOICEPI_STT_BACKEND", "whisper");
        assert_eq!(effective, "openai");
        // The gate itself is exercised in
        // `stt_credential_skipped_for_local_whisper_backend`; here we assert
        // the input plumbing that decides which branch that gate takes.
    }
}
