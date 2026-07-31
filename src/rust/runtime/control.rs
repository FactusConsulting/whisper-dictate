//! Native supervisor lifecycle controls.

use anyhow::Result;
use std::sync::atomic::Ordering;

use super::supervisor::{RuntimeEvent, RuntimeState, RuntimeSupervisor};
use super::worker_command::WorkerCommand;

impl RuntimeSupervisor {
    pub fn stop(&mut self) -> Result<()> {
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
                "[runtime/debug] stop stage=drop-native-session closing hotkey, audio, STT, and injection backends"
            );
        }
        self.hotkey_handle.take();
        self.runtime_active = None;
        self.coord_slot_keepalive = None;
        let was_running = self.state != RuntimeState::Stopped;
        self.state = RuntimeState::Stopped;
        if was_running {
            crate::diag::log!("[runtime] native runtime stopped");
            let _ = self.tx.send(RuntimeEvent::Exited { code: Some(0) });
        }
        if let Some(notifier) = self.repaint_notifier.as_ref() {
            notifier();
        }
        Ok(())
    }

    pub fn restart(&mut self, command: WorkerCommand) -> Result<()> {
        crate::diag::log!("[runtime] native runtime restart requested");
        self.stop()?;
        self.start(command)
    }

    pub fn poll(&mut self) -> Vec<RuntimeEvent> {
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
            self.hotkey_handle.take();
            self.runtime_active = None;
            self.coord_slot_keepalive = None;
            self.state = RuntimeState::Stopped;
            if let Some(notifier) = self.repaint_notifier.as_ref() {
                notifier();
            }
        }
        events
    }
}
