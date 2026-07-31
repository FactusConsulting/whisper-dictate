//! Native supervisor lifecycle controls.

use anyhow::Result;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::supervisor::{RuntimeEvent, RuntimeState, RuntimeSupervisor};
use super::worker_command::WorkerCommand;

impl RuntimeSupervisor {
    pub fn stop(&mut self) -> Result<()> {
        self.pending_restart = None;
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] stop requested state={} hotkey_installed={}",
                self.state.label(),
                self.hotkey_handle.is_some()
            );
        }
        if let Some(active) = self.runtime_active.as_ref() {
            active.store(false, Ordering::Release);
            if crate::diag::trace_enabled() {
                crate::diag::log!(
                    "[runtime/trace] stop stage=lifecycle-gate active=false before hotkey suspension"
                );
            }
        }
        if self.hotkey_handle.is_some() {
            crate::diag::log!(
                "[runtime/debug] stop stage=detach-native-session closing hotkey, audio, STT, and injection backends asynchronously"
            );
        }
        self.stop_capture("stop");
        let was_running = self.state != RuntimeState::Stopped;
        if was_running {
            self.emit_exit_after_teardown = true;
        }
        let teardown_pending = self.begin_async_teardown();
        self.runtime_active = None;
        self.state = RuntimeState::Stopped;
        if was_running {
            if teardown_pending {
                crate::diag::log!(
                    "[runtime] native runtime input and injection stopped; resource teardown is running"
                );
            } else {
                crate::diag::log!("[runtime] native runtime stopped");
                let _ = self.tx.send(RuntimeEvent::Exited { code: Some(0) });
                self.emit_exit_after_teardown = false;
            }
        }
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
        Ok(())
    }

    pub fn restart(&mut self, command: WorkerCommand) -> Result<()> {
        crate::diag::log!("[runtime] native runtime restart requested");
        if let Err(err) = super::supervisor::validate_engine_selection(
            std::env::var(super::in_process::ENGINE_ENV).ok().as_deref(),
        ) {
            self.stop()?;
            return Err(err);
        }
        self.pending_restart = None;
        self.emit_exit_after_teardown = false;
        if let Some(active) = self.runtime_active.as_ref() {
            active.store(false, Ordering::Release);
        }
        self.stop_capture("restart");
        self.runtime_active = None;
        if self.begin_async_teardown() {
            self.pending_restart = Some(command);
            self.state = RuntimeState::Starting;
            crate::diag::log!(
                "[runtime/debug] restart stage=wait-for-teardown; replacement start queued"
            );
            if let Some(notifier) = self.repaint_notifier.as_ref() {
                notifier();
            }
            Ok(())
        } else {
            self.start(command)
        }
    }

    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
        self.finish_async_teardown();
        let listener_dead = self
            .hotkey_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_listener_alive());
        if listener_dead {
            let message =
                "native hotkey listener exited; stopping runtime because push-to-talk is unavailable";
            crate::diag::log!("[runtime] {message}");
            let _ = self.tx.send(RuntimeEvent::Error(message.to_owned()));
            let _ = self.tx.send(RuntimeEvent::Exited { code: Some(1) });
        }
        let events: Vec<_> = self.rx.try_iter().collect();
        if self.state != RuntimeState::Stopped
            && events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::Exited { .. }))
        {
            crate::diag::log!(
                "[runtime] native runtime reported terminal exit; tearing down session resources"
            );
            if let Some(active) = self.runtime_active.as_ref() {
                active.store(false, Ordering::Release);
            }
            self.stop_capture("terminal-exit");
            self.emit_exit_after_teardown = false;
            self.begin_async_teardown();
            self.runtime_active = None;
            self.state = RuntimeState::Stopped;
            if let Some(notifier) = self.repaint_notifier.as_ref() {
                notifier();
            }
        }
        events
    }

    fn stop_capture(&mut self, reason: &str) {
        let Some(stop) = self.capture_stop.take() else {
            return;
        };
        if crate::diag::debug_enabled() {
            crate::diag::log!(
                "[runtime/debug] lifecycle stage=close-audio reason={reason} before-state-change"
            );
        }
        stop();
    }

    fn begin_async_teardown(&mut self) -> bool {
        if self.teardown_rx.is_some() {
            return true;
        }
        let Some(mut handle) = self.hotkey_handle.take() else {
            self.coord_slot_keepalive = None;
            return false;
        };
        handle.begin_shutdown();
        let coord_slot = self.coord_slot_keepalive.take();
        self.teardown_rx = Some(spawn_teardown_task(move || {
            if crate::diag::trace_enabled() {
                crate::diag::log!(
                    "[runtime/trace] teardown thread stage=drop-hotkey-and-session begin"
                );
            }
            // A lexical scope invokes the production handle's Drop while also
            // compiling cleanly for reduced builds where HotkeyHandle is a
            // zero-resource stub without a Drop implementation.
            {
                let _handle_to_close = handle;
            }
            let _coord_slot_to_release = coord_slot;
            if crate::diag::trace_enabled() {
                crate::diag::log!(
                    "[runtime/trace] teardown thread stage=drop-hotkey-and-session complete"
                );
            }
        }));
        true
    }

    fn finish_async_teardown(&mut self) {
        let result = self.teardown_rx.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(())) | Some(Err(TryRecvError::Disconnected)) => {
                self.teardown_rx = None;
                crate::diag::log!("[runtime] native runtime resource teardown complete");
                if let Some(command) = self.pending_restart.take() {
                    self.state = RuntimeState::Stopped;
                    crate::diag::log!(
                        "[runtime/debug] restart stage=teardown-complete starting replacement"
                    );
                    if let Err(err) = self.start(command) {
                        crate::diag::log!("[runtime] queued restart failed: {err}");
                    }
                } else if self.emit_exit_after_teardown {
                    let _ = self.tx.send(RuntimeEvent::Exited { code: Some(0) });
                }
                self.emit_exit_after_teardown = false;
                if let Some(notifier) = self.repaint_notifier.as_ref() {
                    notifier();
                }
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }
}

pub(super) fn spawn_teardown_task<F>(teardown: F) -> Receiver<()>
where
    F: FnOnce() + Send + 'static,
{
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("runtime-teardown".to_owned())
        .spawn(move || {
            teardown();
            let _ = done_tx.send(());
        })
        .expect("spawn native runtime teardown thread");
    done_rx
}
