//! Linux tray icon: a freedesktop StatusNotifierItem served over the session
//! D-Bus by `ksni`, which Waybar, KDE Plasma and GNOME's AppIndicator extension
//! all show. Pure Rust over zbus, so the Linux build gains no GTK dependency.
//!
//! The item runs on ksni's own thread. The app pushes state changes through the
//! handle; a click or the menu's "Open" entry sets a flag the app polls each
//! frame. The service waits for a StatusNotifierWatcher that is not up yet and
//! registers again whenever one appears, so a bar that starts after the app, or
//! restarts, still shows the icon.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ksni::blocking::TrayMethods;

use super::{tray_icon_rgba, tray_tooltip, TrayInteraction, TrayState};

/// Pixmap sizes offered to the host; it picks the one closest to its own size.
const ICON_SIZES: [u32; 3] = [22, 32, 64];

/// Convert the shared RGBA buffer to the ARGB32 (network byte order) layout the
/// StatusNotifierItem `IconPixmap` property carries.
pub(in crate::ui) fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect()
}

fn pixmaps(state: TrayState) -> Vec<ksni::Icon> {
    ICON_SIZES
        .iter()
        .map(|&size| ksni::Icon {
            width: size as i32,
            height: size as i32,
            data: rgba_to_argb(&tray_icon_rgba(state, size)),
        })
        .collect()
}

struct StatusItem {
    state: TrayState,
    tooltip: &'static str,
    activate: Arc<AtomicBool>,
}

impl ksni::Tray for StatusItem {
    fn id(&self) -> String {
        "whisper-dictate".into()
    }

    fn title(&self) -> String {
        "Whisper Dictate".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        pixmaps(self.state)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.activate.store(true, Ordering::Release);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![ksni::menu::StandardItem {
            label: "Open Whisper Dictate".into(),
            activate: Box::new(|this: &mut Self| this.activate.store(true, Ordering::Release)),
            ..Default::default()
        }
        .into()]
    }
}

/// Owns the StatusNotifierItem service and the last state pushed to it, so the
/// D-Bus properties change only when the mapped state does.
pub(in crate::ui) struct TrayManager {
    handle: Option<ksni::blocking::Handle<StatusItem>>,
    current: Option<TrayState>,
    activate: Arc<AtomicBool>,
    /// Set after a failed start, so the failure is logged once and never retried.
    init_failed: bool,
}

impl TrayManager {
    pub(in crate::ui) fn new() -> Self {
        Self {
            handle: None,
            current: None,
            activate: Arc::new(AtomicBool::new(false)),
            init_failed: false,
        }
    }

    /// Start the item on the first call and mirror `state` afterwards. Returns
    /// `Err` with a one-line reason when the session bus is unusable; the caller
    /// logs it once and disables the tray.
    pub(in crate::ui) fn sync(
        &mut self,
        state: TrayState,
        raw_language: &str,
    ) -> Result<(), String> {
        if self.init_failed || self.current == Some(state) {
            return Ok(());
        }
        let tooltip = tray_tooltip(state, raw_language);
        match &self.handle {
            None => {
                let item = StatusItem {
                    state,
                    tooltip,
                    activate: Arc::clone(&self.activate),
                };
                let handle = item
                    .assume_sni_available(true)
                    .spawn()
                    .map_err(|e| e.to_string())?;
                self.handle = Some(handle);
            }
            Some(handle) => {
                handle.update(|item| {
                    item.state = state;
                    item.tooltip = tooltip;
                });
            }
        }
        self.current = Some(state);
        Ok(())
    }

    pub(in crate::ui) fn disable(&mut self) {
        self.init_failed = true;
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
    }

    pub(in crate::ui) fn poll_interaction(&self) -> TrayInteraction {
        TrayInteraction {
            activate_window: self.activate.swap(false, Ordering::AcqRel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_moves_alpha_first() {
        assert_eq!(
            rgba_to_argb(&[1, 2, 3, 4, 10, 20, 30, 40]),
            vec![4, 1, 2, 3, 40, 10, 20, 30]
        );
    }

    #[test]
    fn pixmaps_match_their_declared_sizes() {
        for icon in pixmaps(TrayState::Recording) {
            assert_eq!(icon.width, icon.height);
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        }
    }

    #[test]
    fn recording_pixmap_centre_is_opaque_red() {
        let icon = pixmaps(TrayState::Recording).remove(1);
        let centre = ((icon.height / 2 * icon.width + icon.width / 2) * 4) as usize;
        let [a, r, g, b] = [
            icon.data[centre],
            icon.data[centre + 1],
            icon.data[centre + 2],
            icon.data[centre + 3],
        ];
        assert_eq!(a, 255);
        assert!(r > g && r > b, "centre is not red: {r},{g},{b}");
    }

    #[test]
    fn a_click_is_reported_once() {
        let manager = TrayManager::new();
        manager.activate.store(true, Ordering::Release);
        assert!(manager.poll_interaction().activate_window);
        assert!(!manager.poll_interaction().activate_window);
    }
}
