//! Layout-independent paste shortcuts.
//!
//! Phase 2.1 adds a configurable paste keystroke that survives non-US
//! keyboard layouts (Russian, AZERTY, Dvorak …). The trick is to send the
//! shortcut via the platform's *virtual* key codes, not the printable
//! character — `V` and `Insert` have stable VK codes that the OS resolves to
//! the right scancode for the active layout. The values live here as plain
//! constants so unit tests can assert them and `enigo_backend` can read them
//! without depending on the platform crates.
//!
//! Clipboard save/restore is also coordinated here: the typed text is
//! deposited briefly, the paste shortcut fires, then the previous clipboard
//! contents are restored after a short delay (paste targets — especially on
//! Wayland where `wl-copy` serves content lazily — may read the clipboard
//! after we triggered the keystroke; restoring instantly would race).
//!
//! `clipboard_io` is a trait so tests can verify save/restore semantics
//! without pulling in `arboard` (which needs a display server even to
//! initialise on Linux).

use serde::{Deserialize, Serialize};

/// Configurable paste keystroke. The default is `Ctrl+V` everywhere except
/// macOS, where it becomes `Cmd+V`. Terminals frequently swallow `Ctrl+V` (it
/// inserts a literal `^V`) — use `CtrlShiftV` for Linux GTK/Electron text
/// widgets and terminals, `ShiftInsert` as the legacy/X11 escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteShortcut {
    CtrlV,
    CtrlShiftV,
    ShiftInsert,
    CmdV,
}

impl Default for PasteShortcut {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            PasteShortcut::CmdV
        } else {
            PasteShortcut::CtrlV
        }
    }
}

impl PasteShortcut {
    /// Parse a user-supplied shortcut spec (`ctrl_v`, `ctrl+v`, `shift_insert`,
    /// `cmd+v`). Case-insensitive; `+`, `_`, `-`, and spaces are all valid
    /// separators. Returns the parsed value or `None` for an unknown string.
    pub fn parse(spec: &str) -> Option<Self> {
        let normalised: String = spec
            .chars()
            .filter(|c| !matches!(c, '+' | '_' | '-' | ' '))
            .flat_map(char::to_lowercase)
            .collect();
        match normalised.as_str() {
            "ctrlv" => Some(PasteShortcut::CtrlV),
            "ctrlshiftv" => Some(PasteShortcut::CtrlShiftV),
            "shiftinsert" | "shiftins" => Some(PasteShortcut::ShiftInsert),
            "cmdv" | "metav" | "superv" | "winv" => Some(PasteShortcut::CmdV),
            _ => None,
        }
    }

    /// Pick the appropriate shortcut for a Linux target. Mirrors the existing
    /// Wayland `target_prefers_terminal_paste` heuristic so this module stays
    /// the single source of truth.
    pub fn for_linux_target(prefers_terminal: bool) -> Self {
        if prefers_terminal {
            PasteShortcut::CtrlShiftV
        } else {
            PasteShortcut::CtrlV
        }
    }
}

/// Windows `VK_*` codes used by the enigo backend. Defined here so they can be
/// unit-tested on every platform.
pub mod vk {
    pub const VK_CONTROL: u16 = 0x11;
    pub const VK_SHIFT: u16 = 0x10;
    /// `VK_MENU` is the Win32 name for the Alt key (both sides). Exposed so
    /// the stale-modifier sweep in `EnigoInjectBackend::inject` can drop
    /// an Alt held from a push-to-talk chord before the burst lands —
    /// matching `vp_inject.py::_release_stale_modifiers`'s full
    /// Shift / Alt / Ctrl / Cmd set.
    pub const VK_MENU: u16 = 0x12;
    pub const VK_LWIN: u16 = 0x5B;
    pub const VK_V: u16 = 0x56;
    pub const VK_INSERT: u16 = 0x2D;
}

/// Linux evdev keycodes. These match the ones already used by the Wayland
/// path and the enigo backend reuses them for `dotool`/`ydotool` fallbacks.
pub mod evdev {
    pub const KEY_LEFTCTRL: u16 = 29;
    pub const KEY_LEFTSHIFT: u16 = 42;
    /// `KEY_LEFTALT` pairs with `vk::VK_MENU` for the stale-modifier
    /// release sweep on the enigo path (see `EnigoInjectBackend::inject`).
    pub const KEY_LEFTALT: u16 = 56;
    pub const KEY_V: u16 = 47;
    pub const KEY_INSERT: u16 = 110;
    pub const KEY_LEFTMETA: u16 = 125;
}

/// Trait over the system clipboard so paste logic can be tested without
/// arboard / wl-copy / X selections. Implementations live in
/// [`super::clipboard`].
pub trait Clipboard {
    fn read(&mut self) -> Option<String>;
    fn write(&mut self, value: &str) -> bool;
}

/// Saves the current clipboard, copies `text`, and returns a guard that
/// restores the previous contents when [`PasteGuard::restore`] is called —
/// **and only if** the clipboard still holds `text` (so a user's mid-paste
/// copy is never clobbered).
///
/// Pure-logic counterpart to the Python `_restore_clipboard_after_delay`
/// background thread. The caller (typically `dispatcher.rs`) is responsible
/// for the wait between the paste keystroke and `restore()`, mirroring the
/// 2 s delay used by the Python path.
pub struct PasteGuard {
    previous: Option<String>,
    injected: String,
}

impl PasteGuard {
    /// Stash the current clipboard, copy `text`. Returns `None` if `text`
    /// could not be written (no clipboard backend, transient failure) — in
    /// which case the caller should not attempt to send the paste shortcut.
    pub fn copy_with_backup<C: Clipboard + ?Sized>(clip: &mut C, text: &str) -> Option<Self> {
        let previous = clip.read();
        if !clip.write(text) {
            return None;
        }
        Some(PasteGuard {
            previous,
            injected: text.to_owned(),
        })
    }

    /// Restore the saved clipboard, but only if it still holds the text we
    /// injected. Returns `true` when a restore actually happened.
    pub fn restore<C: Clipboard + ?Sized>(self, clip: &mut C) -> bool {
        let Some(previous) = self.previous else {
            return false;
        };
        match clip.read() {
            Some(current) if current == self.injected => clip.write(&previous),
            _ => false,
        }
    }

    pub fn injected(&self) -> &str {
        &self.injected
    }

    pub fn previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClipboard {
        contents: Option<String>,
        reads: usize,
        writes: Vec<String>,
        fail_write: bool,
    }

    impl FakeClipboard {
        fn new(initial: Option<&str>) -> Self {
            FakeClipboard {
                contents: initial.map(str::to_owned),
                reads: 0,
                writes: Vec::new(),
                fail_write: false,
            }
        }
    }

    impl Clipboard for FakeClipboard {
        fn read(&mut self) -> Option<String> {
            self.reads += 1;
            self.contents.clone()
        }
        fn write(&mut self, value: &str) -> bool {
            if self.fail_write {
                return false;
            }
            self.writes.push(value.to_owned());
            self.contents = Some(value.to_owned());
            true
        }
    }

    #[test]
    fn parses_common_shortcut_spellings() {
        assert_eq!(PasteShortcut::parse("ctrl+v"), Some(PasteShortcut::CtrlV));
        assert_eq!(PasteShortcut::parse("Ctrl_V"), Some(PasteShortcut::CtrlV));
        assert_eq!(
            PasteShortcut::parse("ctrl-shift-v"),
            Some(PasteShortcut::CtrlShiftV)
        );
        assert_eq!(
            PasteShortcut::parse("Shift+Insert"),
            Some(PasteShortcut::ShiftInsert)
        );
        assert_eq!(
            PasteShortcut::parse("shift ins"),
            Some(PasteShortcut::ShiftInsert)
        );
        assert_eq!(PasteShortcut::parse("CMD+V"), Some(PasteShortcut::CmdV));
        assert!(PasteShortcut::parse("ctrl+y").is_none());
    }

    #[test]
    fn linux_target_picks_terminal_safe_shortcut() {
        assert_eq!(
            PasteShortcut::for_linux_target(true),
            PasteShortcut::CtrlShiftV
        );
        assert_eq!(PasteShortcut::for_linux_target(false), PasteShortcut::CtrlV);
    }

    #[test]
    fn vk_constants_match_win32_documented_values() {
        // Spot-check against the well-known Win32 VK codes; if these ever
        // change Windows itself broke compatibility.
        assert_eq!(vk::VK_CONTROL, 0x11);
        assert_eq!(vk::VK_SHIFT, 0x10);
        assert_eq!(vk::VK_V, 0x56);
        assert_eq!(vk::VK_INSERT, 0x2D);
    }

    #[test]
    fn paste_guard_restores_previous_when_unchanged() {
        let mut clip = FakeClipboard::new(Some("prior"));
        let guard = PasteGuard::copy_with_backup(&mut clip, "injected").unwrap();
        assert_eq!(guard.previous(), Some("prior"));
        assert_eq!(guard.injected(), "injected");
        assert_eq!(clip.contents.as_deref(), Some("injected"));

        // Restore happens because the clipboard still holds the injected text.
        assert!(guard.restore(&mut clip));
        assert_eq!(clip.contents.as_deref(), Some("prior"));
    }

    #[test]
    fn paste_guard_does_not_clobber_user_copy() {
        let mut clip = FakeClipboard::new(Some("prior"));
        let guard = PasteGuard::copy_with_backup(&mut clip, "injected").unwrap();

        // User copies something else in the meantime.
        clip.write("user copy");

        assert!(!guard.restore(&mut clip));
        assert_eq!(clip.contents.as_deref(), Some("user copy"));
    }

    #[test]
    fn paste_guard_skips_restore_when_no_previous_value() {
        let mut clip = FakeClipboard::new(None);
        let guard = PasteGuard::copy_with_backup(&mut clip, "injected").unwrap();
        assert!(!guard.restore(&mut clip));
    }

    #[test]
    fn paste_guard_returns_none_when_write_fails() {
        let mut clip = FakeClipboard::new(Some("prior"));
        clip.fail_write = true;
        assert!(PasteGuard::copy_with_backup(&mut clip, "injected").is_none());
        // Original contents intact.
        assert_eq!(clip.contents.as_deref(), Some("prior"));
    }

    #[test]
    fn default_paste_shortcut_is_platform_appropriate() {
        let default = PasteShortcut::default();
        if cfg!(target_os = "macos") {
            assert_eq!(default, PasteShortcut::CmdV);
        } else {
            assert_eq!(default, PasteShortcut::CtrlV);
        }
    }
}
