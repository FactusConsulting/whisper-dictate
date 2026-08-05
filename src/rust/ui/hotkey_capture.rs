//! State and key mapping for the Speech settings shortcut capture control.

use std::collections::BTreeSet;

use eframe::egui;

/// State for the settings-page shortcut capture control.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::ui) enum HotkeyCaptureState {
    #[default]
    Idle,
    Listening {
        held: BTreeSet<String>,
        seen: BTreeSet<String>,
    },
    Pending(String),
}

impl HotkeyCaptureState {
    pub(in crate::ui) fn start(&mut self) {
        *self = Self::Listening {
            held: BTreeSet::new(),
            seen: BTreeSet::new(),
        };
    }

    pub(in crate::ui) fn cancel(&mut self) {
        *self = Self::Idle;
    }

    pub(in crate::ui) fn reject(&mut self) {
        *self = Self::Idle;
    }

    pub(in crate::ui) fn is_listening(&self) -> bool {
        matches!(self, Self::Listening { .. })
    }

    /// Return a complete chord only after its final key is released.
    pub(in crate::ui) fn observe(&mut self, key: &str, pressed: bool) -> Option<String> {
        let Self::Listening { held, seen } = self else {
            return None;
        };
        let key = canonical_capture_key(key);
        if pressed {
            seen.insert(key.clone());
            held.insert(key);
            return None;
        }
        held.remove(&key);
        if held.is_empty() && !seen.is_empty() {
            let chord = crate::hotkey::capture::format_captured_chord(seen);
            *self = Self::Pending(chord.clone());
            return Some(chord);
        }
        None
    }

    pub(in crate::ui) fn apply_pending(&mut self) -> Option<String> {
        let Self::Pending(chord) = self else {
            return None;
        };
        let chord = chord.clone();
        *self = Self::Idle;
        Some(chord)
    }
}

/// Map the keys egui exposes to names accepted by the hotkey setting format.
/// Other keys remain available through the text field or diagnostic listener.
pub(in crate::ui) fn capture_token_for_egui_key(key: egui::Key) -> Option<&'static str> {
    use egui::Key;
    Some(match key {
        Key::ShiftLeft => "shift_l",
        Key::ShiftRight => "shift_r",
        Key::ControlLeft => "ctrl_l",
        Key::ControlRight => "ctrl_r",
        Key::AltLeft => "alt_l",
        Key::AltRight => "alt_r",
        Key::SuperLeft => "cmd_l",
        Key::SuperRight => "cmd_r",
        Key::Escape => "esc",
        Key::Tab => "tab",
        Key::Enter => "enter",
        Key::Space => "space",
        Key::F1 => "f1",
        Key::F2 => "f2",
        Key::F3 => "f3",
        Key::F4 => "f4",
        Key::F5 => "f5",
        Key::F6 => "f6",
        Key::F7 => "f7",
        Key::F8 => "f8",
        Key::F9 => "f9",
        Key::F10 => "f10",
        Key::F11 => "f11",
        Key::F12 => "f12",
        _ => return None,
    })
}

fn canonical_capture_key(key: &str) -> String {
    crate::hotkey::modifier_match::canonical_side(&key.trim().to_ascii_lowercase()).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_preserves_modifier_side_and_waits_for_release() {
        let mut capture = HotkeyCaptureState::default();
        capture.start();
        assert_eq!(capture.observe("ctrl_l", true), None);
        assert_eq!(capture.observe("f9", true), None);
        assert_eq!(capture.observe("f9", false), None);
        assert_eq!(
            capture.observe("ctrl_l", false),
            Some("ctrl_l+f9".to_owned())
        );
        assert_eq!(capture, HotkeyCaptureState::Pending("ctrl_l+f9".to_owned()));
    }

    #[test]
    fn capture_ignores_duplicate_keydown_and_orders_modifiers() {
        let mut capture = HotkeyCaptureState::default();
        capture.start();
        capture.observe("shift_r", true);
        capture.observe("ctrl_l", true);
        capture.observe("f12", true);
        capture.observe("shift_r", true);
        capture.observe("f12", false);
        assert_eq!(capture.observe("ctrl_l", false), None);
        assert_eq!(
            capture.observe("shift_r", false),
            Some("ctrl_l+shift_r+f12".to_owned())
        );
    }

    #[test]
    fn capture_handles_idle_state_and_common_modifier_aliases() {
        let mut capture = HotkeyCaptureState::default();
        assert!(!capture.is_listening());
        assert_eq!(capture.observe("ctrl_l", true), None);
        assert_eq!(capture.apply_pending(), None);
        capture.cancel();
        capture.reject();

        capture.start();
        for key in ["alt_gr", "cmd_r", "shift_l", "ctrl_r", "f1"] {
            capture.observe(key, true);
        }
        for key in ["f1", "ctrl_r", "shift_l", "cmd_r", "alt_gr"] {
            capture.observe(key, false);
        }
        assert_eq!(
            capture,
            HotkeyCaptureState::Pending("ctrl_r+shift_l+alt_r+cmd_r+f1".to_owned())
        );
        assert_eq!(
            capture.apply_pending(),
            Some("ctrl_r+shift_l+alt_r+cmd_r+f1".to_owned())
        );
        assert!(!capture.is_listening());
    }

    #[test]
    fn egui_capture_maps_supported_special_and_function_keys() {
        assert_eq!(
            capture_token_for_egui_key(egui::Key::ControlLeft),
            Some("ctrl_l")
        );
        assert_eq!(capture_token_for_egui_key(egui::Key::F9), Some("f9"));
        assert_eq!(capture_token_for_egui_key(egui::Key::A), None);
    }

    #[test]
    fn egui_capture_preserves_modifier_side() {
        assert_eq!(
            capture_token_for_egui_key(egui::Key::ControlRight),
            Some("ctrl_r")
        );
        assert_eq!(capture_token_for_egui_key(egui::Key::Backspace), None);
        assert_eq!(capture_token_for_egui_key(egui::Key::F13), None);
    }

    #[test]
    fn egui_capture_maps_each_supported_modifier_and_trigger() {
        for (key, expected) in [
            (egui::Key::ShiftLeft, "shift_l"),
            (egui::Key::ShiftRight, "shift_r"),
            (egui::Key::ControlRight, "ctrl_r"),
            (egui::Key::AltLeft, "alt_l"),
            (egui::Key::AltRight, "alt_r"),
            (egui::Key::SuperLeft, "cmd_l"),
            (egui::Key::SuperRight, "cmd_r"),
            (egui::Key::Escape, "esc"),
            (egui::Key::Tab, "tab"),
            (egui::Key::Enter, "enter"),
            (egui::Key::Space, "space"),
            (egui::Key::F1, "f1"),
            (egui::Key::F12, "f12"),
        ] {
            assert_eq!(capture_token_for_egui_key(key), Some(expected));
        }
    }
}
