//! Render tests for the update badge: the confirmation dialog, the error line
//! under the badge, and the state suffix on the badge label.

use super::super::render_test_support::{contains_text, rendered_texts_after_frames};
use super::super::test_support::test_app;
use super::super::*;
use super::update_badge::update_badge_suffix;

fn badge_app(state: UpdateInstallState) -> WhisperDictateApp {
    let mut app = test_app(AppSettings::default());
    app.audio_devices_loaded = true;
    app.tray.disable();
    app.update_available = Some("9.9.9".to_owned());
    app.update_install = state;
    app
}

#[test]
fn confirmation_dialog_offers_update_and_cancel() {
    let mut app = badge_app(UpdateInstallState::Confirming(UpdateRun::Installer {
        version: "9.9.9".to_owned(),
    }));

    // The modal lays itself out invisibly on its first frame.
    let texts = rendered_texts_after_frames(&mut app, 2);

    for needle in ["Update whisper-dictate v9.9.9", "Update now", "Cancel"] {
        assert!(
            contains_text(&texts, needle),
            "missing {needle:?} in {texts:?}"
        );
    }
    // Rendering alone must not confirm anything.
    assert!(matches!(
        app.update_install,
        UpdateInstallState::Confirming(_)
    ));
}

#[test]
fn no_confirmation_dialog_without_a_pending_update() {
    let mut app = badge_app(UpdateInstallState::Idle);

    let texts = rendered_texts_after_frames(&mut app, 2);

    assert!(contains_text(&texts, "v9.9.9 available"));
    assert!(!contains_text(&texts, "Update now"));
}

#[test]
fn update_error_is_shown_under_the_badge() {
    let mut app = badge_app(UpdateInstallState::Failed("checksum mismatch".to_owned()));

    let texts = rendered_texts_after_frames(&mut app, 1);

    assert!(contains_text(&texts, "Update failed: checksum mismatch"));
}

#[test]
fn badge_suffix_reflects_the_update_state() {
    assert_eq!(
        update_badge_suffix(&UpdateInstallState::Started, false, "en"),
        "  · update started"
    );
    let (_tx, rx) = std::sync::mpsc::channel();
    assert_eq!(
        update_badge_suffix(
            &UpdateInstallState::Downloading(InstallerDownload::from_receiver(rx)),
            false,
            "da"
        ),
        "  · henter 0.0 MB"
    );
    assert_eq!(
        update_badge_suffix(&UpdateInstallState::Idle, true, "en"),
        "  · Copied!"
    );
    assert_eq!(
        update_badge_suffix(&UpdateInstallState::Idle, false, "en"),
        ""
    );
}
