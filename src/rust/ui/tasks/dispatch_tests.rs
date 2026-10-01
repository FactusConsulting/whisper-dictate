use super::*;

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
