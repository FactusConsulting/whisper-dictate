//! Shared test scaffolding for [`super::EnigoInjectBackend`].
//!
//! Pulled out of `inject_tests.rs` so the two test groups
//! (`inject_tests` for delegation / error mapping, `inject_cleanup_tests`
//! for the Codex P1 + P2 pre-injection cleanup behaviour) can share the
//! `RecordingBackend` / `RecordingClipboard` fakes without either file
//! exceeding the repo's ~500-line modularity gate. The whole module is
//! `#[cfg(test)]` so it never compiles into a release binary.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};

use super::EnigoInjectBackend;
use crate::injection::enigo_backend::InjectorBackend;
use crate::injection::paste::Clipboard;
use crate::injection::{InjectMethod, Injector};

/// Recording fake — captures every call made by [`Injector`] so the
/// assertion can pin down what the trait impl actually delegates. Lives
/// behind `Arc<Mutex<…>>` so the test retains a handle after the value
/// is moved into the [`Injector`].
#[derive(Default, Clone)]
pub(super) struct RecordingBackend {
    pub(super) events: Arc<Mutex<Vec<String>>>,
    fail_next: Arc<Mutex<Option<String>>>,
}

impl RecordingBackend {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn failing(msg: &str) -> Self {
        let me = Self::default();
        *me.fail_next.lock().unwrap() = Some(msg.to_owned());
        me
    }

    fn take_fail(&self) -> Option<String> {
        self.fail_next.lock().unwrap().take()
    }
}

impl InjectorBackend for RecordingBackend {
    fn type_text(&mut self, text: &str) -> Result<()> {
        if let Some(msg) = self.take_fail() {
            return Err(anyhow!("recorded failure: {msg}"));
        }
        self.events.lock().unwrap().push(format!("type:{text}"));
        Ok(())
    }
    fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
        if let Some(msg) = self.take_fail() {
            return Err(anyhow!("recorded failure: {msg}"));
        }
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("chord:[{}]+{:#x}", mods.join(","), key));
        Ok(())
    }
    fn release_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
        // Override the trait's default no-op so the wrapper's
        // pre-injection sweep is observable. Record one event per VK
        // batch (with the order the wrapper passes them) so a
        // regression that skipped a modifier or reordered the sweep
        // shows up in the assertion as a different vector — not a
        // hash of the set.
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("release:[{}]", mods.join(",")));
        Ok(())
    }
}

/// Test-only [`Clipboard`] backend. Captures every read / write so the
/// paste-mode assertion can check the save / inject / restore cycle.
#[derive(Default, Clone)]
pub(super) struct RecordingClipboard {
    /// Current logical clipboard contents. Started as a real value so a
    /// "previous" save is non-trivial — tests verify the previous value
    /// is restored after the paste chord fires.
    contents: Arc<Mutex<Option<String>>>,
    writes: Arc<Mutex<Vec<String>>>,
    reads: Arc<Mutex<usize>>,
    fail_write: Arc<Mutex<bool>>,
}

impl RecordingClipboard {
    pub(super) fn with_initial(value: Option<&str>) -> Self {
        let me = Self::default();
        *me.contents.lock().unwrap() = value.map(str::to_owned);
        me
    }

    pub(super) fn snapshot_writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    pub(super) fn read_contents(&self) -> Option<String> {
        self.contents.lock().unwrap().clone()
    }

    pub(super) fn read_count(&self) -> usize {
        *self.reads.lock().unwrap()
    }

    pub(super) fn arm_write_failure(&self) {
        *self.fail_write.lock().unwrap() = true;
    }
}

impl Clipboard for RecordingClipboard {
    fn read(&mut self) -> Option<String> {
        *self.reads.lock().unwrap() += 1;
        self.contents.lock().unwrap().clone()
    }
    fn write(&mut self, value: &str) -> bool {
        if *self.fail_write.lock().unwrap() {
            return false;
        }
        self.writes.lock().unwrap().push(value.to_owned());
        *self.contents.lock().unwrap() = Some(value.to_owned());
        true
    }
}

/// Build a wrapper with the supplied recording backend installed via
/// `Injector::with_backend`. No clipboard is wired — paste-mode
/// callers should use [`backend_with_clipboard`] instead.
///
/// The clipboard-restore delay is forced to `Duration::ZERO` so paste
/// tests don't pay the 2 s production wall-clock wait — see
/// [`super::DEFAULT_CLIPBOARD_RESTORE_DELAY`] / Codex P1 #419
/// inject.rs:266 for the rationale.
pub(super) fn backend_with(method: InjectMethod, fake: RecordingBackend) -> EnigoInjectBackend {
    let injector = Injector::new().with_backend(Box::new(fake));
    EnigoInjectBackend::new(injector, method).with_restore_delay(Duration::ZERO)
}

/// Build a paste-capable wrapper: recording backend + recording
/// clipboard installed in one step. Restore delay forced to ZERO so the
/// paste-cleanup assertions don't pay the 2 s production wall-clock wait.
pub(super) fn backend_with_clipboard(
    method: InjectMethod,
    fake: RecordingBackend,
    clipboard: RecordingClipboard,
) -> EnigoInjectBackend {
    let injector = Injector::new().with_backend(Box::new(fake));
    EnigoInjectBackend::new(injector, method)
        .with_clipboard(Box::new(clipboard))
        .with_restore_delay(Duration::ZERO)
}
