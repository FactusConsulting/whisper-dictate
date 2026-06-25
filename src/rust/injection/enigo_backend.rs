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
pub fn make_default_backend() -> Result<Box<dyn InjectorBackend>> {
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

    pub fn make_default_backend() -> Result<Box<dyn InjectorBackend>> {
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
    }

    /// Map our platform-agnostic VK code constants to enigo's [`Key`] enum.
    /// We only need the handful of keys used by the paste shortcuts.
    fn vk_to_enigo(vk: u16) -> Result<Key> {
        use super::super::paste::vk as platform;
        Ok(match vk {
            platform::VK_CONTROL => Key::Control,
            platform::VK_SHIFT => Key::Shift,
            platform::VK_LWIN => Key::Meta,
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
}
