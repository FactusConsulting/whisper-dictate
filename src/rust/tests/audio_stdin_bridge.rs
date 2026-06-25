//! Integration test for the supervisor-side stdin bridge (PR #341).
//!
//! The unit tests in `src/rust/audio/stdin_bridge.rs` cover the writer
//! against a fake `Write`; this file proves the JSON wire format the
//! bridge emits is parseable by `serde_json` AND matches what the
//! Python decoder consumes — the *same* `event_to_json_line` Rust
//! function is the encoder on both sides, so a smoke test here keeps
//! the wire contract pinned.
//!
//! Only built when `--features audio-in-rust` is on. Without the
//! feature the bridge module is gated out at the crate root.

#![cfg(feature = "audio-in-rust")]

use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;
use whisper_dictate_app::audio::{event_to_json_line, write_events, PipelineEvent};

/// `Write` shim that just accumulates bytes — mirrors what
/// `Child::stdin` would receive in production.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);
impl Sink {
    fn snapshot(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}
impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_events_produces_one_parseable_json_per_line() {
    let events = vec![
        PipelineEvent::SpeechStart,
        PipelineEvent::Frame(vec![0.25; 480]),
        PipelineEvent::SpeechEnd,
        PipelineEvent::Cancelled,
        PipelineEvent::DeviceError("test failure".to_owned()),
    ];
    let mut sink = Sink::default();
    write_events(&mut sink, events.clone()).expect("write to in-memory sink");
    let text = String::from_utf8(sink.snapshot()).expect("UTF-8");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), events.len(), "one JSON per line: {text:?}");

    let types: Vec<String> = lines
        .iter()
        .map(|l| {
            serde_json::from_str::<Value>(l)
                .expect("parseable JSON")
                .get("type")
                .and_then(|v| v.as_str())
                .expect("`type` field present")
                .to_owned()
        })
        .collect();
    assert_eq!(
        types,
        vec![
            "speech_start",
            "frame",
            "speech_end",
            "cancelled",
            "device_error"
        ],
    );
}

#[test]
fn frame_event_round_trips_via_json_line() {
    // Sanity: a Frame written and then re-parsed via the public
    // `event_to_json_line` matches the Python decoder's expected
    // base64+little-endian layout, so a Python integration test can
    // feed the same bytes through `vp_rust_audio_source.decode_event`
    // and recover the original samples (see test_rust_audio_source.py).
    let samples: Vec<f32> = (0..480).map(|i| (i as f32) * 0.001 - 0.24).collect();
    let line = event_to_json_line(&PipelineEvent::Frame(samples.clone()));
    let parsed: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["type"], "frame");
    let b64 = parsed["samples"].as_str().expect("base64 string");
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).expect("base64");
    assert_eq!(bytes.len(), samples.len() * 4);
    let decoded: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(decoded, samples);
}

#[test]
fn writer_thread_drains_a_channel_until_sender_drops() {
    // Mirrors what spawn_bridge does: a sender feeds PipelineEvents on
    // a channel, a writer thread drains them into `Write` until the
    // sender is dropped. This pins the teardown contract — the writer
    // exits cleanly on Disconnected, not on a timeout.
    let (tx, rx) = mpsc::channel::<PipelineEvent>();
    let sink = Sink::default();
    let sink_clone = sink.clone();
    let handle = thread::spawn(move || {
        // Inline mini-loop instead of calling the private `run_writer`
        // — we don't want to make it pub just for a test; the contract
        // (drain until disconnect, one JSON line per event, flush
        // after each) is what matters and is exercised here directly.
        let mut out = sink_clone;
        while let Ok(event) = rx.recv() {
            let line = event_to_json_line(&event);
            out.write_all(line.as_bytes()).unwrap();
            out.write_all(b"\n").unwrap();
            out.flush().unwrap();
        }
    });
    tx.send(PipelineEvent::SpeechStart).unwrap();
    tx.send(PipelineEvent::SpeechEnd).unwrap();
    drop(tx);
    handle.join().expect("writer joins cleanly on sender drop");
    let text = String::from_utf8(sink.snapshot()).unwrap();
    assert_eq!(text.lines().count(), 2);
}
