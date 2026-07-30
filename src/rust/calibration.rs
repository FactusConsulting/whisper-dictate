//! Native microphone and WAV calibration.
//!
//! This ports the supported `--calibrate-mic` / `--calibrate-file` behavior
//! from Python onto the shared Rust capture, WAV, and audio-DSP modules.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

const SAMPLE_RATE_HZ: f64 = 16_000.0;
const MIN_ANALYSIS_SAMPLES: usize = 480 * 4;
const MIN_MIC_SECONDS: f64 = 1.0;
const MAX_MIC_SECONDS: f64 = 300.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recommendations {
    #[serde(rename = "VOICEPI_TARGET_DBFS")]
    target_dbfs: String,
    #[serde(rename = "VOICEPI_MIN_INPUT_DBFS")]
    min_input_dbfs: String,
    #[serde(rename = "VOICEPI_MIN_SNR_DB")]
    min_snr_db: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibrationReport {
    event: &'static str,
    status: &'static str,
    warnings: Vec<&'static str>,
    duration_s: f64,
    raw_dbfs: f64,
    noise_dbfs: f64,
    snr_db: f64,
    peak: f64,
    recommended: Recommendations,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decode_s: Option<f64>,
}

fn calibration_status(raw_dbfs: f64, snr_db: f64) -> (&'static str, Vec<&'static str>) {
    let mut warnings = Vec::new();
    if raw_dbfs < -55.0 {
        warnings.push("input is very quiet");
    } else if raw_dbfs < -42.0 {
        warnings.push("input is quiet");
    }
    if snr_db < 6.0 {
        warnings.push("speech/noise contrast is too low");
    } else if snr_db < 15.0 {
        warnings.push("speech/noise contrast is marginal");
    }
    let status = if warnings.is_empty() {
        "pass"
    } else if raw_dbfs < -55.0 || snr_db < 6.0 {
        "fail"
    } else {
        "warn"
    };
    (status, warnings)
}

pub fn analyze(samples: &[f32]) -> Result<CalibrationReport> {
    if samples.len() < MIN_ANALYSIS_SAMPLES {
        return Err(anyhow!(
            "calibration input is too short: need at least {:.2}s of 16 kHz audio",
            MIN_ANALYSIS_SAMPLES as f64 / SAMPLE_RATE_HZ
        ));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(anyhow!("calibration input contains non-finite samples"));
    }
    let peak = samples
        .iter()
        .map(|sample| f64::from(sample.abs()))
        .fold(0.0_f64, f64::max);
    if peak <= f64::EPSILON {
        return Err(anyhow!(
            "calibration input is silent; check that the microphone is unmuted"
        ));
    }

    let metrics =
        crate::audio_dsp::capture_metrics(samples, &crate::audio_dsp::StatusThresholds::default());
    let (status, warnings) = calibration_status(metrics.raw_dbfs, metrics.snr_db);
    let recommended_min_input = (metrics.raw_dbfs - 18.0).clamp(-65.0, -35.0);
    let recommended_min_snr = if metrics.snr_db < 15.0 {
        6.0
    } else {
        (metrics.snr_db - 12.0).clamp(6.0, 12.0)
    };
    Ok(CalibrationReport {
        event: "mic_calibration",
        status,
        warnings,
        duration_s: samples.len() as f64 / SAMPLE_RATE_HZ,
        raw_dbfs: metrics.raw_dbfs,
        noise_dbfs: metrics.noise_dbfs,
        snr_db: metrics.snr_db,
        peak,
        recommended: Recommendations {
            target_dbfs: "-20".to_owned(),
            min_input_dbfs: format!("{recommended_min_input:.0}"),
            min_snr_db: format!("{recommended_min_snr:.0}"),
        },
        source_file: None,
        decode_s: None,
    })
}

fn print_report(report: &CalibrationReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(report)?);
        return Ok(());
    }
    println!("[calibrate] status={}", report.status);
    println!(
        "[calibrate] raw={:.0}dBFS noise={:.0}dBFS snr={:.0}dB peak={:.3}",
        report.raw_dbfs, report.noise_dbfs, report.snr_db, report.peak
    );
    for warning in &report.warnings {
        println!("[calibrate] warning: {warning}");
    }
    println!("[calibrate] recommended settings:");
    println!("  VOICEPI_TARGET_DBFS={}", report.recommended.target_dbfs);
    println!(
        "  VOICEPI_MIN_INPUT_DBFS={}",
        report.recommended.min_input_dbfs
    );
    println!("  VOICEPI_MIN_SNR_DB={}", report.recommended.min_snr_db);
    Ok(())
}

pub fn handle_file(path: &Path, json: bool) -> Result<()> {
    let started = Instant::now();
    let samples = crate::whisper::decode_wav_16k_mono(path)
        .with_context(|| format!("decode calibration file {}", path.display()))?;
    let mut report = analyze(&samples)?;
    report.source_file = Some(path.to_path_buf());
    report.decode_s = Some(started.elapsed().as_secs_f64());
    print_report(&report, json)
}

fn selected_device(override_device: Option<&str>) -> Result<String> {
    if let Some(device) = override_device {
        return Ok(device.trim().to_owned());
    }
    Ok(crate::config::load_settings()
        .context("load configured microphone")?
        .audio_device)
}

pub fn handle_microphone(seconds: f64, device: Option<&str>, json: bool) -> Result<()> {
    handle_microphone_with(seconds, device, json, capture_microphone, print_report)
}

fn handle_microphone_with(
    seconds: f64,
    device: Option<&str>,
    json: bool,
    capture: impl FnOnce(&str, f64) -> Result<Vec<f32>>,
    report: impl FnOnce(&CalibrationReport, bool) -> Result<()>,
) -> Result<()> {
    if !seconds.is_finite() || !(MIN_MIC_SECONDS..=MAX_MIC_SECONDS).contains(&seconds) {
        return Err(anyhow!(
            "calibration seconds must be between {MIN_MIC_SECONDS} and {MAX_MIC_SECONDS}"
        ));
    }
    let device = selected_device(device)?;
    eprintln!("[calibrate] speak normally for {seconds:.1}s...");
    let samples = capture(&device, seconds)?;
    let calibration = analyze(&samples)?;
    report(&calibration, json)
}

#[cfg(feature = "audio-capture")]
fn capture_microphone(device: &str, seconds: f64) -> Result<Vec<f32>> {
    crate::dictate::mic::capture_pcm_for(device, seconds, MIN_MIC_SECONDS)
}

#[cfg(not(feature = "audio-capture"))]
fn capture_microphone(_device: &str, _seconds: f64) -> Result<Vec<f32>> {
    Err(anyhow!(
        "calibrate-mic requires the `audio-capture` cargo feature; rebuild with \
         `cargo build --features audio-capture`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::{restore_env, ENV_LOCK};
    use std::cell::{Cell, RefCell};

    fn contrasted_audio() -> Vec<f32> {
        let mut samples = vec![0.001; 480 * 8];
        for sample in &mut samples[480 * 4..] {
            *sample = 0.2;
        }
        samples
    }

    #[test]
    fn analysis_matches_python_recommendation_contract() {
        let report = analyze(&contrasted_audio()).unwrap();
        assert_eq!(report.event, "mic_calibration");
        assert_eq!(report.status, "pass");
        assert!(report.snr_db > 15.0);
        assert_eq!(report.recommended.target_dbfs, "-20");
        assert_eq!(report.recommended.min_snr_db, "12");
    }

    #[test]
    fn quiet_flat_audio_reports_failure_and_bounded_recommendations() {
        let report = analyze(&vec![0.0005; 480 * 4]).unwrap();
        assert_eq!(report.status, "fail");
        assert!(report.warnings.contains(&"input is very quiet"));
        assert!(report
            .warnings
            .contains(&"speech/noise contrast is too low"));
        assert_eq!(report.recommended.min_input_dbfs, "-65");
        assert_eq!(report.recommended.min_snr_db, "6");
    }

    #[test]
    fn invalid_short_and_silent_inputs_fail_clearly() {
        assert!(analyze(&[0.1; 100])
            .unwrap_err()
            .to_string()
            .contains("too short"));
        assert!(analyze(&vec![0.0; MIN_ANALYSIS_SAMPLES])
            .unwrap_err()
            .to_string()
            .contains("silent"));
        let mut invalid = vec![0.1; MIN_ANALYSIS_SAMPLES];
        invalid[20] = f32::NAN;
        assert!(analyze(&invalid)
            .unwrap_err()
            .to_string()
            .contains("non-finite"));
    }

    #[test]
    fn microphone_duration_bounds_fail_before_opening_hardware() {
        for seconds in [0.5, 301.0, f64::NAN, f64::INFINITY] {
            let capture_called = Cell::new(false);
            let error = handle_microphone_with(
                seconds,
                Some("unused"),
                false,
                |_, _| {
                    capture_called.set(true);
                    Ok(contrasted_audio())
                },
                |_, _| Ok(()),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("between 1"), "{error}");
            assert!(!capture_called.get());
        }
    }

    #[test]
    fn microphone_handler_uses_override_and_reports_without_real_device() {
        let captured = RefCell::new(None);
        let reported = Cell::new(false);
        handle_microphone_with(
            2.5,
            Some("  USB Mic  "),
            true,
            |device, seconds| {
                captured.replace(Some((device.to_owned(), seconds)));
                Ok(contrasted_audio())
            },
            |report, json| {
                assert_eq!(report.status, "pass");
                assert!(json);
                reported.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(captured.into_inner(), Some(("USB Mic".to_owned(), 2.5)));
        assert!(reported.get());
    }

    #[test]
    fn configured_device_is_used_when_override_is_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"audio_device":"Configured Mic"}"#).unwrap();
        let old = std::env::var_os("VOICEPI_CONFIG");
        std::env::set_var("VOICEPI_CONFIG", &path);

        assert_eq!(selected_device(None).unwrap(), "Configured Mic");
        assert_eq!(
            selected_device(Some(" Explicit Mic ")).unwrap(),
            "Explicit Mic"
        );

        restore_env("VOICEPI_CONFIG", old);
    }

    #[cfg(not(feature = "audio-capture"))]
    #[test]
    fn stock_build_microphone_stub_is_actionable() {
        let error = capture_microphone("", 1.0).unwrap_err().to_string();
        assert!(error.contains("audio-capture"), "{error}");
    }
}
