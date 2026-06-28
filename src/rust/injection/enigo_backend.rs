//! Cross-platform `enigo`-backed injection (Windows + macOS + Linux/X11).
//!
//! The whole module is gated behind the `rust-injection` cargo feature so the
//! default build never pulls in `enigo`'s native deps (`xdo`/`xtest` on X11,
//! `CGEventTap` on macOS, `SendInput` on Windows). The dispatcher in
//! `mod.rs` performs runtime detection: even with the feature compiled in,
//! the path is only taken when `VOICEPI_INJECTION_BACKEND=rust` is set.
//!
//! Layout-independence: `enigo`'s `Key::Unicode` types the literal codepoint
//! regardless of the active layout, and for the paste shortcut we send the VK
//! codes from [`super::paste::vk`] so `Ctrl+V` works on AZERTY/Russian/Dvorak
//! without remapping. The `key_chord` helper is pulled into a free function
//! that tests can drive with a recording fake of [`InjectorBackend`] —
//! `enigo` itself is hard to construct in CI (no display server).

use anyhow::{anyhow, Result};

use super::paste::PasteShortcut;

/// Abstract injection backend. Exists so unit tests can drive
/// `paste_with_shortcut` / `type_text` end-to-end against a recording fake
/// without instantiating `enigo`.
pub trait InjectorBackend {
    fn type_text(&mut self, text: &str) -> Result<()>;
    /// Press and release a chord of platform-specific virtual key codes. The
    /// modifiers are pressed in order, then the main key is tapped, then the
    /// modifiers are released in reverse order.
    fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()>;
    /// Send a bare `Release` for each VK code, mirroring the user lifting
    /// the modifier key. The default impl is a no-op so existing recording
    /// fakes (and the trait's other consumers) don't have to override
    /// anything; the real `enigo` impl overrides this to actually drop
    /// held modifiers. Used by `EnigoInjectBackend::inject` to clear a
    /// stale push-to-talk chord (Ctrl / Shift / Alt / Cmd) before
    /// synthesising the burst — without this a held PTT modifier turns
    /// dictated characters into shortcuts, matching the Python
    /// `_release_stale_modifiers` sweep. Codex P2 #417 inject.rs:110.
    fn release_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
        let _ = modifiers;
        Ok(())
    }
}

/// Send the configured paste keystroke. Pure logic over the backend trait;
/// the actual `enigo::Enigo` impl lives behind `cfg(feature = "rust-injection")`.
///
/// `?Sized` lets the dispatcher pass a `&mut dyn InjectorBackend` straight
/// through without an extra level of generics — important now that
/// `Injector` accepts trait-object backends (P1 #2 from PR #351 review).
pub fn send_paste_shortcut<B: InjectorBackend + ?Sized>(
    backend: &mut B,
    shortcut: PasteShortcut,
) -> Result<()> {
    use super::paste::vk;

    let (modifiers, key): (Vec<u16>, u16) = match shortcut {
        PasteShortcut::CtrlV => (vec![vk::VK_CONTROL], vk::VK_V),
        PasteShortcut::CtrlShiftV => (vec![vk::VK_CONTROL, vk::VK_SHIFT], vk::VK_V),
        PasteShortcut::ShiftInsert => (vec![vk::VK_SHIFT], vk::VK_INSERT),
        PasteShortcut::CmdV => (vec![vk::VK_LWIN], vk::VK_V),
    };
    backend.key_chord(&modifiers, key)
}

#[cfg(not(feature = "rust-injection"))]
pub fn make_default_backend() -> Result<Box<dyn InjectorBackend + Send>> {
    Err(anyhow!(
        "rust-injection feature not compiled in (rebuild with --features rust-injection)"
    ))
}

#[cfg(feature = "rust-injection")]
pub use enigo_impl::make_default_backend;

#[cfg(feature = "rust-injection")]
mod enigo_impl {
    use super::*;
    use enigo::{
        Direction::{Click, Press, Release},
        Enigo, Key, Keyboard, Settings,
    };

    pub fn make_default_backend() -> Result<Box<dyn InjectorBackend + Send>> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow!("failed to initialise enigo: {e}"))?;
        Ok(Box::new(EnigoBackend { enigo }))
    }

    struct EnigoBackend {
        enigo: Enigo,
    }

    impl InjectorBackend for EnigoBackend {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.enigo
                .text(text)
                .map_err(|e| anyhow!("enigo type failed: {e}"))
        }

        fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
            for m in modifiers {
                let key = vk_to_enigo(*m)?;
                self.enigo
                    .key(key, Press)
                    .map_err(|e| anyhow!("enigo press {m:#x}: {e}"))?;
            }
            let main = vk_to_enigo(key)?;
            self.enigo
                .key(main, Click)
                .map_err(|e| anyhow!("enigo click {key:#x}: {e}"))?;
            for m in modifiers.iter().rev() {
                let key = vk_to_enigo(*m)?;
                self.enigo
                    .key(key, Release)
                    .map_err(|e| anyhow!("enigo release {m:#x}: {e}"))?;
            }
            Ok(())
        }

        fn release_modifiers(&mut self, modifiers: &[u16]) -> Result<()> {
            // Drop each held modifier individually. Sending a Release for
            // a key that wasn't down is a no-op on every supported
            // platform (Win32 `SendInput`, macOS `CGEventPost`, X11
            // `XTestFakeKeyEvent`), so we don't gate on whether enigo
            // believes the modifier is currently pressed. Unmapped VKs
            // (e.g. a future code we haven't taught `vk_to_enigo` yet)
            // and individual Release failures are silenced rather than
            // failing the whole sweep — losing a modifier release is
            // strictly less bad than aborting the burst.
            for m in modifiers {
                if let Ok(key) = vk_to_enigo(*m) {
                    let _ = self.enigo.key(key, Release);
                }
            }
            Ok(())
        }
    }

    /// Map our platform-agnostic VK code constants to enigo's [`Key`] enum.
    /// We only need the handful of keys used by the paste shortcuts and the
    /// side-specific variants used by the stale-modifier sweep.
    fn vk_to_enigo(vk: u16) -> Result<Key> {
        use super::super::paste::vk as platform;
        Ok(match vk {
            platform::VK_CONTROL => Key::Control,
            platform::VK_SHIFT => Key::Shift,
            // Alt — only used by `release_modifiers` today (no paste
            // chord uses Alt); enigo's `Key::Alt` maps to the
            // platform's left-Alt scancode on Win32 / X11 and to the
            // Option key on macOS, matching `_release_stale_modifiers`.
            platform::VK_MENU => Key::Alt,
            platform::VK_LWIN => Key::Meta,
            // Side-specific modifier variants. Codex P2 #419 inject.rs:84:
            // a PTT binding like `ctrl_r` leaves VK_RCONTROL logically
            // down; the generic VK_CONTROL release does NOT clear the
            // right-side scancode on Win32. enigo exposes `Key::RControl`
            // / `Key::RShift` cross-platform (their X11 / macOS impl maps
            // to the same OS-level "right" scancode where one exists,
            // and is a harmless no-op otherwise). `Key::RMenu` and
            // `Key::RWin` are Windows-only in enigo 0.6, so we only
            // include those mappings on Windows — on other platforms
            // those VKs fall through to the silent-skip path in
            // `release_modifiers`.
            platform::VK_RCONTROL => Key::RControl,
            platform::VK_RSHIFT => Key::RShift,
            #[cfg(target_os = "windows")]
            platform::VK_RMENU => Key::RMenu,
            #[cfg(target_os = "windows")]
            platform::VK_RWIN => Key::RWin,
            platform::VK_V => Key::Unicode('v'),
            platform::VK_INSERT => Key::Insert,
            other => return Err(anyhow!("unsupported VK code: {other:#x}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recording {
        events: Vec<String>,
    }

    impl InjectorBackend for Recording {
        fn type_text(&mut self, text: &str) -> Result<()> {
            self.events.push(format!("type:{text}"));
            Ok(())
        }
        fn key_chord(&mut self, modifiers: &[u16], key: u16) -> Result<()> {
            let mods: Vec<String> = modifiers.iter().map(|m| format!("{m:#x}")).collect();
            self.events
                .push(format!("chord:[{}]+{:#x}", mods.join(","), key));
            Ok(())
        }
    }

    #[test]
    fn ctrl_v_paste_emits_single_ctrl_plus_v_chord() {
        use super::super::paste::vk;
        let mut backend = Recording::default();
        send_paste_shortcut(&mut backend, PasteShortcut::CtrlV).unwrap();
        assert_eq!(
            backend.events,
            vec![format!("chord:[{:#x}]+{:#x}", vk::VK_CONTROL, vk::VK_V)]
        );
    }

    #[test]
    fn ctrl_shift_v_paste_emits_two_modifiers_then_v() {
        use super::super::paste::vk;
        let mut backend = Recording::default();
        send_paste_shortcut(&mut backend, PasteShortcut::CtrlShiftV).unwrap();
        assert_eq!(
            backend.events,
            vec![format!(
                "chord:[{:#x},{:#x}]+{:#x}",
                vk::VK_CONTROL,
                vk::VK_SHIFT,
                vk::VK_V
            )]
        );
    }

    #[test]
    fn shift_insert_paste_targets_insert_vk() {
        use super::super::paste::vk;
        let mut backend = Recording::default();
        send_paste_shortcut(&mut backend, PasteShortcut::ShiftInsert).unwrap();
        assert_eq!(
            backend.events,
            vec![format!("chord:[{:#x}]+{:#x}", vk::VK_SHIFT, vk::VK_INSERT)]
        );
    }

    #[test]
    fn cmd_v_paste_uses_lwin_vk() {
        use super::super::paste::vk;
        let mut backend = Recording::default();
        send_paste_shortcut(&mut backend, PasteShortcut::CmdV).unwrap();
        assert_eq!(
            backend.events,
            vec![format!("chord:[{:#x}]+{:#x}", vk::VK_LWIN, vk::VK_V)]
        );
    }

    #[test]
    fn type_text_passes_text_through_to_backend() {
        let mut backend = Recording::default();
        backend.type_text("Hello, world!").unwrap();
        assert_eq!(backend.events, vec!["type:Hello, world!"]);
    }

    #[cfg(not(feature = "rust-injection"))]
    #[test]
    fn make_default_backend_errors_without_feature() {
        assert!(make_default_backend().is_err());
    }

    #[test]
    fn default_release_modifiers_is_a_silent_noop() {
        // The trait-level default of `release_modifiers` exists so the
        // existing recording fakes (which only care about type_text /
        // key_chord) don't have to override anything. Verify it neither
        // records events nor errors when called with a non-empty modifier
        // list. The real enigo override is exercised on the dispatcher
        // path -- see `Injector::release_held_modifiers` tests in
        // `dispatcher.rs`. Coverage guard for PR #419 / Codex P2 #417.
        use super::super::paste::vk;
        let mut backend = Recording::default();
        backend
            .release_modifiers(&[vk::VK_CONTROL, vk::VK_SHIFT, vk::VK_MENU])
            .unwrap();
        assert!(
            backend.events.is_empty(),
            "default impl must not record any events, got {:?}",
            backend.events
        );
    }

    #[test]
    fn default_release_modifiers_accepts_empty_list() {
        // Empty input is the "no stale modifiers held" hot path and must
        // stay a quiet success.
        let mut backend = Recording::default();
        backend.release_modifiers(&[]).unwrap();
        assert!(backend.events.is_empty());
    }
}
