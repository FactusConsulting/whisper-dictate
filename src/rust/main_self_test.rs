//! The `wd self-test` verb family: headless regression checks for the
//! shipped runtime (no OS hooks, no audio, no display). Each sub-verb
//! exercises one past-breakage class and exits non-zero on any failure so
//! CI (and the wayland-user-smoke script) can pin it without shelling out
//! for platform detects.
//!
//! The feature-gated media probes some sub-verbs delegate to (audio
//! capture, Whisper load) live in `main_media`.

use crate::main_media::{handle_audio_capture_self_test, handle_whisper_load};
use whisper_dictate_app::cli::SelfTestCommand;
/// Dispatch the `self-test` subcommand family. Every verb here is a pure,
/// headless regression check — no OS hooks, no audio, no display. Exits
/// non-zero on any failure so CI (and `wayland-user-smoke.sh`) can pin the
/// check without shelling out for platform detects.
pub(crate) fn handle_self_test(cmd: SelfTestCommand) -> anyhow::Result<()> {
    use whisper_dictate_app::hotkey::self_test::{
        features_available as ptt_features_available, run_ptt_wedge_test, SelfTestDriver,
    };
    use whisper_dictate_app::injection::self_test::{
        features_available as inj_features_available, run_injection_idempotency_test,
    };

    match cmd {
        SelfTestCommand::PttWedge {
            iterations,
            json,
            driver,
        } => {
            if iterations == 0 {
                return Err(anyhow::anyhow!(
                    "--iterations must be at least 1 (0 would be a vacuous pass)"
                ));
            }
            // Reject typo'd `--driver` BEFORE running the test, matching the
            // `hotkey capture --driver` policy (a smoke-script mis-spelling
            // should fail fast, not silently pick the auto backend).
            let parsed_driver = SelfTestDriver::parse(&driver).ok_or_else(|| {
                anyhow::anyhow!(
                    "--driver expects auto | rdev | evdev (or the x11 / wayland aliases); \
                     got {driver:?}"
                )
            })?;
            // Stock builds cannot exercise the guard bracket semantics (the
            // injector's `arm_start` lives behind `rust-injection`) — a "pass"
            // there would be a false negative and mask a real regression.
            // Surface an actionable rebuild message and exit non-zero.
            if !ptt_features_available() {
                return Err(anyhow::anyhow!(
                    "self-test ptt-wedge requires the `rust-hotkeys` and `rust-injection` \
                     cargo features - rebuild with \
                     `cargo build --features rust-hotkeys,rust-injection`"
                ));
            }
            let report = run_ptt_wedge_test(iterations, parsed_driver);
            if json {
                println!("{}", report.to_json());
            } else {
                print!("{}", report.to_plain());
            }
            if report.all_passed() {
                Ok(())
            } else {
                // Non-zero exit so CI trips. The report already printed the
                // per-iteration detail; a bare error keeps the tail short.
                Err(anyhow::anyhow!(
                    "self-test ptt-wedge failed (see report above for the failing iteration and stage)"
                ))
            }
        }
        SelfTestCommand::InjectionIdempotency {
            iterations,
            json,
            backend,
            live,
        } => {
            if iterations == 0 {
                return Err(anyhow::anyhow!(
                    "--iterations must be at least 1 (0 would be a vacuous pass)"
                ));
            }
            // Same feature-gate policy as ptt-wedge: on a stock build the
            // idempotency assertions can't fire (both the plan builder and
            // the guard bracket counter live behind those features), so a
            // "pass" would be a false negative. Surface a rebuild message.
            if !inj_features_available() {
                return Err(anyhow::anyhow!(
                    "self-test injection-idempotency requires the `rust-hotkeys` and \
                     `rust-injection` cargo features - rebuild with \
                     `cargo build --features rust-hotkeys,rust-injection`"
                ));
            }
            if live {
                // Loud stderr warning BEFORE any execution — mirrors the
                // `inject-text --do-it` policy so an operator who typed
                // `--live` by mistake sees the warning while they still
                // have a chance to Ctrl-C.
                eprintln!(
                    "warning: `self-test injection-idempotency --live` is REAL and will \
                     type into the active window on every iteration. Focus a scratch \
                     window NOW or Ctrl-C to abort."
                );
                // Codex #518 F5: on `--live --backend paste` the OS
                // clipboard is a shared resource. The harness inserts a
                // small spacer between iterations to let async clipboard
                // writes flush, but the operator is responsible for
                // ensuring the scratch window doesn't retain stale
                // content across iterations — surface this as a
                // documented limitation so nobody mistakes the run for a
                // hermetic test.
                if backend == "paste" {
                    eprintln!(
                        "warning: `--live --backend paste` shares the OS clipboard between \
                         iterations. Stale clipboard content from prior sessions may leak; \
                         inspect each iteration's pasted output manually rather than trusting \
                         the summary alone."
                    );
                }
            }
            let report = run_injection_idempotency_test(iterations, &backend, live);
            if json {
                println!("{}", report.to_json());
            } else {
                print!("{}", report.to_plain());
            }
            if report.all_passed() {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "self-test injection-idempotency failed (see report above for the failing iteration and stage)"
                ))
            }
        }
        SelfTestCommand::AudioCapture {
            duration_ms,
            device,
            json,
            fail_on_silence,
        } => handle_audio_capture_self_test(duration_ms, device, json, fail_on_silence),
        SelfTestCommand::WhisperLoad { model, json } => handle_whisper_load(&model, json),
        SelfTestCommand::Feedback { delay_ms, json } => handle_self_test_feedback(delay_ms, json),
        SelfTestCommand::AudioDucking { duration_ms, json } => {
            handle_self_test_audio_ducking(duration_ms, json)
        }
        SelfTestCommand::ProfileMatch {
            title,
            process,
            json,
        } => handle_self_test_profile_match(&title, &process, json),
        SelfTestCommand::HistoryWrite { text, json } => handle_self_test_history_write(&text, json),
        SelfTestCommand::MetricsWrite { text, json } => handle_self_test_metrics_write(&text, json),
        SelfTestCommand::Preview {
            frames,
            frame_samples,
            sample_rate,
            interval_ms,
            canned_text,
            json,
        } => handle_self_test_preview(
            frames,
            frame_samples,
            sample_rate,
            interval_ms,
            canned_text,
            json,
        ),
        SelfTestCommand::HotkeyBoot {
            hold_ms,
            chord,
            json,
            driver,
        } => handle_self_test_hotkey_boot(hold_ms, &chord, json, &driver),
    }
}

/// Dispatch `self-test hotkey-boot`. Exercises the SAME
/// [`whisper_dictate_app::hotkey::install_hotkey`] path the Phase-B
/// supervisor uses so a Windows-side wedge is reproducible from
/// PowerShell with visible stderr (the GUI binary runs under the
/// Windows GUI subsystem attribute and discards its stderr).
///
/// The `--driver` flag lets the operator pin a specific backend
/// most importantly `register` on Windows, which is the RegisterHotKey
/// driver the GUI binary uses to bypass the WH_KEYBOARD_LL hook chain.
/// Reproducing a GUI-side wedge from PowerShell needs the same
/// driver, so this flag mirrors `hotkey capture --driver`.
fn handle_self_test_hotkey_boot(
    hold_ms: u64,
    chord: &str,
    json: bool,
    driver: &str,
) -> anyhow::Result<()> {
    use whisper_dictate_app::hotkey::boot_self_test::{
        features_available, reconcile_config_load, resolve_chord, run_boot_test,
    };
    if !features_available() {
        return Err(anyhow::anyhow!(
            "self-test hotkey-boot requires the `rust-hotkeys` and `rust-injection` \
             cargo features - rebuild with \
             `cargo build --features rust-hotkeys,rust-injection`"
        ));
    }
    // Route the driver preference through the same env var the
    // shipping install path consults. Reject unrecognised values BEFORE
    // installing anything so a typo does not silently fall back to
    // Auto — matches the `hotkey capture --driver` policy.
    whisper_dictate_app::hotkey::capture::validate_driver_flag(driver)?;
    std::env::set_var("VOICEPI_HOTKEY_DRIVER", driver);
    // Fetch the on-disk config's `key` field so a bare invocation
    // uses the same chord the supervisor would. finding
    // r3658983556: a bare `unwrap_or_default()` masked a corrupt-config
    // I/O / parse failure and re-emerged as the misleading "no PTT
    // chord configured" message below, hiding the actual root cause
    // an operator debugging a wedge needs. The branching lives in the
    // pure helper `reconcile_config_load` so the "propagate the load
    // error when there is no override, otherwise warn-and-continue"
    // behaviour is directly unit-testable.
    let load_result = whisper_dictate_app::config::load_settings()
        .map(|s| s.key)
        .map_err(|err| err.to_string());
    let had_load_err = load_result.is_err();
    let config_key =
        reconcile_config_load(chord, load_result).map_err(|msg| anyhow::anyhow!(msg))?;
    if had_load_err {
        // discussion 3665200198 (main.rs:324): a plain
        // `eprintln!` panics on `write_all` failure, and a self-test
        // invoked from a hidden Windows launcher or with a closed
        // redirected stderr consumer would abort the CLI before
        // `run_boot_test` ever runs — the exact class of failure the
        // same commit fixed inside `diag::write_line`. Route through
        // the fallible-writer path (`diag::write_line`) instead so a
        // dead stderr is swallowed and the boot self-test still emits
        // its final report to stdout.
        whisper_dictate_app::diag::write_line(
            "[self-test hotkey-boot] warning: config load failed; \
             continuing with --chord override",
        );
    }
    let resolved = resolve_chord(chord, &config_key);
    if resolved.is_empty() {
        return Err(anyhow::anyhow!(
            "no PTT chord configured: pass `--chord <chord>` or set one via \
             `wd config set key ctrl_l+shift_l` first"
        ));
    }
    let report = run_boot_test(resolved, hold_ms);
    if json {
        println!("{}", report.to_json());
    } else {
        println!("{}", report.to_plain());
    }
    if report.ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test hotkey-boot failed (see report above for the driver / chord / error)"
        ))
    }
}

/// Dispatch `self-test feedback`.
fn handle_self_test_feedback(delay_ms: u64, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::feedback::{
        run_feedback_self_test, FeedbackOptions,
    };
    let report = run_feedback_self_test(FeedbackOptions {
        delay: std::time::Duration::from_millis(delay_ms),
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test feedback failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test audio-ducking`.
fn handle_self_test_audio_ducking(duration_ms: u64, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::audio_ducking::{
        run_audio_ducking_self_test, AudioDuckingOptions,
    };
    let report = run_audio_ducking_self_test(AudioDuckingOptions {
        duration: std::time::Duration::from_millis(duration_ms),
        force_enabled: None,
        force_level: None,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test audio-ducking failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test profile-match`.
fn handle_self_test_profile_match(title: &str, process: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::profile_match::{
        run_profile_match_self_test, ProfileMatchOptions,
    };
    let report = run_profile_match_self_test(ProfileMatchOptions {
        title: title.to_owned(),
        process: process.to_owned(),
        profiles_json_override: String::new(),
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test profile-match failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test history-write`.
fn handle_self_test_history_write(text: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::history_write::{
        run_history_write_self_test, HistoryWriteOptions,
    };
    let report = run_history_write_self_test(HistoryWriteOptions {
        text: text.to_owned(),
        path_override: None,
        force_enabled: None,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test history-write failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test metrics-write`.
fn handle_self_test_metrics_write(text: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::metrics_write::{
        run_metrics_write_self_test, MetricsWriteOptions,
    };
    let report = run_metrics_write_self_test(MetricsWriteOptions {
        text: text.to_owned(),
        path_override: None,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test metrics-write failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

/// Dispatch `self-test preview`.
fn handle_self_test_preview(
    frames: usize,
    frame_samples: usize,
    sample_rate: u32,
    interval_ms: u64,
    canned_text: String,
    json: bool,
) -> anyhow::Result<()> {
    use whisper_dictate_app::dictate::self_test::preview::{run_preview_self_test, PreviewOptions};
    let report = run_preview_self_test(PreviewOptions {
        frames,
        frame_samples,
        sample_rate,
        interval: std::time::Duration::from_millis(interval_ms),
        canned_text,
    });
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.exit_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test preview failed: {}",
            report.error.unwrap_or_else(|| "unknown".to_owned())
        ))
    }
}

#[cfg(test)]
mod tests {
    //! Source contracts for the self-test dispatcher family. The verb
    //! bodies delegate to lib self-test runners whose behavior is pinned in
    //! the lib suite; what the bin layer adds is the stderr plumbing, so
    //! the contract here is about that plumbing.

    use std::fs;
    use std::path::PathBuf;

    fn this_file() -> String {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fs::read_to_string(manifest.join("main_self_test.rs")).expect("read main_self_test.rs")
    }

    /// The hotkey-boot config-load warning must route through the fallible
    /// diagnostic writer, not `eprintln!` — a closed stderr would abort the
    /// launcher before `run_boot_test` starts. The lib-side scanner
    /// (`diag::logger_tests`) pins the same contract for the function body;
    /// this pins the routing call itself.
    #[test]
    fn hotkey_boot_warning_routes_through_the_fallible_writer() {
        let body = this_file();
        let at = body
            .find("fn handle_self_test_hotkey_boot(")
            .expect("hotkey-boot dispatcher present");
        assert!(
            body[at..].contains("whisper_dictate_app::diag::write_line"),
            "hotkey-boot must emit its config warning via diag::write_line"
        );
    }
}
