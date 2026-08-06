//! Integration coverage for native UI background-task wiring.

use super::*;
use crate::platform::window_enumeration::VisibleWindow;
use crate::ui::test_support::test_app;
use std::time::{Duration, Instant};

fn fixed_windows() -> Result<Vec<VisibleWindow>, String> {
    Ok(vec![
        VisibleWindow {
            title: "Notes - draft".to_owned(),
            process: "notepad.exe".to_owned(),
        },
        VisibleWindow {
            title: "Browser".to_owned(),
            process: "browser.exe".to_owned(),
        },
    ])
}

#[test]
fn run_list_windows_populates_profiles_options_after_background_completion() {
    let mut app = test_app(AppSettings::default());
    *TEST_WINDOW_ENUMERATOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fixed_windows);

    app.run_list_windows();

    let deadline = Instant::now() + Duration::from_secs(2);
    while app.background_task.is_some() && Instant::now() < deadline {
        app.poll_background_task();
        std::thread::sleep(Duration::from_millis(1));
    }
    *TEST_WINDOW_ENUMERATOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

    assert!(
        app.background_task.is_none(),
        "background task did not finish"
    );
    assert_eq!(
        app.window_options,
        vec![
            ("Notes - draft".to_owned(), "notepad.exe".to_owned()),
            ("Browser".to_owned(), "browser.exe".to_owned()),
        ]
    );
    assert!(
        app.runtime_log
            .contains("[ui] window list refreshed: 2 window(s)"),
        "completion was not dispatched to the window-list handler: {}",
        app.runtime_log
    );
}

#[test]
fn reinject_last_reports_when_no_transcript_exists() {
    let mut app = test_app(AppSettings::default());
    app.run_reinject_last(REINJECT_LAST_LABEL);

    assert!(app.background_task.is_none());
    assert_eq!(
        app.last_runtime_error.as_deref(),
        Some("No transcript is available yet.")
    );
    assert!(app.runtime_log.contains("no transcript available"));
}
