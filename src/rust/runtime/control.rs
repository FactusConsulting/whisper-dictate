//! Native supervisor lifecycle controls.

use anyhow::Result;

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
        if let Some(handle) = self.hotkey_handle.as_ref() {
            handle.suspend();
        }
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
        self.rx.try_iter().collect()
    }
}
