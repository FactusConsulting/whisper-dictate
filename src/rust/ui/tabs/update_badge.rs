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
#[cfg(windows)]
use std::path::Path;
use std::time::{Duration, Instant};

/// Check whether the Chocolatey package directory for whisper-dictate exists.
///
/// Our Chocolatey package is a wrapper around the Inno installer, so the
/// running exe always lands in `%LOCALAPPDATA%\Programs\WhisperDictate` —
/// indistinguishable from a bare Inno install via the exe path alone. This
/// directory check is therefore the primary Chocolatey signal.
///
/// Lookup order:
/// 1. `$ChocolateyInstall\lib\whisper-dictate` (honoured when the env var is
///    set, which is the norm on machines where Chocolatey changed the install
///    root from the default).
/// 2. `%ProgramData%\chocolatey\lib\whisper-dictate` (the Chocolatey default).
///
/// Returns `false` on any error (missing env var, path construction failure,
/// I/O error) so the caller always gets a definitive `bool`.
///
/// Windows-only: Chocolatey does not exist elsewhere, so the non-Windows stub
/// returns `false` without touching the filesystem.
#[cfg(windows)]
fn probe_choco_pkg_dir() -> bool {
    // Try $ChocolateyInstall first, fall back to the hard-coded default.
    let candidates: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        if let Ok(choco_root) = std::env::var("ChocolateyInstall") {
            v.push(Path::new(&choco_root).join("lib").join("whisper-dictate"));
        }
        // Always include the well-known default path.
        if let Ok(program_data) = std::env::var("ProgramData") {
            v.push(
                Path::new(&program_data)
                    .join("chocolatey")
                    .join("lib")
                    .join("whisper-dictate"),
            );
        } else {
            // Hardcoded fallback if ProgramData is somehow unset.
            v.push(Path::new(r"C:\ProgramData\chocolatey\lib\whisper-dictate").to_path_buf());
        }
        v
    };
    candidates.iter().any(|p| p.is_dir())
}

/// Non-Windows stub: Chocolatey is Windows-only, so no filesystem probe.
#[cfg(not(windows))]
fn probe_choco_pkg_dir() -> bool {
    false
}

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
        // Probe the Chocolatey package dir at most once per session (cached so
        // we never hit the filesystem on every repaint frame).
        let choco_pkg_dir_exists = *self
            .choco_pkg_dir_exists
            .get_or_insert_with(probe_choco_pkg_dir);
        // When the offered version is a pre-release (`-rc.N`), the upgrade action
        // pins choco `--prerelease`/`--version` and links the rc's tag page for
        // the other install methods (see `upgrade_hint::upgrade_action`).
        let is_prerelease = version_is_prerelease(&version);
        let action = upgrade_action(
            detect_install_method(&exe_path, Os::current(), choco_pkg_dir_exists),
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
