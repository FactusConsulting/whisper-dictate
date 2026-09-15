//! Tests for the pure open-on-press device policy.

use super::{
    has_named_device, open_with_fallback, probe_startup_device, OpenBreaker, OpenFailure,
    OpenTarget, StartupDevice,
};

fn is_timeout(error: &anyhow::Error) -> bool {
    error.to_string().contains("timed out")
}

#[test]
fn named_device_is_opened_first_and_default_is_not_touched() {
    let mut selectors = Vec::new();
    let mut breaker = OpenBreaker::default();
    let opened = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |selector| {
            selectors.push(selector.to_owned());
            Ok(selector.to_owned())
        },
        is_timeout,
    )
    .unwrap();
    assert_eq!(opened.target, OpenTarget::Configured);
    assert_eq!(opened.stream, "USB microphone");
    assert_eq!(opened.configured_error, None);
    assert_eq!(selectors, ["USB microphone"]);
}

#[test]
fn configured_failure_falls_back_to_default_and_keeps_the_error() {
    let mut selectors = Vec::new();
    let mut breaker = OpenBreaker::default();
    let opened = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |selector| {
            selectors.push(selector.to_owned());
            if selector.is_empty() {
                Ok("default")
            } else {
                Err(anyhow::anyhow!("named device disappeared"))
            }
        },
        is_timeout,
    )
    .unwrap();
    assert_eq!(opened.target, OpenTarget::SystemDefault);
    assert_eq!(
        opened.configured_error.as_deref(),
        Some("named device disappeared")
    );
    assert_eq!(selectors, ["USB microphone", ""]);
    assert_eq!(
        breaker,
        OpenBreaker::default(),
        "plain errors must be retried next press"
    );
}

#[test]
fn both_failures_are_reported_and_later_presses_may_retry() {
    let mut breaker = OpenBreaker::default();
    let failure = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |selector| Err::<(), _>(anyhow::anyhow!("cannot open {selector:?}")),
        is_timeout,
    )
    .unwrap_err();
    assert!(failure
        .message
        .contains("configured input (cannot open \"USB microphone\")"));
    assert!(failure
        .message
        .contains("system default input (cannot open \"\")"));
    assert!(!failure.paused);
}

#[test]
fn empty_selector_only_tries_the_system_default() {
    let mut selectors = Vec::new();
    let opened = open_with_fallback(
        " ",
        &mut OpenBreaker::default(),
        |selector| {
            selectors.push(selector.to_owned());
            Ok(())
        },
        is_timeout,
    )
    .unwrap();
    assert_eq!(opened.target, OpenTarget::SystemDefault);
    assert_eq!(opened.configured_error, None);
    assert_eq!(selectors, [""]);
}

#[test]
fn configured_timeout_is_skipped_on_every_later_press() {
    let mut breaker = OpenBreaker::default();
    let _ = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |selector| {
            if selector.is_empty() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("open timed out"))
            }
        },
        is_timeout,
    )
    .unwrap();
    assert!(breaker.configured_timed_out);
    assert_eq!(
        breaker.candidates("USB microphone"),
        [OpenTarget::SystemDefault]
    );
}

#[test]
fn all_timeouts_pause_opening_without_further_attempts() {
    let mut breaker = OpenBreaker::default();
    let failure = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |_| Err::<(), _>(anyhow::anyhow!("open timed out")),
        is_timeout,
    )
    .unwrap_err();
    assert!(failure.paused);

    let mut attempts = 0;
    let failure = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |_| {
            attempts += 1;
            Ok::<(), anyhow::Error>(())
        },
        is_timeout,
    )
    .unwrap_err();
    assert_eq!(attempts, 0, "a paused breaker must not call the driver");
    assert_eq!(
        failure,
        OpenFailure {
            message: "an earlier microphone open timed out inside the audio driver".to_owned(),
            paused: true,
        }
    );
}

#[test]
fn configured_timeout_with_plain_default_failure_is_not_paused() {
    let mut breaker = OpenBreaker::default();
    let failure = open_with_fallback(
        "USB microphone",
        &mut breaker,
        |selector| {
            if selector.is_empty() {
                Err::<(), _>(anyhow::anyhow!("default busy"))
            } else {
                Err(anyhow::anyhow!("open timed out"))
            }
        },
        is_timeout,
    )
    .unwrap_err();
    assert!(!failure.paused);
    assert_eq!(
        breaker.candidates("USB microphone"),
        [OpenTarget::SystemDefault]
    );
}

#[test]
fn targets_map_to_cpal_selectors_and_labels() {
    assert_eq!(OpenTarget::Configured.selector("USB mic"), "USB mic");
    assert_eq!(OpenTarget::SystemDefault.selector("USB mic"), "");
    assert_eq!(OpenTarget::Configured.effective_label("USB mic"), "USB mic");
    assert_eq!(
        OpenTarget::SystemDefault.effective_label("USB mic"),
        "System default"
    );
    assert!(has_named_device("USB mic"));
    assert!(!has_named_device(" \t"));
}

#[test]
fn startup_probe_accepts_a_present_configured_input() {
    let mut probed = Vec::new();
    let device = probe_startup_device("USB mic", |selector| {
        probed.push(selector.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(device, StartupDevice::Configured);
    assert_eq!(probed, ["USB mic"]);
}

#[test]
fn startup_probe_reports_a_missing_configured_input_when_default_exists() {
    let device = probe_startup_device("USB mic", |selector| {
        if selector.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("input device not found: USB mic"))
        }
    })
    .unwrap();
    assert_eq!(
        device,
        StartupDevice::ConfiguredMissing {
            error: "input device not found: USB mic".to_owned()
        }
    );
}

#[test]
fn startup_probe_fails_when_no_input_exists() {
    let error = probe_startup_device("USB mic", |selector| {
        Err(anyhow::anyhow!("missing {selector:?}"))
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("configured input (missing \"USB mic\")"));
    assert!(error.contains("system default input is also unavailable (missing \"\")"));

    assert!(probe_startup_device("", |_| Err(anyhow::anyhow!("none"))).is_err());
    assert_eq!(
        probe_startup_device("", |_| Ok(())).unwrap(),
        StartupDevice::SystemDefault
    );
}
