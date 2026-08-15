//! Runtime module: supervises the native dictation session, owns
//! native launch configuration, and exposes the CLI-level entry points
//! (`run_terminal`, `setup_ubuntu`, `version`).
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
use clap::Parser;

// ---------------------------------------------------------------------------
// Submodule declarations. Existing sibling files (audio_spawn, the
// rust_session_* group, and the test-only submodules) survived the
// split unchanged; the new post-refactor files
// (supervisor / control / worker_command) hold the code that used to
// live inline here.
// ---------------------------------------------------------------------------

pub mod audio_spawn;

// Foreground driver that installs the Rust dictation runtime end-to-end.
// Originally introduced as the hidden Phase A `dictate-run` bridge; the public
// `wd run` route now calls it directly, while the hidden verb
// remains available to compatibility callers. Kept public so `main.rs` can
// dispatch the hidden verb without an extra re-export.
pub mod dictate_run;
#[cfg(any(all(feature = "rust-hotkeys", feature = "rust-injection"), test))]
mod dictate_run_output;

// Native in-process Rust dictation dispatch.
#[cfg(feature = "rust-hotkeys")]
pub(crate) mod hotkey_probe;
pub(crate) mod in_process;

const HOTKEY_PROBE_CHILD_ARG: &str = "--internal-hotkey-probe";

/// Internal child-process dispatch used by the guided hotkey verifier. Both
/// binaries call this before normal argument handling so the spawned child is
/// the same executable (and, on Windows, the same subsystem) as its UI parent.
#[doc(hidden)]
pub fn hotkey_probe_child_requested() -> bool {
    std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(HOTKEY_PROBE_CHILD_ARG))
}

#[doc(hidden)]
pub fn run_hotkey_probe_child() -> Result<()> {
    let args = std::iter::once(std::ffi::OsString::from("wd"))
        .chain(std::env::args_os().skip(2))
        .collect::<Vec<_>>();
    let cli = crate::cli::Cli::try_parse_from(args)?;
    match cli.command {
        Some(crate::cli::Command::Hotkey { command }) => {
            crate::hotkey::capture::handle_hotkey_command(command)
        }
        _ => Err(anyhow!(
            "the internal hotkey probe accepts only the hotkey capture command"
        )),
    }
}

pub mod cloud_api_keys;
mod control;
pub(crate) mod live_settings;
pub(crate) mod settings_snapshot;
pub(crate) mod supervisor;
mod terminal_run;
pub(crate) mod worker_command;

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

// VAD-free audio pump that forwards raw capture frames into the real
// `DictateSession`. Gated on all three features the full backend requires.
#[cfg(all(
    feature = "whisper-rs-local",
    feature = "rust-injection",
    feature = "audio-capture"
))]
pub(crate) mod rust_session_audio;

// ---------------------------------------------------------------------------
// Test submodules — same layout as before the 500-LOC split; their
// `use super::*;` still resolves against the re-exports below.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod app_root_tests;
#[cfg(test)]
mod audio_spawn_tests;
// Sibling tests for `in_process` (Phase B step 1). Moved out of the
// module body in the review-response round so the production module
// stays under the AGENTS.md 500-LOC modularity limit (Codex P2 PR
// #519 in_process.rs:444).
#[cfg(test)]
mod in_process_tests;
#[cfg(test)]
mod terminal_run_tests;
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
mod supervisor_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod ubuntu_setup_tests;
#[cfg(test)]
#[cfg(test)]
mod worker_command_tests;

// ---------------------------------------------------------------------------
// Public API re-exports. Preserves `crate::runtime::Foo` for every
// item that used to live directly in `runtime.rs`.
// ---------------------------------------------------------------------------

pub use supervisor::{RepaintNotifier, RuntimeEvent, RuntimeState, RuntimeSupervisor, WorkerEvent};
pub use terminal_run::run_terminal;
pub use worker_command::{
    cli_exe_path, default_worker_command, resource_app_root, worker_command, WorkerCommand,
};

pub(crate) use worker_command::default_worker_command_with_ambient_env;

// ---------------------------------------------------------------------------
// Test-visible re-exports. The runtime submodule tests do
// `use super::*;` (i.e. glob-import every item that appears at
// `crate::runtime::`) so every private helper they poke at needs a
// crate-visible re-export from this module. Grouped by source
// submodule so the glue-back is easy to follow.
// ---------------------------------------------------------------------------

#[allow(unused_imports)]
pub(crate) use worker_command::{app_root_from_exe_path, cli_exe_from, source_root, APP_ROOT_ENV};

// ---------------------------------------------------------------------------
// CLI entry points that still live in this module: run_terminal /
// setup_ubuntu / version, plus the Linux desktop-entry
// installers and the Windows stale-process sweep. Kept here (rather
// than in a submodule) because they are the file-scoped glue tying
// the CLI to the rest of the runtime submodules.
// ---------------------------------------------------------------------------

pub fn setup_ubuntu() -> Result<()> {
    let root = resource_app_root();
    let script = ubuntu_setup_script_path(&root);
    if !script.exists() {
        return Err(anyhow!(
            "Ubuntu setup script not found at {}",
            script.display()
        ));
    }
    let mut command = Command::new("bash");
    command.arg(&script).env("VOICEPI_RUST_OWNS_DESKTOP", "1");
    settings_snapshot::scrub_credentials_from_child(&mut command);
    let status = command.status()?;
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

    let mut command = Command::new("update-desktop-database");
    command.arg(&applications);
    settings_snapshot::scrub_credentials_from_child(&mut command);
    let _ = command.status();
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
    let mut command = if command_exists("gtk-launch") {
        println!("Started Whisper Dictate UI via app launcher.");
        let mut command = Command::new("gtk-launch");
        command.arg("whisper-dictate");
        command
    } else if command_exists("setsid") {
        println!("Started Whisper Dictate UI.");
        let mut command = Command::new("setsid");
        command.arg(&exe).arg("ui");
        command
    } else {
        println!("Started Whisper Dictate UI.");
        let mut command = Command::new(&exe);
        command.arg("ui");
        command
    };
    settings_snapshot::scrub_credentials_from_child(&mut command);
    command.spawn()?;
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

pub fn version() -> String {
    let root = resource_app_root();
    if let Ok(raw) = std::fs::read_to_string(root.join("VERSION")) {
        let version = raw.trim().trim_start_matches('v');
        if !version.is_empty() {
            return version.to_owned();
        }
    }

    let mut command = Command::new("git");
    command
        .args(["describe", "--tags", "--always", "--dirty"])
        .current_dir(&root);
    settings_snapshot::scrub_credentials_from_child(&mut command);
    if let Ok(output) = command.output() {
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
