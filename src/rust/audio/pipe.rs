//! JSON-line serializer that bridges [`super::PipelineEvent`] to the
//! Python worker's stdin under ``--audio-source=rust-stdin``.
//!
//! The wire format mirrors `src/python/whisper_dictate/vp_rust_audio_source.py`
//! exactly. Each event is one JSON object per line; frames carry their
//! 480 little-endian f32 samples base64-encoded so the payload stays
//! compact (~2.5 KB instead of ~10 KB of decimal numbers) without binary
//! framing on the pipe.
//!
//! Kept narrow so it can be unit-tested without spinning up a Python
//! subprocess or a real cpal stream.

use std::io::{self, Write};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde_json::json;

use super::PipelineEvent;

/// Serialize one pipeline event as a single JSON line **without** the
/// trailing newline. The caller is responsible for the newline so the
/// stdin writer can batch writes if it wants to.
pub fn event_to_json_line(event: &PipelineEvent) -> String {
    match event {
        PipelineEvent::SpeechStart => json!({ "type": "speech_start" }).to_string(),
        PipelineEvent::SpeechEnd => json!({ "type": "speech_end" }).to_string(),
        PipelineEvent::DeviceError(message) => {
            json!({ "type": "device_error", "message": message }).to_string()
        }
        PipelineEvent::Frame(samples) => {
            // Pack f32s into little-endian bytes (matches `<f4` in numpy).
            // We allocate once for the byte buffer and once for the base64
            // string; not perf-critical at 30 ms cadence.
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let encoded = BASE64_STANDARD.encode(&bytes);
            json!({ "type": "frame", "samples": encoded }).to_string()
        }
    }
}

/// Stream events into `out` as one JSON line each, flushing after every
/// event so the Python worker sees them immediately. Returns the first
/// IO error so the supervisor can tear the pipeline down — typically
/// `BrokenPipe` when the worker exits.
pub fn write_events<W, I>(out: &mut W, events: I) -> io::Result<()>
where
    W: Write,
    I: IntoIterator<Item = PipelineEvent>,
{
    for event in events {
        let line = event_to_json_line(&event);
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    #[test]
    fn speech_start_serializes_to_known_json() {
        let line = event_to_json_line(&PipelineEvent::SpeechStart);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["type"], "speech_start");
        assert_eq!(parsed.as_object().unwrap().len(), 1);
    }

    #[test]
    fn speech_end_serializes_to_known_json() {
        let line = event_to_json_line(&PipelineEvent::SpeechEnd);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["type"], "speech_end");
    }

    #[test]
    fn device_error_preserves_message() {
        let line = event_to_json_line(&PipelineEvent::DeviceError("no mic".into()));
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["type"], "device_error");
        assert_eq!(parsed["message"], "no mic");
    }

    #[test]
    fn frame_samples_round_trip_through_base64_little_endian() {
        let samples: Vec<f32> = (0..480).map(|i| (i as f32) * 0.001 - 0.24).collect();
        let line = event_to_json_line(&PipelineEvent::Frame(samples.clone()));
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(parsed["type"], "frame");
        let encoded = parsed["samples"].as_str().expect("samples is a string");
        let bytes = BASE64_STANDARD.decode(encoded).expect("base64 round-trip");
        assert_eq!(bytes.len(), samples.len() * 4);
        // Decode the bytes back to f32 little-endian and check exact equality.
        let mut decoded = Vec::with_capacity(samples.len());
        for chunk in bytes.chunks_exact(4) {
            decoded.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        assert_eq!(decoded, samples);
    }

    #[test]
    fn write_events_appends_newlines_per_event() {
        let mut out: Vec<u8> = Vec::new();
        write_events(
            &mut out,
            vec![PipelineEvent::SpeechStart, PipelineEvent::SpeechEnd],
        )
        .expect("write to Vec never fails");
        let text = String::from_utf8(out).expect("UTF-8 output");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("speech_start"));
        assert!(lines[1].contains("speech_end"));
    }
}
