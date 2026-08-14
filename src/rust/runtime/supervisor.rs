//! Native dictation runtime supervisor.
//!
//! The supervisor owns the in-process hotkey/session installation and its
//! observable lifecycle. Python runtime fallback was retired in #703: startup
//! failures stay visible and actionable instead of changing engines.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{atomic::AtomicBool, Arc};

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::in_process::{self, InProcessInstallError, ENGINE_ENV};
use super::settings_snapshot::RuntimeSettingsSnapshot;
use super::worker_command::WorkerCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting,
    Running,
}

impl RuntimeState {
    pub fn label(self) -> &'static str {
        match self {
            RuntimeState::Stopped => "Stopped",
            RuntimeState::Starting => "Starting",
            RuntimeState::Running => "Running",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    Started { command: String },
    Worker(WorkerEvent),
    Stdout(String),
    Stderr(String),
    Exited { code: Option<i32> },
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerEvent {
    pub event: String,
    pub state: Option<String>,
    pub payload: Value,
}

pub type RepaintNotifier = std::sync::Arc<dyn Fn() + Send + Sync>;
pub(crate) type CaptureStop = std::sync::Arc<dyn Fn() + Send + Sync>;

pub struct RuntimeSupervisor {
    pub(super) state: RuntimeState,
    pub(super) tx: Sender<RuntimeEvent>,
    pub(super) rx: Receiver<RuntimeEvent>,
    pub(super) repaint_notifier: Option<RepaintNotifier>,
    pub(super) hotkey_handle: Option<crate::hotkey::HotkeyHandle>,
    pub(super) runtime_active: Option<Arc<AtomicBool>>,
    pub(super) capture_stop: Option<CaptureStop>,
    pub(super) coord_slot_keepalive:
        Option<Arc<std::sync::OnceLock<crate::hotkey::coordinator::CoordinatorHandle>>>,
    /// Completion signal for resource teardown. The hotkey coordinator owns
    /// synchronous transcription, so joining it must never block egui's thread.
    pub(super) teardown_rx: Option<Receiver<()>>,
    /// A Settings-triggered restart waits here until the prior coordinator,
    /// audio stream, model, and injector have finished shutting down.
    pub(super) pending_restart: Option<WorkerCommand>,
    pub(super) emit_exit_after_teardown: bool,
    pub(super) active_runtime: Option<RuntimeSettingsSnapshot>,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeSupervisor {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: RuntimeState::Stopped,
            tx,
            rx,
            repaint_notifier: None,
            hotkey_handle: None,
            runtime_active: None,
            capture_stop: None,
            coord_slot_keepalive: None,
            teardown_rx: None,
            pending_restart: None,
            emit_exit_after_teardown: false,
            active_runtime: None,
        }
    }

    pub fn set_repaint_notifier(&mut self, notifier: RepaintNotifier) {
        self.repaint_notifier = Some(notifier);
    }

    pub fn has_repaint_notifier(&self) -> bool {
        self.repaint_notifier.is_some()
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, RuntimeState::Running)
    }

    pub fn is_running_or_restarting(&self) -> bool {
        self.is_running() || self.pending_restart.is_some()
    }

    pub fn is_teardown_pending(&self) -> bool {
        self.teardown_rx.is_some()
    }

    pub(crate) fn active_local_only(&self) -> Option<bool> {
        self.active_runtime
            .as_ref()
            .map(|runtime| runtime.settings().local_only)
    }

    #[cfg(test)]
    pub(crate) fn set_running_for_tests(&mut self) {
        self.state = RuntimeState::Running;
    }

    #[cfg(test)]
    pub(crate) fn set_active_local_only_for_tests(&mut self, value: bool) {
        let mut runtime = RuntimeSettingsSnapshot::default();
        runtime
            .set("VOICEPI_LOCAL_ONLY", if value { "True" } else { "False" })
            .unwrap();
        self.active_runtime = Some(runtime);
        self.state = RuntimeState::Running;
    }

    /// Start the native runtime or return its failure directly.
    ///
    /// `VOICEPI_DICTATE_ENGINE=python` now produces a migration error. An
    /// unknown value is also rejected instead of silently selecting a
    /// different runtime.
    pub fn start(&mut self, command: WorkerCommand) -> Result<()> {
        self.poll();
        if self.teardown_rx.is_some() {
            return Err(anyhow!(
                "native runtime is still stopping; wait for teardown completion before starting"
            ));
        }
        if self.is_running() {
            return Err(anyhow!("runtime is already running"));
        }
        validate_engine_selection(std::env::var(ENGINE_ENV).ok().as_deref())?;

        self.state = RuntimeState::Starting;
        crate::diag::log!(
            "[runtime] native start requested; state=starting env_entries={} args={}",
            command.runtime.value_count(),
            command.args.len()
        );
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] start stage=apply-config features hotkeys={} injection={} audio={} local_whisper={}",
                cfg!(feature = "rust-hotkeys"),
                cfg!(feature = "rust-injection"),
                cfg!(feature = "audio-capture"),
                cfg!(feature = "whisper-rs-local")
            );
        }
        if crate::diag::trace_enabled() {
            crate::diag::log!(
                "[runtime/trace] start command metadata program_present={} working_dir_present={} env_names={:?}",
                !command.program.as_os_str().is_empty(),
                !command.working_dir.as_os_str().is_empty(),
                redacted_env_names(&command)
            );
        }

        match self.attempt_in_process_start(&command) {
            Ok(()) => {
                self.active_runtime = Some(command.runtime.clone());
                Ok(())
            }
            Err(err) => {
                self.state = RuntimeState::Stopped;
                self.active_runtime = None;
                let message = format!(
                    "native runtime start failed at {}: {err}",
                    install_error_stage(&err)
                );
                crate::diag::log!("[runtime] {message}");
                let _ = self.tx.send(RuntimeEvent::Error(message.clone()));
                if let Some(notifier) = self.repaint_notifier.as_ref() {
                    notifier();
                }
                Err(anyhow!(message))
            }
        }
    }

    pub(super) fn attempt_in_process_start(
        &mut self,
        command: &WorkerCommand,
    ) -> std::result::Result<(), InProcessInstallError> {
        if crate::diag::debug_enabled() {
            crate::diag::log!("[runtime/debug] start stage=apply-worker-config");
        }
        let ambient_live_env = crate::config::ambient_live_runtime_env();
        in_process::maybe_emit_env_precedence_note(&self.tx);

        if crate::diag::debug_enabled() {
            crate::diag::log!("[runtime/debug] start stage=validate-effective-options");
        }
        let effective_device = command.runtime.settings().device.as_str();
        let effective_stt_backend = command.runtime.settings().stt_backend.as_str();
        if crate::diag::trace_enabled() {
            crate::diag::log!(
                "[runtime/trace] effective options device={} stt_backend={}",
                ascii_escape(effective_device),
                ascii_escape(effective_stt_backend)
            );
        }
        super::dictate_run::validate_native_runtime_options(
            Some(effective_device),
            Some(effective_stt_backend),
            cfg!(feature = "whisper-rs-vulkan"),
        )
        .map_err(|err| InProcessInstallError::InvalidOptions(err.to_string()))?;

        if crate::diag::debug_enabled() {
            crate::diag::log!("[runtime/debug] start stage=build-backends-and-install-hotkey");
        }
        let installation = in_process::try_install(
            self.tx.clone(),
            self.repaint_notifier.clone(),
            command.runtime.clone(),
            ambient_live_env,
        )?;
        let installed_key_names = installation.key_names.clone();
        self.stash_in_process_installation(installation);

        let (driver, chord) = self.in_process_install_summary(&installed_key_names);
        self.state = RuntimeState::Running;
        let started_line = format!("native-rust (in-process; driver={driver}, chord={chord})");
        crate::diag::log!("[runtime] native runtime installed: {started_line}");
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] start stage=ready driver={driver} chord_components={}",
                installed_key_names.len()
            );
        }
        let _ = self.tx.send(RuntimeEvent::Started {
            command: started_line,
        });
        in_process::emit_ready_worker_event(&self.tx);
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
        Ok(())
    }

    fn in_process_install_summary(&self, installed_key_names: &[String]) -> (&'static str, String) {
        (
            self.in_process_driver_label(),
            format_installed_chord(installed_key_names),
        )
    }

    #[cfg(feature = "rust-hotkeys")]
    fn in_process_driver_label(&self) -> &'static str {
        self.hotkey_handle
            .as_ref()
            .map(|handle| handle.driver_name())
            .unwrap_or("none")
    }

    #[cfg(not(feature = "rust-hotkeys"))]
    fn in_process_driver_label(&self) -> &'static str {
        "none"
    }

    #[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
    fn stash_in_process_installation(&mut self, installation: in_process::InProcessInstallation) {
        self.runtime_active = Some(installation.runtime_active);
        self.capture_stop = Some(installation.capture_stop);
        self.hotkey_handle = Some(installation.hotkey_handle);
        self.coord_slot_keepalive = Some(installation.coord_slot_keepalive);
    }

    #[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
    fn stash_in_process_installation(&mut self, _installation: in_process::InProcessInstallation) {}
}

pub(super) fn validate_engine_selection(raw: Option<&str>) -> Result<()> {
    match raw.map(str::trim).unwrap_or("") {
        "" => Ok(()),
        value if value.eq_ignore_ascii_case("rust") => Ok(()),
        value if value.eq_ignore_ascii_case("python") => Err(anyhow!(
            "{ENGINE_ENV}=python is no longer supported; remove the variable and restart. \
             Dictation now runs in the native Rust runtime."
        )),
        other => Err(anyhow!(
            "unknown {ENGINE_ENV}=\"{}\"; remove the variable or set it to `rust`",
            ascii_escape(other)
        )),
    }
}

pub(super) fn ascii_escape(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

pub(super) fn install_error_stage(error: &InProcessInstallError) -> &'static str {
    match error {
        InProcessInstallError::FeaturesMissing => "feature-check",
        InProcessInstallError::ConfigLoadFailed(_) => "config-load",
        InProcessInstallError::InvalidOptions(_) => "runtime-options",
        InProcessInstallError::EmptyChord => "hotkey-config",
        InProcessInstallError::MissingBackend(_) => "backend-build",
        InProcessInstallError::HotkeyInstallFailed(_) => "hotkey-install",
        InProcessInstallError::PttAlreadyHeld(_) => "ptt-ownership",
        InProcessInstallError::Panicked(_) => "panic-boundary",
    }
}

pub(super) fn redacted_env_names(command: &WorkerCommand) -> Vec<String> {
    command.runtime.value_names()
}

pub(super) fn format_installed_chord(installed_key_names: &[String]) -> String {
    if installed_key_names.is_empty() {
        "?".to_owned()
    } else {
        installed_key_names.join("+")
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
