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
    audio_devices_command, audio_pipeline_available, audio_pipeline_requested, benchmark_command,
    cli_exe_path, default_worker_command, default_worker_command_with_args, doctor_command,
    install_command, install_command_from_exe, record_corpus_item_command, resource_app_root,
    test_audio_device_command, windows_command, worker_command, worker_command_with_args,
    PlannedCommand, WorkerCommand, AUDIO_BACKEND_ENV,
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
    let command = default_worker_command_with_args(args);
    run_foreground(&command)
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
