use super::{InjectMethod, Injector};
use crate::injection::paste::PasteShortcut;
use anyhow::Result;

#[test]
fn injector_builder_threads_through_state() {
    let injector = Injector::new()
        .with_target("Notepad", "notepad.exe")
        .with_xkb_layout("dk");
    assert_eq!(injector.target_title, "Notepad");
    assert_eq!(injector.target_process, "notepad.exe");
    assert_eq!(injector.xkb_layout, "dk");
}

// -- Trait-object backend wiring (P1 #2 from PR #351 review) --
//
// The dispatcher used to call `make_default_backend()` inline, so tests
// could not exercise `inject_text` end-to-end. `with_backend()` lets us
// plug in a recording fake on any platform — including Linux, where the
// injected backend now wins over the helper chain.

use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
struct RecordingBackend {
    events: Arc<Mutex<Vec<String>>>,
}

impl crate::injection::enigo_backend::InjectorBackend for RecordingBackend {
    fn type_text(&mut self, text: &str) -> Result<()> {
        self.events.lock().unwrap().push(format!("type:{text}"));
        Ok(())
    }
    fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("chord:[{}]+{:#x}", mods.join(","), key));
        Ok(())
    }
    fn release_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
        // Overridden so `Injector::release_held_modifiers` tests can
        // assert the modifier sweep actually reached the backend (the
        // trait's default would swallow it silently). Mirrors the
        // production enigo path that drops Ctrl / Shift / Alt / Cmd
        // before the burst lands — Codex P2 #417 inject.rs:110.
        let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("release:[{}]", mods.join(",")));
        Ok(())
    }
}

#[test]
fn inject_text_routes_typing_through_injected_backend() {
    let backend = RecordingBackend::default();
    let events = backend.events.clone();
    let mut injector = Injector::new().with_backend(Box::new(backend));
    injector.inject_text("hello", InjectMethod::Typing).unwrap();
    assert_eq!(*events.lock().unwrap(), vec!["type:hello".to_string()]);
}

#[test]
fn inject_text_routes_paste_through_injected_backend() {
    let backend = RecordingBackend::default();
    let events = backend.events.clone();
    let mut injector = Injector::new().with_backend(Box::new(backend));
    injector
        .inject_text("ignored", InjectMethod::Paste(Some(PasteShortcut::CtrlV)))
        .unwrap();
    let recorded = events.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "expected single chord, got {recorded:?}");
    assert!(
        recorded[0].starts_with("chord:["),
        "expected chord event, got {:?}",
        recorded[0]
    );
}

#[test]
fn inject_text_paste_with_none_uses_default_shortcut_on_backend_path() {
    // The trait-backend path (Windows/macOS) can't read the target
    // title/process so None collapses to PasteShortcut::default()
    // there. Verify a chord is still emitted rather than the call
    // panicking or no-op'ing — the Linux-specific terminal-aware
    // heuristic lives in `inject_on_linux` and is exercised separately.
    let backend = RecordingBackend::default();
    let events = backend.events.clone();
    let mut injector = Injector::new().with_backend(Box::new(backend));
    injector
        .inject_text("ignored", InjectMethod::Paste(None))
        .unwrap();
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        1,
        "expected single chord for Paste(None), got {recorded:?}"
    );
}

// -- release_held_modifiers wiring (Codex P2 #417 inject.rs:110) --
//
// The PTT-modifier sweep needs to reach the backend on Windows/macOS
// (always) and on Linux when a backend is explicitly installed; on
// Linux without a backend it must stay a quiet success because the
// wayland.rs / ydotool helper already runs its own release sweep
// before the paste chord. These tests pin all three branches.

#[test]
fn release_held_modifiers_forwards_to_injected_backend() {
    // Holds on Windows / macOS (always taken) AND on Linux (the
    // `if let Some(backend) = ...` early-return branch). Asserts the
    // exact VK codes propagate so a regression that drops one would
    // be caught.
    use crate::injection::paste::vk;
    let backend = RecordingBackend::default();
    let events = backend.events.clone();
    let mut injector = Injector::new().with_backend(Box::new(backend));
    injector
        .release_held_modifiers(&[vk::VK_CONTROL, vk::VK_SHIFT, vk::VK_MENU])
        .unwrap();
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![format!(
            "release:[{:#x},{:#x},{:#x}]",
            vk::VK_CONTROL,
            vk::VK_SHIFT,
            vk::VK_MENU,
        )]
    );
}

#[test]
fn release_held_modifiers_with_empty_list_is_ok() {
    // Empty input is the "no stale modifiers held" hot path; it must
    // still reach the backend so a future implementation that, say,
    // batches into a single SendInput call sees a well-formed empty
    // request rather than the dispatcher swallowing the call.
    let backend = RecordingBackend::default();
    let events = backend.events.clone();
    let mut injector = Injector::new().with_backend(Box::new(backend));
    injector.release_held_modifiers(&[]).unwrap();
    assert_eq!(*events.lock().unwrap(), vec!["release:[]".to_string()]);
}

#[cfg(target_os = "linux")]
#[test]
fn release_held_modifiers_without_backend_runs_helper_sweep_on_linux() {
    // Codex P2 #419 dispatcher.rs:184: with no trait-object backend
    // installed the call must run the helper-chain release sweep
    // (formerly a silent no-op that left wtype/kwtype/dotool paths
    // exposed to held PTT modifiers). The actual sweep shells out
    // to ydotool/xdotool when installed and silently succeeds when
    // neither is — both branches surface as `Ok(())` from the public
    // API, so we can only assert the success contract here. The
    // exact command and argument vector is pinned by the
    // `linux_helpers::plan_modifier_release` tests where the locator
    // is injectable without touching `$PATH`.
    use crate::injection::paste::vk;
    let mut injector = Injector::new();
    assert!(injector.release_held_modifiers(&[vk::VK_CONTROL]).is_ok());
    assert!(injector.release_held_modifiers(&[]).is_ok());
}

#[test]
fn cancelled_modifier_release_does_not_initialize_a_native_backend() {
    let mut injector = Injector::new();
    assert!(injector
        .release_held_modifiers_cancellable(&[0x11], &|| false)
        .unwrap_err()
        .to_string()
        .contains("injection cancelled"));
    assert!(injector.backend.is_none());
}
