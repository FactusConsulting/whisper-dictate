//! `cpal`-based microphone capture in a dedicated worker thread.
//!
//! The pipeline downstream of this module wants mono `f32` samples at the
//! device's native rate; resampling to 16 kHz happens later (see
//! [`super::resampler`]). Keeping the capture callback minimal — sample
//! format conversion + channel-average mixdown only — leaves enough
//! headroom on slow USB mics to never drop a buffer.
//!
//! Lifecycle:
//! * [`start_capture`] spawns a worker that opens the chosen device,
//!   negotiates a supported config (priority `F32 > I16 > I32`) at the
//!   device's native rate, and starts the stream.
//! * Each callback converts the device buffer to mono `f32` and pushes a
//!   [`AudioChunk::Samples`] message onto the `mpsc::channel`.
//! * Setting `stop_flag` to `true` triggers the worker to drop the stream
//!   and push a final [`AudioChunk::EndOfStream`] sentinel so the consumer
//!   knows when it's safe to flush the resampler and shut down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};

/// Messages sent from the capture worker to the consumer thread.
#[derive(Debug)]
pub enum AudioChunk {
    /// A burst of mono `f32` samples at the device's native sample rate.
    Samples(Vec<f32>),
    /// The capture loop stopped cleanly. Pushed AFTER all in-flight
    /// `Samples` messages so the consumer can drain them and then flush
    /// the resampler without losing the tail of the recording.
    EndOfStream,
    /// The capture loop hit an unrecoverable error. The consumer should
    /// surface this to the user and tear the pipeline down.
    Error(String),
}

/// Handle to a running capture worker. Drop to stop, or call [`stop`]
/// explicitly to block until the worker has emitted `EndOfStream`.
pub struct CaptureHandle {
    stop_flag: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    /// The native sample rate the worker negotiated, exposed so the
    /// consumer can build the matching [`super::resampler::FrameResampler`].
    sample_rate: u32,
}

impl CaptureHandle {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Signal the worker to stop and wait for it to finish. Idempotent.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start capturing from the named input device. An empty `device_name`
/// selects the system default input. Returns the handle plus the
/// negotiated native sample rate.
///
/// The producer side runs on a dedicated thread because cpal's stream
/// callback is invoked from a high-priority audio thread that we must
/// keep extremely short — we do nothing in the callback other than
/// mix-to-mono + send a `Vec<f32>` over the channel.
pub fn start_capture(
    device_name: &str,
    tx: Sender<AudioChunk>,
) -> Result<CaptureHandle, anyhow::Error> {
    let host = cpal::default_host();
    let device = pick_device(&host, device_name)?;

    let supported = pick_config(&device)?;
    let sample_format = supported.sample_format();
    let channels = supported.channels();
    // cpal 0.18 type-aliased SampleRate to a plain `u32`, so the old
    // tuple-struct `.0` accessor is gone — the call returns the rate
    // directly.
    let sample_rate = supported.sample_rate();
    let config: StreamConfig = supported.into();

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_for_worker = stop_flag.clone();

    let join = thread::spawn(move || {
        // Build + start the stream INSIDE the worker so the cpal Stream is
        // dropped on the same thread it was created on — required by some
        // host backends (WASAPI in particular).
        let tx_for_cb = tx.clone();
        let tx_for_err = tx.clone();
        let build_result = build_input_stream(
            &device,
            config,
            sample_format,
            channels,
            move |samples| {
                let _ = tx_for_cb.send(AudioChunk::Samples(samples));
            },
            move |err| {
                let _ = tx_for_err.send(AudioChunk::Error(format!("cpal stream error: {err}")));
            },
        );
        let stream = match build_result {
            Ok(s) => s,
            Err(err) => {
                let _ = tx.send(AudioChunk::Error(format!("build input stream: {err}")));
                let _ = tx.send(AudioChunk::EndOfStream);
                return;
            }
        };
        if let Err(err) = stream.play() {
            let _ = tx.send(AudioChunk::Error(format!("start stream: {err}")));
            let _ = tx.send(AudioChunk::EndOfStream);
            return;
        }
        // Park-with-poll loop. We don't need precise wake-up — 10 ms is far
        // shorter than the worst-case capture latency on Windows.
        while !stop_for_worker.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
        }
        // Dropping `stream` here stops the stream cleanly.
        drop(stream);
        let _ = tx.send(AudioChunk::EndOfStream);
    });

    Ok(CaptureHandle {
        stop_flag,
        join: Some(join),
        sample_rate,
    })
}

// ----- helpers ----------------------------------------------------------------

/// Outcome of [`resolve_device_index`] — either an index into the
/// enumerated device list or a structured error for the caller to
/// translate.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeviceLookup {
    /// The selector matched the device at this index.
    Matched(usize),
    /// The selector is a numeric index that's outside the device list.
    IndexOutOfRange { wanted: usize, available: usize },
    /// No exact name, substring or numeric index matched.
    NotFound,
}

/// Resolve a device selector against a list of device names. Pure
/// helper so the lookup precedence (exact → substring → numeric index)
/// can be unit-tested without a live cpal host.
///
/// Precedence:
///   1. Empty selector → caller handles default device.
///   2. Exact case-insensitive match — first hit wins.
///   3. Case-insensitive substring match — first hit wins (sounddevice
///      style; matches `vp_devices.py`).
///   4. Trimmed numeric selector → index into `device_names`.
///
/// Returns [`DeviceLookup::Matched`] with the chosen index on success,
/// [`DeviceLookup::IndexOutOfRange`] when a parseable index is past the
/// end of the list, and [`DeviceLookup::NotFound`] otherwise.
pub(crate) fn resolve_device_index(device_names: &[String], selector: &str) -> DeviceLookup {
    let needle = selector.trim().to_lowercase();
    // 1. Exact case-insensitive match wins.
    for (idx, name) in device_names.iter().enumerate() {
        if name.to_lowercase() == needle {
            return DeviceLookup::Matched(idx);
        }
    }
    // 2. Bidirectional substring match, keeping the LONGEST device name — the
    //    same precedence as `crate::devices::find_in` (and Python's
    //    `vp_devices._best_match`), so a truncated / generic `--device` value
    //    (e.g. a Windows MME-truncated endpoint name, or a bare "Microphone")
    //    binds to its fullest sibling rather than to whichever shorter match
    //    happens to enumerate first. Either side may be the prefix. An empty
    //    needle never reaches here in production (`pick_device` maps "" to the
    //    default device first); guard it so it can't spuriously match every
    //    device via the empty-substring rule.
    if !needle.is_empty() {
        let mut best: Option<usize> = None;
        for (idx, name) in device_names.iter().enumerate() {
            let lower = name.to_lowercase();
            if lower.is_empty() || !(lower.contains(&needle) || needle.contains(&lower)) {
                continue;
            }
            match best {
                None => best = Some(idx),
                Some(prev) if name.len() > device_names[prev].len() => best = Some(idx),
                _ => {}
            }
        }
        if let Some(idx) = best {
            return DeviceLookup::Matched(idx);
        }
    }
    // 3. Numeric index fallback (capture-specific; `devices::find_in` has none).
    if let Ok(idx) = selector.trim().parse::<usize>() {
        if idx < device_names.len() {
            return DeviceLookup::Matched(idx);
        }
        return DeviceLookup::IndexOutOfRange {
            wanted: idx,
            available: device_names.len(),
        };
    }
    DeviceLookup::NotFound
}

/// Resolve a CPAL input device by selector string.
///
/// Lookup order (see [`resolve_device_index`] for the pure logic):
///   1. Empty string → the host's default input device.
///   2. Exact (case-insensitive) name match against any enumerated input.
///   3. Case-insensitive substring match against any enumerated input.
///   4. Numeric selector → index into the host's input device list.
fn pick_device(host: &cpal::Host, device_name: &str) -> Result<cpal::Device, anyhow::Error> {
    if device_name.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("no default input device available"));
    }
    // Enumerate once. cpal's iterator can only be walked once on some
    // backends, so we materialise into a Vec before doing the name +
    // numeric passes.
    let devices: Vec<cpal::Device> = host
        .input_devices()
        .map_err(|err| anyhow::anyhow!("enumerate input devices: {err}"))?
        .collect();
    // cpal 0.18 removed `Device::name()` in favour of the `Display`
    // impl + a structured `description()`. `to_string()` is equivalent
    // on every backend.
    let names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
    match resolve_device_index(&names, device_name) {
        DeviceLookup::Matched(idx) => Ok(devices.into_iter().nth(idx).expect("index in range")),
        DeviceLookup::IndexOutOfRange { wanted, available } => Err(anyhow::anyhow!(
            "input device index {wanted} out of range (have {available} input device(s))"
        )),
        DeviceLookup::NotFound => Err(anyhow::anyhow!("input device not found: {device_name:?}")),
    }
}

fn pick_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, anyhow::Error> {
    // Priority F32 > I16 > I32. We always pick the device's native rate
    // (max_sample_rate of the supported config) and resample later.
    let mut best_f32: Option<cpal::SupportedStreamConfigRange> = None;
    let mut best_i16: Option<cpal::SupportedStreamConfigRange> = None;
    let mut best_i32: Option<cpal::SupportedStreamConfigRange> = None;

    let supported = device
        .supported_input_configs()
        .map_err(|err| anyhow::anyhow!("supported_input_configs: {err}"))?;
    for cfg in supported {
        match cfg.sample_format() {
            SampleFormat::F32 => best_f32 = Some(cfg),
            SampleFormat::I16 => best_i16 = Some(cfg),
            SampleFormat::I32 => best_i32 = Some(cfg),
            _ => {}
        }
    }
    let picked = best_f32
        .or(best_i16)
        .or(best_i32)
        .ok_or_else(|| anyhow::anyhow!("no F32/I16/I32 input config supported"))?;
    // Pick the highest natively-supported rate within the range.
    Ok(picked.with_max_sample_rate())
}

fn build_input_stream<F, E>(
    device: &cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    channels: u16,
    on_samples: F,
    on_error: E,
) -> Result<cpal::Stream, anyhow::Error>
where
    F: FnMut(Vec<f32>) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    // We're paranoid about the audio callback: wrap the user-supplied
    // `on_samples` in a closure that owns the (cheap) mix-to-mono.
    let channels_usize = channels as usize;
    let on_samples = std::sync::Mutex::new(on_samples);
    let on_samples = std::sync::Arc::new(on_samples);

    macro_rules! callback_for {
        ($sample_ty:ty, $to_f32:expr) => {{
            let on_samples = on_samples.clone();
            move |buffer: &[$sample_ty], _: &cpal::InputCallbackInfo| {
                let mono = mix_to_mono(buffer, channels_usize, $to_f32);
                if let Ok(mut cb) = on_samples.lock() {
                    cb(mono);
                }
            }
        }};
    }

    // cpal 0.18 unified the stream/build errors under a single
    // `cpal::Error` (the old `StreamError` was removed); the callback
    // signature is `FnMut(cpal::Error)` now.
    let on_error = std::sync::Mutex::new(on_error);
    let on_error = std::sync::Arc::new(on_error);
    let err_cb = move |err: cpal::Error| {
        if let Ok(mut cb) = on_error.lock() {
            cb(err);
        }
    };

    // cpal 0.18 takes `StreamConfig` by value (not by ref) and adds an
    // explicit `timeout: Option<Duration>` arg. `None` matches the prior
    // "block indefinitely until the device opens" semantics.
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
            config,
            callback_for!(f32, |s: f32| s),
            err_cb,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream::<i16, _, _>(
            config,
            callback_for!(i16, |s: i16| (s as f32) / (i16::MAX as f32)),
            err_cb,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream::<i32, _, _>(
            config,
            callback_for!(i32, |s: i32| (s as f32) / (i32::MAX as f32)),
            err_cb,
            None,
        ),
        other => {
            return Err(anyhow::anyhow!(
                "unsupported sample format negotiated: {other:?}"
            ));
        }
    };
    stream.map_err(|err| anyhow::anyhow!("build_input_stream: {err}"))
}

/// Channel-average mix to mono. Pure / no `cfg` so it can be unit tested
/// on every build. The `to_f32` closure normalises integer samples into
/// the `[-1.0, 1.0]` range and is a no-op for native f32 buffers.
pub fn mix_to_mono<T, F>(buffer: &[T], channels: usize, to_f32: F) -> Vec<f32>
where
    T: Copy,
    F: Fn(T) -> f32,
{
    if channels <= 1 {
        return buffer.iter().copied().map(&to_f32).collect();
    }
    let frames = buffer.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for frame_idx in 0..frames {
        let start = frame_idx * channels;
        let mut sum = 0.0_f32;
        for ch in 0..channels {
            sum += to_f32(buffer[start + ch]);
        }
        out.push(sum / channels as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_passthrough_keeps_samples_unchanged() {
        let buf: Vec<f32> = vec![0.1, -0.2, 0.3, -0.4];
        let out = mix_to_mono(&buf, 1, |s: f32| s);
        assert_eq!(out, buf);
    }

    #[test]
    fn stereo_is_averaged_per_frame() {
        // Interleaved L, R, L, R, ...
        let buf: Vec<f32> = vec![0.1, 0.3, -0.4, 0.4];
        let out = mix_to_mono(&buf, 2, |s: f32| s);
        // Frame 0: (0.1 + 0.3) / 2 = 0.2
        // Frame 1: (-0.4 + 0.4) / 2 = 0.0
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.2).abs() < 1e-6);
        assert!(out[1].abs() < 1e-6);
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_device_index_prefers_exact_match_over_substring() {
        let devs = names(&["Realtek HD Audio Mic", "Realtek Mic"]);
        // "realtek mic" exactly matches index 1 (case-insensitive),
        // even though it'd also substring-match index 0.
        assert_eq!(
            resolve_device_index(&devs, "realtek mic"),
            DeviceLookup::Matched(1)
        );
    }

    #[test]
    fn resolve_device_index_substring_match_when_no_exact() {
        let devs = names(&["Realtek HD Audio Mic", "Webcam Mic"]);
        assert_eq!(
            resolve_device_index(&devs, "webcam"),
            DeviceLookup::Matched(1)
        );
    }

    #[test]
    fn resolve_device_index_prefers_longest_bidirectional_substring() {
        // Two generic endpoints both substring-match "usb mic"; the resolver
        // must bind to the LONGEST name (index 1), not the first enumeration
        // hit — matching `devices::find_in` and the Python `_best_match`.
        let devs = names(&["USB Mic (Front)", "USB Mic (Rear Panel Connector)"]);
        assert_eq!(
            resolve_device_index(&devs, "usb mic"),
            DeviceLookup::Matched(1)
        );
        // Truncated saved value (Windows MME caps names at 31 chars): the
        // selector is a prefix of the fuller device name AND a superstring of
        // the bare "Microphone" — the bidirectional match must still bind to
        // the fullest sibling (index 1), not the shorter generic one.
        let devs = names(&["Microphone", "Microphone (High Definition Audio)"]);
        assert_eq!(
            resolve_device_index(&devs, "Microphone (High Definition"),
            DeviceLookup::Matched(1)
        );
    }

    #[test]
    fn resolve_device_index_numeric_selector_indexes_into_list() {
        let devs = names(&["Mic A", "Mic B", "Mic C"]);
        // No name "2" exists, so we fall through to the numeric pass.
        assert_eq!(resolve_device_index(&devs, "2"), DeviceLookup::Matched(2));
        // Leading/trailing whitespace is trimmed before parsing.
        assert_eq!(resolve_device_index(&devs, " 1 "), DeviceLookup::Matched(1));
    }

    #[test]
    fn resolve_device_index_numeric_selector_out_of_range_returns_error() {
        let devs = names(&["Mic A", "Mic B"]);
        assert_eq!(
            resolve_device_index(&devs, "7"),
            DeviceLookup::IndexOutOfRange {
                wanted: 7,
                available: 2
            }
        );
    }

    #[test]
    fn resolve_device_index_numeric_substring_match_wins_over_index_fallback() {
        // If a device literally has "2" in its name, substring match
        // catches it before we'd try the numeric index path.
        let devs = names(&["Mic 2 — USB", "Mic A", "Mic B"]);
        assert_eq!(resolve_device_index(&devs, "2"), DeviceLookup::Matched(0));
    }

    #[test]
    fn resolve_device_index_unknown_selector_returns_not_found() {
        let devs = names(&["Mic A"]);
        assert_eq!(
            resolve_device_index(&devs, "nonexistent"),
            DeviceLookup::NotFound
        );
    }

    #[test]
    fn integer_mixdown_normalises_to_unit_range() {
        // 4 frames stereo @ i16: full-positive on L, full-negative on R.
        let buf: Vec<i16> = vec![i16::MAX, i16::MIN, i16::MAX, i16::MIN];
        let out = mix_to_mono(&buf, 2, |s: i16| (s as f32) / (i16::MAX as f32));
        // Per frame: (1.0 + ~-1.0) / 2 ≈ 0.0 (off by 1 LSB on i16 MIN).
        for &s in &out {
            assert!(s.abs() < 0.001, "frame mixed to ~0 but got {s}");
        }
    }
}
