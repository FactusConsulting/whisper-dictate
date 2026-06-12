//! Regression tests for the update-check channel handling.
//!
//! These tests drive [`WhisperDictateApp::poll_update_check`] indirectly via
//! a pre-wired channel sender, or call [`apply_update_outcome`] directly (the
//! pure helper extracted for this purpose), to assert:
//!
//! 1. A simulated fetch-error outcome does NOT clear a prior `Some` badge.
//! 2. Flipping `local_only` ON clears `update_available` and drops the in-flight
//!    receiver so a late result is ignored.
//! 3. Flipping `update_check` OFF has the same clearing effect.
//! 4. A `Newer` outcome sets the badge; `UpToDate` clears it.

use super::test_support::test_app;
use super::update_check::{apply_update_outcome, UpdateCheckOutcome};
use super::*;

// ── apply_update_outcome (pure, no app state needed) ─────────────────────────

#[test]
fn apply_failed_preserves_prior_badge() {
    let prev = Some("1.9.0".to_owned());
    assert_eq!(
        apply_update_outcome(prev.clone(), UpdateCheckOutcome::Failed),
        prev
    );
}

#[test]
fn apply_failed_with_no_prior_badge_stays_none() {
    assert_eq!(apply_update_outcome(None, UpdateCheckOutcome::Failed), None);
}

#[test]
fn apply_up_to_date_clears_badge() {
    assert_eq!(
        apply_update_outcome(Some("1.9.0".to_owned()), UpdateCheckOutcome::UpToDate),
        None
    );
}

#[test]
fn apply_newer_sets_badge_from_none() {
    assert_eq!(
        apply_update_outcome(None, UpdateCheckOutcome::Newer("2.0.0".to_owned())),
        Some("2.0.0".to_owned())
    );
}

#[test]
fn apply_newer_replaces_older_badge() {
    assert_eq!(
        apply_update_outcome(
            Some("1.9.0".to_owned()),
            UpdateCheckOutcome::Newer("2.0.0".to_owned())
        ),
        Some("2.0.0".to_owned())
    );
}

// ── poll_update_check integration (drives WhisperDictateApp) ─────────────────

/// Wire a ready channel into `update_check_rx` and call `poll_update_check`
/// once; assert the badge reflects the outcome.
fn drive_poll(
    update_check: bool,
    local_only: bool,
    prior_badge: Option<String>,
    outcome: UpdateCheckOutcome,
) -> Option<String> {
    let settings = AppSettings {
        update_check,
        local_only,
        ..AppSettings::default()
    };

    let mut app = test_app(settings);
    app.update_available = prior_badge;

    // Pre-wire a channel that already has the outcome ready.
    let (tx, rx) = std::sync::mpsc::channel::<UpdateCheckOutcome>();
    tx.send(outcome).unwrap();
    app.update_check_rx = Some(rx);

    app.poll_update_check();
    app.update_available
}

#[test]
fn fetch_error_does_not_clear_prior_badge_in_poll() {
    let badge = drive_poll(
        true,
        false,
        Some("1.9.0".to_owned()),
        UpdateCheckOutcome::Failed,
    );
    assert_eq!(badge, Some("1.9.0".to_owned()));
}

#[test]
fn up_to_date_clears_badge_in_poll() {
    let badge = drive_poll(
        true,
        false,
        Some("1.9.0".to_owned()),
        UpdateCheckOutcome::UpToDate,
    );
    assert_eq!(badge, None);
}

#[test]
fn newer_sets_badge_in_poll() {
    let badge = drive_poll(
        true,
        false,
        None,
        UpdateCheckOutcome::Newer("2.0.0".to_owned()),
    );
    assert_eq!(badge, Some("2.0.0".to_owned()));
}

/// When `local_only` is turned ON, `poll_update_check` must clear any prior
/// badge and drop the in-flight receiver (so a late result is never adopted).
#[test]
fn local_only_on_clears_badge_and_drops_receiver() {
    let settings = AppSettings {
        update_check: true,
        local_only: true,
        ..AppSettings::default()
    };

    let mut app = test_app(settings);
    app.update_available = Some("1.9.0".to_owned());

    // An in-flight receiver whose result must be ignored.
    let (tx, rx) = std::sync::mpsc::channel::<UpdateCheckOutcome>();
    app.update_check_rx = Some(rx);

    // Drop the sender before polling so trying to receive yields Disconnected,
    // but the guard `!self.settings.local_only` must run and clear before any
    // result is adopted.
    drop(tx);

    app.poll_update_check();

    assert_eq!(
        app.update_available, None,
        "badge must be cleared when local_only is on"
    );
    assert!(
        app.update_check_rx.is_none(),
        "in-flight receiver must be dropped"
    );
}

/// When `update_check` is turned OFF, `poll_update_check` must clear badge and
/// drop the in-flight receiver.
#[test]
fn update_check_off_clears_badge_and_drops_receiver() {
    let settings = AppSettings {
        update_check: false,
        ..AppSettings::default()
    };

    let mut app = test_app(settings);
    app.update_available = Some("1.9.0".to_owned());

    let (_tx, rx) = std::sync::mpsc::channel::<UpdateCheckOutcome>();
    app.update_check_rx = Some(rx);

    app.poll_update_check();

    assert_eq!(
        app.update_available, None,
        "badge must be cleared when update_check is off"
    );
    assert!(
        app.update_check_rx.is_none(),
        "in-flight receiver must be dropped"
    );
}

// ── update_check_settings_changed (pure) ─────────────────────────────────────

#[test]
fn update_settings_change_detected_per_field_and_ignores_unrelated() {
    use super::update_check::update_check_settings_changed;
    let base = AppSettings::default();
    assert!(!update_check_settings_changed(&base, &base));

    let mut toggled = base.clone();
    toggled.update_check = !base.update_check;
    assert!(update_check_settings_changed(&base, &toggled));

    let mut interval = base.clone();
    interval.update_check_interval_minutes = "42".to_owned();
    assert!(update_check_settings_changed(&base, &interval));

    let mut rc = base.clone();
    rc.update_include_prereleases = !base.update_include_prereleases;
    assert!(update_check_settings_changed(&base, &rc));

    // An unrelated settings change must NOT reset the poll timer.
    let mut unrelated = base.clone();
    unrelated.model = "small".to_owned();
    assert!(!update_check_settings_changed(&base, &unrelated));
}
