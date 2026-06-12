//! The discreet, CLICKABLE "vX.Y.Z available" sidebar badge.
//!
//! This is the thin, untestable UI shell for the upgrade hint: it reads the
//! running executable path (`current_exe()`), renders the badge, and on click
//! either copies the install-method-specific upgrade command to the clipboard
//! (with a transient "Copied!" confirmation) or opens the latest-release page.
//! All the decision logic — classifying the install method and choosing the
//! upgrade action — is PURE and unit-tested in `ui::upgrade_hint`.

use super::super::*; // crate::ui::* — UiTextKey, ui_text, upgrade-hint helpers, open_url, …
use super::*; // tabs::* — icon_text, palette types
use egui_material_icons::icons;
use std::time::{Duration, Instant};

/// How long the transient "Copied!" confirmation stays visible after a copy.
const COPIED_CONFIRMATION: Duration = Duration::from_secs(2);

impl WhisperDictateApp {
    /// Render the actionable update badge. Only called when `update_available`
    /// is `Some`.
    pub(in crate::ui) fn update_badge(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let Some(version) = self.update_available.clone() else {
            return;
        };
        let lang = self.settings.ui_language.clone();
        // Thin shell: read the running exe path (best-effort) and classify it.
        let exe_path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        // When the offered version is a pre-release (`-rc.N`), the upgrade action
        // pins choco `--prerelease`/`--version` and links the rc's tag page for
        // the other install methods (see `upgrade_hint::upgrade_action`).
        let is_prerelease = version_is_prerelease(&version);
        let action = upgrade_action(
            detect_install_method(&exe_path, Os::current()),
            &version,
            is_prerelease,
        );

        // A transient "Copied!" suffix for a couple of seconds after a copy.
        let copied = self
            .update_command_copied_until
            .is_some_and(|until| Instant::now() < until);
        let suffix = if copied {
            format!("  · {}", ui_text(&lang, UiTextKey::UpdateCommandCopied))
        } else {
            String::new()
        };
        let label = format!(
            "v{} {}{}",
            version,
            ui_text(&lang, UiTextKey::UpdateAvailable),
            suffix,
        );

        // The hover leads with the generic "newer version" note, then spells out
        // exactly what a click does (copy <cmd> / open <url>) plus the target.
        let hover_key = match action {
            UpgradeAction::Command(_) => UiTextKey::UpdateCopyCommandHover,
            UpgradeAction::OpenUrl(_) => UiTextKey::UpdateOpenReleaseHover,
        };
        let hover = format!(
            "{}\n{}\n{}",
            ui_text(&lang, UiTextKey::UpdateAvailableHover),
            ui_text(&lang, hover_key),
            action.target(),
        );

        let response = ui
            .add(
                egui::Label::new(
                    icon_text(icons::ICON_ARROW_UPWARD, label)
                        .text_style(egui::TextStyle::Small)
                        .strong()
                        .color(palette.accent_blue),
                )
                .selectable(false)
                .sense(egui::Sense::click()),
            )
            .on_hover_text(hover)
            // A pointing-hand cursor signals the badge is actionable.
            .on_hover_cursor(egui::CursorIcon::PointingHand);

        if response.clicked() {
            match action {
                UpgradeAction::Command(cmd) => {
                    ui.ctx().copy_text(cmd);
                    self.update_command_copied_until = Some(Instant::now() + COPIED_CONFIRMATION);
                    // Repaint once the confirmation expires so the "Copied!" suffix
                    // clears itself without requiring another user interaction.
                    ui.ctx().request_repaint_after(COPIED_CONFIRMATION);
                }
                UpgradeAction::OpenUrl(url) => {
                    let _ = open_url(&url);
                }
            }
        }
    }
}
