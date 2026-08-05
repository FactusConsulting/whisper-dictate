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
            let chord = format_capture_chord(seen);
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
        Key::ShiftLeft | Key::ShiftRight => "shift",
        Key::ControlLeft | Key::ControlRight => "ctrl",
        Key::AltLeft | Key::AltRight => "alt",
        Key::SuperLeft | Key::SuperRight => "cmd",
        Key::Escape => "esc",
        Key::Tab => "tab",
        Key::Backspace => "backspace",
        Key::Enter => "enter",
        Key::Space => "space",
        Key::Insert => "insert",
        Key::Delete => "delete",
        Key::Home => "home",
        Key::End => "end",
        Key::PageUp => "page_up",
        Key::PageDown => "page_down",
        Key::ArrowUp => "up",
        Key::ArrowDown => "down",
        Key::ArrowLeft => "left",
        Key::ArrowRight => "right",
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
        Key::F13 => "f13",
        Key::F14 => "f14",
        Key::F15 => "f15",
        Key::F16 => "f16",
        Key::F17 => "f17",
        Key::F18 => "f18",
        Key::F19 => "f19",
        Key::F20 => "f20",
        Key::F21 => "f21",
        Key::F22 => "f22",
        Key::F23 => "f23",
        Key::F24 => "f24",
        _ => return None,
    })
}

fn canonical_capture_key(key: &str) -> String {
    crate::hotkey::modifier_match::modifier_family(key)
        .map(str::to_owned)
        .unwrap_or_else(|| key.to_owned())
}

fn format_capture_chord(keys: &BTreeSet<String>) -> String {
    let mut ordered: Vec<&str> = keys.iter().map(String::as_str).collect();
    ordered.sort_by_key(|key| {
        crate::hotkey::modifier_match::modifier_family(key)
            .map(|family| match family {
                "ctrl" => 0,
                "shift" => 1,
                "alt" => 2,
                "cmd" => 3,
                _ => 4,
            })
            .unwrap_or(10)
    });
    ordered.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_collapses_modifier_sides_and_waits_for_release() {
        let mut capture = HotkeyCaptureState::default();
        capture.start();
        assert_eq!(capture.observe("ctrl_l", true), None);
        assert_eq!(capture.observe("f9", true), None);
        assert_eq!(capture.observe("f9", false), None);
        assert_eq!(capture.observe("ctrl_r", false), Some("ctrl+f9".to_owned()));
        assert_eq!(capture, HotkeyCaptureState::Pending("ctrl+f9".to_owned()));
    }

    #[test]
    fn capture_ignores_duplicate_keydown_and_orders_modifiers() {
        let mut capture = HotkeyCaptureState::default();
        capture.start();
        capture.observe("shift_r", true);
        capture.observe("ctrl_l", true);
        capture.observe("f12", true);
        capture.observe("shift_l", true);
        capture.observe("f12", false);
        assert_eq!(capture.observe("ctrl_r", false), None);
        assert_eq!(
            capture.observe("shift", false),
            Some("ctrl+shift+f12".to_owned())
        );
    }

    #[test]
    fn egui_capture_maps_supported_special_and_function_keys() {
        assert_eq!(
            capture_token_for_egui_key(egui::Key::ControlLeft),
            Some("ctrl")
        );
        assert_eq!(capture_token_for_egui_key(egui::Key::F9), Some("f9"));
        assert_eq!(capture_token_for_egui_key(egui::Key::A), None);
    }
}
