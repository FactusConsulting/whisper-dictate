//! `whisper-dictate self-test preview` — spin up a real
//! [`crate::dictate::PreviewEngine`] with a mock backend, drive
//! `push_frame` N times, and collect the emitted previews.
//!
//! ## What this catches
//!
//! The preview engine is a background thread with a channel + interval
//! timer + fresh-audio gate. Silent breakage modes:
//!
//! * `Start` -> `Frame` -> `Stop` messages don't wake the worker
//!   (channel deadlocked, worker panicked).
//! * The fresh-audio gate rejects every tick (a mis-tuned
//!   `MIN_NEW_AUDIO_S` at a low sample rate).
//! * The `PreviewSink` panics on emit and the worker never recovers.
//!
//! This verb boots the shipping engine with a `CannedPreviewBackend`
//! that just returns a fixed string, feeds it N frames of large-enough
//! audio to clear the fresh-audio gate, and reports how many previews
//! landed. `emissions >= 1` is the pass signal — the worker booted,
//! ticked, and delivered.
//!
//! ## Envelope
//!
//! ```json
//! {
//!   "kind": "preview_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   "frames_pushed": 5,
//!   "frame_samples": 24000,
//!   "sample_rate": 16000,
//!   "interval_ms": 100,
//!   "emissions": [
//!     {"text": "canned preview", "recording_s": 7.5}
//!   ]
//! }
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::dictate::session::preview::{
    PreviewBackend, PreviewEmission, PreviewEngine, PreviewEngineConfig, PreviewError, PreviewSink,
};

/// Options for [`run_preview_self_test`].
#[derive(Debug, Clone)]
pub struct PreviewOptions {
    /// How many `push_frame` calls to make. Defaults to 5; the frame
    /// size is chosen so 5 frames cross the fresh-audio gate at 16 kHz.
    pub frames: usize,
    /// Sample rate the frames are at. Must match the config so the
    /// gate arithmetic lines up.
    pub sample_rate: u32,
    /// Number of f32 samples per frame. Default `24000` (1.5 s @ 16
    /// kHz), which crosses the [`crate::dictate::session::preview::MIN_NEW_AUDIO_S`]
    /// gate on the very first tick.
    pub frame_samples: usize,
    /// Interval between preview ticks. Kept short so the verb runs
    /// under a second.
    pub interval: Duration,
    /// Canned text the mock backend returns. Empty string forces an
    /// empty emission (the engine treats "" as "nothing to show" and
    /// skips silently) — useful for negative testing.
    pub canned_text: String,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            frames: 5,
            sample_rate: 16_000,
            frame_samples: 24_000,
            interval: Duration::from_millis(100),
            canned_text: "canned preview".to_owned(),
        }
    }
}

/// Verb output.
#[derive(Debug, Clone)]
pub struct PreviewReport {
    pub frames_pushed: usize,
    pub frame_samples: usize,
    pub sample_rate: u32,
    pub interval: Duration,
    pub emissions: Vec<PreviewEmission>,
    pub error: Option<String>,
}

impl PreviewReport {
    pub fn exit_ok(&self) -> bool {
        self.error.is_none()
    }

    pub fn to_json(&self) -> String {
        let emissions: Vec<Value> = self
            .emissions
            .iter()
            .map(|e| {
                json!({
                    "text": e.text,
                    "recording_s": e.recording_s,
                })
            })
            .collect();
        json!({
            "kind": "preview_self_test",
            "ok": self.exit_ok(),
            "error": self.error,
            "frames_pushed": self.frames_pushed,
            "frame_samples": self.frame_samples,
            "sample_rate": self.sample_rate,
            "interval_ms": self.interval.as_millis() as u64,
            "emissions": emissions,
        })
        .to_string()
    }

    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test preview] frames_pushed={} frame_samples={} sr={} interval={}ms\n",
            self.frames_pushed,
            self.frame_samples,
            self.sample_rate,
            self.interval.as_millis()
        );
        out.push_str(&format!("  emissions={}\n", self.emissions.len()));
        for (i, em) in self.emissions.iter().enumerate() {
            out.push_str(&format!(
                "    [{i}] text={:?} recording_s={:.2}\n",
                em.text, em.recording_s
            ));
        }
        if let Some(err) = &self.error {
            out.push_str(&format!("  FAIL: {err}\n"));
        } else {
            out.push_str("  PASS\n");
        }
        out
    }
}

/// Mock backend that returns a fixed string on every call and counts
/// invocations. Exposed to the tests so they can assert the worker
/// actually invoked the backend (not just booted the thread).
pub struct CannedPreviewBackend {
    text: String,
    calls: AtomicUsize,
}

impl CannedPreviewBackend {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl PreviewBackend for CannedPreviewBackend {
    fn transcribe_partial(&self, _pcm: &[f32], _sample_rate: u32) -> Result<String, PreviewError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.text.clone())
    }
}

/// Drive the engine. Returns after `frames` frames have been pushed and
/// enough wall-clock time has elapsed to give the worker several tick
/// opportunities.
pub fn run_preview_self_test(opts: PreviewOptions) -> PreviewReport {
    let config = PreviewEngineConfig {
        interval: opts.interval,
        sample_rate: opts.sample_rate,
        min_new_audio_s: crate::dictate::session::preview::MIN_NEW_AUDIO_S,
        max_audio_s: crate::dictate::session::preview::PREVIEW_MAX_AUDIO_S,
        text_chars: crate::dictate::session::preview::PREVIEW_TEXT_CHARS,
    };

    let backend: Arc<dyn PreviewBackend> =
        Arc::new(CannedPreviewBackend::new(opts.canned_text.clone()));
    let captured = Arc::new(Mutex::new(Vec::<PreviewEmission>::new()));
    let captured_sink = Arc::clone(&captured);
    let sink: PreviewSink = Arc::new(move |emission: PreviewEmission| {
        if let Ok(mut guard) = captured_sink.lock() {
            guard.push(emission);
        }
    });

    let engine = PreviewEngine::spawn(backend, config, sink);
    engine.notify_start();

    let frame: Vec<f32> = vec![0.01_f32; opts.frame_samples];
    for _ in 0..opts.frames {
        engine.push_frame(&frame);
        // A short sleep between frames so the worker gets a chance to
        // wake up mid-stream. Total wall clock stays under a second for
        // the default (5 frames * 50 ms = 250 ms) plus a settle window.
        thread::sleep(Duration::from_millis(50));
    }

    // Give the worker at least one interval past the last frame push so
    // the tick that observes the accumulated buffer can fire. 300 ms is
    // 3x the default `--interval-ms`.
    thread::sleep(opts.interval + Duration::from_millis(300));

    engine.notify_stop();
    // Drop the engine to join the worker cleanly (this also sends the
    // Shutdown message via the Drop impl).
    drop(engine);

    let emissions = captured
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| Vec::new());

    let error = if !opts.canned_text.trim().is_empty() && emissions.is_empty() {
        Some(
            "preview engine produced no emissions despite a non-empty canned text - \
             worker thread or channel wiring is broken"
                .to_owned(),
        )
    } else {
        None
    };

    PreviewReport {
        frames_pushed: opts.frames,
        frame_samples: opts.frame_samples,
        sample_rate: opts.sample_rate,
        interval: opts.interval,
        emissions,
        error,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canned_backend_returns_fixed_text_and_counts_calls() {
        let backend = CannedPreviewBackend::new("hello");
        assert_eq!(backend.transcribe_partial(&[], 16_000).unwrap(), "hello");
        assert_eq!(backend.transcribe_partial(&[], 16_000).unwrap(), "hello");
        assert_eq!(backend.calls(), 2);
    }

    #[test]
    fn default_options_produce_at_least_one_emission() {
        // Default frame size (24 000 samples @ 16 kHz = 1.5 s) crosses
        // the MIN_NEW_AUDIO_S gate on the first tick, so the engine
        // MUST produce at least one emission. If this ever regresses to
        // zero, the fresh-audio gate arithmetic is broken.
        let report = run_preview_self_test(PreviewOptions::default());
        assert!(
            !report.emissions.is_empty(),
            "expected at least one emission, got report: {}",
            report.to_plain()
        );
        assert!(report.exit_ok(), "run must pass: {}", report.to_plain());
        // First emission must carry the canned text.
        assert_eq!(report.emissions[0].text, "canned preview");
        assert!(report.emissions[0].recording_s > 0.0);
    }

    #[test]
    fn empty_canned_text_produces_no_emissions_and_still_passes() {
        // Engine treats empty text as "nothing to show" and skips
        // silently — that's the correct behaviour, so we do NOT flag
        // this as a failure.
        let report = run_preview_self_test(PreviewOptions {
            canned_text: String::new(),
            ..PreviewOptions::default()
        });
        assert!(report.emissions.is_empty());
        assert!(report.exit_ok());
    }

    #[test]
    fn report_json_shape_has_stable_keys() {
        let report = PreviewReport {
            frames_pushed: 5,
            frame_samples: 24_000,
            sample_rate: 16_000,
            interval: Duration::from_millis(100),
            emissions: vec![PreviewEmission {
                text: "hi".to_owned(),
                recording_s: 1.5,
            }],
            error: None,
        };
        let v: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(v["kind"], "preview_self_test");
        assert_eq!(v["ok"], true);
        assert_eq!(v["frames_pushed"], 5);
        assert_eq!(v["frame_samples"], 24_000);
        assert_eq!(v["sample_rate"], 16_000);
        assert_eq!(v["interval_ms"], 100);
        assert_eq!(v["emissions"][0]["text"], "hi");
        assert_eq!(v["emissions"][0]["recording_s"], 1.5);
    }
}
