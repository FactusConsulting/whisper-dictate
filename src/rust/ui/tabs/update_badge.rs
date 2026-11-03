//! The discreet, CLICKABLE "vX.Y.Z available" sidebar badge.
//!
//! This is the thin UI shell for the upgrade hint: it reads the running
//! executable path (`current_exe()`), renders the badge, and on click runs
//! the install-method-specific action. On Windows Chocolatey, winget and
//! installer installs that opens a confirmation dialog and then runs the
//! update in-app (`ui::update_install_flow`); Nix/Homebrew commands are copied
//! to the clipboard (with a transient "Copied!" confirmation) and portable
//! builds open the release page. The decision logic is PURE and unit-tested
//! in `ui::upgrade_hint` and `ui::update_install`.

use super::*; // tabs::* — icon_text, palette types
use egui_material_icons::icons;
#[cfg(windows)]
use std::path::Path;
use std::time::{Duration, Instant};

/// Check whether the Chocolatey package directory for whisper-dictate exists.
///
/// Our Chocolatey package is a wrapper around the Inno installer, so the
/// running exe always lands in `%LOCALAPPDATA%\Programs\WhisperDictate`
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

/// The text appended to the badge label for the update flow's state: the
/// download progress, "update started", or the transient "Copied!".
pub(in crate::ui) fn update_badge_suffix(
    state: &UpdateInstallState,
    copied: bool,
    lang: &str,
) -> String {
    match state {
        UpdateInstallState::Downloading(download) => {
            let (downloaded, total) = download.progress();
            format!(
                "  · {} {}",
                ui_text(lang, UiTextKey::UpdateDownloading),
                download_progress_text(downloaded, total)
            )
        }
        UpdateInstallState::Started => {
            format!("  · {}", ui_text(lang, UiTextKey::UpdateStarted))
        }
        _ if copied => format!("  · {}", ui_text(lang, UiTextKey::UpdateCommandCopied)),
        _ => String::new(),
    }
}

/// The hover line that says what a click on the badge does.
fn update_action_hover_key(action: &UpgradeAction) -> UiTextKey {
    match action {
        UpgradeAction::Command(_) => UiTextKey::UpdateCopyCommandHover,
        UpgradeAction::OpenUrl(_) => UiTextKey::UpdateOpenReleaseHover,
        UpgradeAction::Run(UpdateRun::Console(ConsoleUpdate::Choco { .. })) => {
            UiTextKey::UpdateRunChocoHover
        }
        UpgradeAction::Run(UpdateRun::Console(ConsoleUpdate::Winget)) => {
            UiTextKey::UpdateRunWingetHover
        }
        UpgradeAction::Run(UpdateRun::Installer { .. }) => UiTextKey::UpdateRunInstallerHover,
    }
}

/// The confirmation dialog's method-specific explanation.
fn update_confirm_detail_key(run: &UpdateRun) -> UiTextKey {
    match run {
        UpdateRun::Console(ConsoleUpdate::Choco { .. }) => UiTextKey::UpdateConfirmChoco,
        UpdateRun::Console(ConsoleUpdate::Winget) => UiTextKey::UpdateConfirmWinget,
        UpdateRun::Installer { .. } => UiTextKey::UpdateConfirmInstaller,
    }
}

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
        // adds choco `--prerelease`, downloads the rc's own installer, and links
        // the rc's tag page for the finals-only methods (see
        // `upgrade_hint::upgrade_action`).
        let is_prerelease = version_is_prerelease(&version);
        let action = upgrade_action(
            detect_install_method(&exe_path, Os::current(), choco_pkg_dir_exists),
            &version,
            is_prerelease,
        );

        // bottom_up layout: the error line added first sits BELOW the badge.
        if let UpdateInstallState::Failed(message) = &self.update_install {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{} {message}",
                        ui_text(&lang, UiTextKey::UpdateFailed)
                    ))
                    .text_style(egui::TextStyle::Small)
                    .color(palette.error_text),
                )
                .wrap(),
            );
        }

        let copied = self
            .update_command_copied_until
            .is_some_and(|until| Instant::now() < until);
        let label = format!(
            "v{} {}{}",
            version,
            ui_text(&lang, UiTextKey::UpdateAvailable),
            update_badge_suffix(&self.update_install, copied, &lang),
        );
        // The hover leads with the generic "newer version" note, then spells out
        // exactly what a click does plus the command / URL it acts on. Once an
        // updater has started, a click does nothing and the hover says so.
        let started = matches!(self.update_install, UpdateInstallState::Started);
        let hover = if started {
            format!(
                "{}\n{}",
                ui_text(&lang, UiTextKey::UpdateAvailableHover),
                ui_text(&lang, UiTextKey::UpdateStartedHover),
            )
        } else {
            format!(
                "{}\n{}\n{}",
                ui_text(&lang, UiTextKey::UpdateAvailableHover),
                ui_text(&lang, update_action_hover_key(&action)),
                action.target(),
            )
        };

        let response = ui
            .add(
                egui::Label::new(
                    icon_text(icons::ICON_ARROW_UPWARD.codepoint, label)
                        .text_style(egui::TextStyle::Small)
                        .strong()
                        .color(palette.accent_blue),
                )
                .selectable(false)
                .sense(egui::Sense::click()),
            )
            .on_hover_text(hover)
            // A pointing-hand cursor signals the badge is actionable.
            .on_hover_cursor(if started {
                egui::CursorIcon::Default
            } else {
                egui::CursorIcon::PointingHand
            });

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
                UpgradeAction::Run(run) => self.request_update_install(run),
            }
        }

        self.update_confirm_dialog(ui.ctx(), &lang, &version);
    }

    /// The modal that asks before an update runs, because the app closes
    /// during it.
    fn update_confirm_dialog(&mut self, ctx: &egui::Context, lang: &str, version: &str) {
        let UpdateInstallState::Confirming(run) = &self.update_install else {
            return;
        };
        let detail = ui_text(lang, update_confirm_detail_key(run));
        let target = run.target();
        let mut confirmed = None;
        let modal = egui::Modal::new(egui::Id::unique("update_install_confirm")).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.heading(format!(
                "{} v{version}",
                ui_text(lang, UiTextKey::UpdateConfirmTitle)
            ));
            ui.add_space(6.0);
            ui.label(detail);
            ui.add_space(4.0);
            ui.label(egui::RichText::new(target).monospace().small());
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(ui_text(lang, UiTextKey::UpdateConfirmClosesApp)).strong(),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .button(ui_text(lang, UiTextKey::UpdateConfirmButton))
                    .clicked()
                {
                    confirmed = Some(true);
                }
                if ui.button(ui_text(lang, UiTextKey::UpdateCancel)).clicked() {
                    confirmed = Some(false);
                }
            });
        });
        match confirmed {
            Some(true) => self.confirm_update_install(&SystemUpdateLauncher),
            Some(false) => self.dismiss_update_install(),
            None if modal.should_close() => self.dismiss_update_install(),
            None => {}
        }
    }
}
