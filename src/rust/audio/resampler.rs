//! Fixed-frame resampler that converts arbitrary-rate mono audio into the
//! 30 ms / 480-sample frames at 16 kHz that the Silero VAD and downstream
//! consumers expect.
//!
//! Why a wrapper around `rubato::Fft` (with `FixedSync::Input`):
//! * The capture callback gives us bursts of mono samples whose size depends
//!   on the device buffer (CPAL doesn't promise a fixed callback size, and on
//!   Windows WASAPI it's commonly 480–960 frames at 48 kHz). We need exactly
//!   480-sample frames at 16 kHz to feed Silero, regardless of what the
//!   device hands us.
//! * `Fft` with `FixedSync::Input` does the actual rate conversion but it
//!   expects a fixed *input* chunk size per call and produces a variable
//!   output chunk size (in rubato 3.x/earlier this was a separate type named
//!   `FftFixedIn`; rubato 4.x merged them behind the `FixedSync` selector).
//!   This wrapper buffers partial input until we have a full input chunk,
//!   then chops the resampler's output into fixed 480-sample 30 ms frames.
//! * On stop we call [`FrameResampler::finish`] which zero-pads any leftover
//!   audio so the last partial frame is still emitted — otherwise the VAD
//!   could lose up to ~30 ms of trailing speech at the end of an utterance.
//!
//! The design is deliberately tiny and callback-based so it can be unit
//! tested without any audio device or threading.

use rubato::audioadapter_buffers::direct::{SequentialSlice, SequentialSliceOfVecs};
use rubato::{Fft, FixedSync, Resampler, WindowFunction};

/// Output sample rate fed to Silero. 16 kHz is what the bundled
/// Silero v4 ONNX model expects.
pub const OUTPUT_RATE: usize = 16_000;
/// Output samples per emitted frame. 30 ms × 16 kHz = 480 samples.
///
/// Note: the bundled Silero **v4** ONNX model documents 512-sample
/// windows at 16 kHz as its supported window size; we feed 480-sample
/// (30 ms) frames instead. `vad-rs` 0.1.5 accepts dynamic ONNX input
/// shapes so this works empirically, but voice-probability accuracy
/// MAY degrade on unsupported window sizes. Calibration on the
/// synthetic 1 s silence + 1 s loud-sine test in
/// `src/rust/tests/audio_pipeline.rs` looks sensible; if real-world
/// recall regresses we should buffer to 512 samples before calling
/// `inner.compute` (still emitting one VAD decision per 30 ms frame)
/// rather than enlarge FRAME_SIZE — the pipe protocol pins FRAME_SIZE
/// to 480 on both ends.
///
/// TODO(audio-in-rust): review on next vad-rs / Silero upgrade — if the
/// upstream model adds a documented 480-sample mode this comment can be
/// dropped. See PR #335 review finding #3.
pub const FRAME_SIZE: usize = 480;
/// Fixed input chunk size handed to `Fft`. 1024 is a good balance
/// between resampler latency (≈ 21 ms at 48 kHz input) and FFT efficiency.
pub const INPUT_CHUNK: usize = 1024;

/// Mono resampler that emits exactly [`FRAME_SIZE`]-sample frames at
/// [`OUTPUT_RATE`] via a `push(&[f32], callback)` API.
pub struct FrameResampler {
    inner: Fft<f32>,
    input_rate: usize,
    /// Pending raw input samples that haven't filled an [`INPUT_CHUNK`] yet.
    input_buffer: Vec<f32>,
    /// Pending resampled samples that haven't filled a [`FRAME_SIZE`]-sample
    /// output frame yet.
    output_buffer: Vec<f32>,
    /// Pre-allocated scratch handed to `Fft::process_into_buffer` every
    /// call. Sized at construction so we never reallocate on the audio
    /// thread. Shape: one inner vec per channel (we're mono, so exactly
    /// one), each vec pre-sized to `output_frames_max()`. Wrapped in a
    /// [`SequentialSliceOfVecs`] adapter at call time.
    process_scratch: Vec<Vec<f32>>,
    /// Whether any non-empty input has been pushed since the last
    /// [`finish`] (or construction). Used as the [`finish`] guard so we
    /// only flush the FFT tail when there's something to flush. The old
    /// guard inspected `input_buffer.len()` + `output_buffer.len()`,
    /// which is fragile: `drain_full_frames` always leaves
    /// `output_buffer` at `< FRAME_SIZE`, so the relevant invariant
    /// could silently flip under a future refactor and lose the tail
    /// without an error.
    fed_any: bool,
}

impl FrameResampler {
    /// Build a resampler that converts mono `input_rate` Hz audio to mono
    /// [`OUTPUT_RATE`] in fixed [`FRAME_SIZE`]-sample frames.
    pub fn new(input_rate: usize) -> Result<Self, rubato::ResamplerConstructionError> {
        // rubato 4.x `Fft::new_custom` args:
        //   (fs_in, fs_out, chunk_size, sub_chunks, nbr_channels, window, fixed).
        // We pin `sub_chunks = 1` (vanilla FFT, no chunked decimation) and
        // `WindowFunction::BlackmanHarris2` so we get the exact same
        // frequency response the pre-v4 `FftFixedIn::new(_, _, _, 1, 1)`
        // gave us — `Fft::new` would have picked `sub_chunks = chunk_size /
        // 256 = 4` instead, which would be a silent behavioural change on
        // this path. `FixedSync::Input` fixes the input chunk size at
        // INPUT_CHUNK and lets the output size vary, matching the old
        // "FixedIn" contract. 1 channel because we already downmix to mono
        // inside the capture callback.
        let inner = Fft::<f32>::new_custom(
            input_rate,
            OUTPUT_RATE,
            INPUT_CHUNK,
            1,
            1,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )?;
        // Allocate the per-call output buffer once. `output_frames_max` is the
        // worst-case output count from a single `process_into_buffer` call,
        // which is what we need to size the scratch vec.
        let scratch_len = inner.output_frames_max();
        let process_scratch = vec![vec![0.0_f32; scratch_len]; 1];
        Ok(Self {
            inner,
            input_rate,
            input_buffer: Vec::with_capacity(INPUT_CHUNK * 2),
            output_buffer: Vec::with_capacity(FRAME_SIZE * 2),
            process_scratch,
            fed_any: false,
        })
    }

    /// Configured input sample rate in Hz.
    pub fn input_rate(&self) -> usize {
        self.input_rate
    }

    /// Push a slice of mono samples. The callback is invoked once per
    /// emitted 30 ms / [`FRAME_SIZE`]-sample frame at 16 kHz, in order. The
    /// callback receives a borrow of the internal frame buffer to avoid an
    /// allocation per frame — clone it if you need to keep the data.
    pub fn push<F>(&mut self, samples: &[f32], mut on_frame: F)
    where
        F: FnMut(&[f32]),
    {
        if samples.is_empty() {
            return;
        }
        self.fed_any = true;
        self.input_buffer.extend_from_slice(samples);

        // Resample as many full input chunks as we currently hold.
        while self.input_buffer.len() >= INPUT_CHUNK {
            // Drain the first INPUT_CHUNK samples out of input_buffer into a
            // scratch input vec. rubato 4.x wants an `Adapter`-shaped
            // buffer; for a mono flat slice, `SequentialSlice` is the
            // zero-copy wrapper.
            let chunk: Vec<f32> = self.input_buffer.drain(..INPUT_CHUNK).collect();

            // Process into the pre-allocated scratch and append the produced
            // samples onto our pending output buffer. Adapters are built in
            // an inner block so the `&mut self.process_scratch` borrow is
            // released before we re-borrow it immutably to copy out the
            // produced samples.
            let scratch_len = self.process_scratch[0].len();
            let result = {
                let input = SequentialSlice::new(&chunk, 1, INPUT_CHUNK)
                    .expect("mono input adapter, chunk pre-sized to INPUT_CHUNK");
                let mut output =
                    SequentialSliceOfVecs::new_mut(&mut self.process_scratch, 1, scratch_len)
                        .expect("mono scratch adapter, pre-sized to output_frames_max");
                self.inner.process_into_buffer(&input, &mut output, None)
            };
            if let Ok((_in_len, out_len)) = result {
                self.output_buffer
                    .extend_from_slice(&self.process_scratch[0][..out_len]);
            }

            // Emit complete 480-sample frames out of the output buffer.
            self.drain_full_frames(&mut on_frame);
        }
    }

    /// Drain any leftover audio at end-of-stream: pad the trailing partial
    /// input chunk with zeros so the resampler emits its final partial
    /// output, then zero-pad the last output frame so the VAD sees a final
    /// full [`FRAME_SIZE`]-sample frame instead of dropping up to ~30 ms of
    /// trailing speech.
    pub fn finish<F>(&mut self, mut on_frame: F)
    where
        F: FnMut(&[f32]),
    {
        // Reset `fed_any` first so a second call to finish() (or re-use
        // after re-feeding) starts from a clean "nothing pushed yet"
        // state. We capture the pre-reset value below to decide whether
        // we have anything to flush.
        let had_input = self.fed_any;
        self.fed_any = false;
        if !had_input {
            // Nothing was ever pushed since the last finish/construction
            // — no tail to flush. Bail BEFORE inflating the input buffer
            // with two silence chunks, otherwise we'd emit a frame of
            // pure silence as the "tail" of zero real input.
            return;
        }
        // FftFixedIn buffers input internally — a single full-INPUT_CHUNK
        // process call doesn't always emit anything (the FFT operates on
        // `fft_size_in` which can be slightly larger than our INPUT_CHUNK,
        // e.g. 1026 vs 1024 for 48 kHz → 16 kHz). To drain a leftover
        // partial input and force the internal saved frames out, we
        // push two zero chunks: the first pads our partial input up to
        // INPUT_CHUNK and the second is pure silence to flush the FFT
        // buffer. Both add only synthetic zeros AFTER the real signal
        // ended, so no real samples are lost or duplicated.
        // First flush chunk: pad pending input up to INPUT_CHUNK with
        // zeros. `resize` is the unified idiom for both the pending > 0
        // case (where it extends from the current length) and the
        // pending == 0 case (where it grows from 0).
        self.input_buffer.resize(INPUT_CHUNK, 0.0);
        self.drain_one_chunk();
        // Second flush chunk: pure-silence buffer to push the FFT's
        // internal saved frames through to the output.
        self.input_buffer.resize(INPUT_CHUNK, 0.0);
        self.drain_one_chunk();

        // Emit every complete frame we now have.
        self.drain_full_frames(&mut on_frame);

        // Zero-pad the remainder, if any, so the final partial frame is
        // emitted at full FRAME_SIZE.
        if !self.output_buffer.is_empty() {
            self.output_buffer.resize(FRAME_SIZE, 0.0);
            on_frame(&self.output_buffer[..FRAME_SIZE]);
            self.output_buffer.clear();
        }
    }

    /// Drain one INPUT_CHUNK from `input_buffer` through the resampler,
    /// appending the produced samples to `output_buffer`. Used only by
    /// [`finish`] when flushing the tail; the steady-state path inlines
    /// the same logic in [`push`].
    fn drain_one_chunk(&mut self) {
        if self.input_buffer.len() < INPUT_CHUNK {
            return;
        }
        let chunk: Vec<f32> = self.input_buffer.drain(..INPUT_CHUNK).collect();
        let scratch_len = self.process_scratch[0].len();
        let result = {
            let input = SequentialSlice::new(&chunk, 1, INPUT_CHUNK)
                .expect("mono input adapter, chunk pre-sized to INPUT_CHUNK");
            let mut output =
                SequentialSliceOfVecs::new_mut(&mut self.process_scratch, 1, scratch_len)
                    .expect("mono scratch adapter, pre-sized to output_frames_max");
            self.inner.process_into_buffer(&input, &mut output, None)
        };
        if let Ok((_in_len, out_len)) = result {
            self.output_buffer
                .extend_from_slice(&self.process_scratch[0][..out_len]);
        }
    }

    fn drain_full_frames<F>(&mut self, on_frame: &mut F)
    where
        F: FnMut(&[f32]),
    {
        while self.output_buffer.len() >= FRAME_SIZE {
            on_frame(&self.output_buffer[..FRAME_SIZE]);
            self.output_buffer.drain(..FRAME_SIZE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn collect_frames(sample_rate: usize, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut resampler = FrameResampler::new(sample_rate).expect("construct resampler");
        let mut frames = Vec::new();
        resampler.push(samples, |frame| frames.push(frame.to_vec()));
        frames
    }

    fn sine(sample_rate: usize, freq_hz: f32, duration_s: f32) -> Vec<f32> {
        let total = (sample_rate as f32 * duration_s) as usize;
        (0..total)
            .map(|i| (2.0 * PI * freq_hz * (i as f32) / (sample_rate as f32)).sin())
            .collect()
    }

    #[test]
    fn frames_are_fixed_480_at_16khz() {
        // 2 s of 1 kHz sine at 44.1 kHz → ~32 kHz of output → ~66 frames of 480.
        let input = sine(44_100, 1_000.0, 2.0);
        let frames = collect_frames(44_100, &input);
        assert!(!frames.is_empty(), "expected at least one frame");
        for frame in &frames {
            assert_eq!(frame.len(), FRAME_SIZE, "every frame must be FRAME_SIZE");
        }
        // 2 s of input → ~32 000 output samples → 66 frames + change of 480.
        // Allow ±2 frames of slack for FFT chunk boundaries.
        let expected = (2 * OUTPUT_RATE) / FRAME_SIZE; // 66
        assert!(
            (frames.len() as isize - expected as isize).abs() <= 2,
            "expected ~{expected} frames, got {}",
            frames.len()
        );
    }

    #[test]
    fn finish_emits_zero_padded_last_frame() {
        // Push less than one full output frame's worth of audio, then finish.
        // The pre-finish push should emit ZERO complete frames, and finish()
        // should emit at least one zero-padded final frame.
        let mut resampler = FrameResampler::new(48_000).expect("construct");
        let tiny: Vec<f32> = (0..200).map(|i| (i as f32) * 0.001).collect();
        let mut mid_frames = 0_usize;
        resampler.push(&tiny, |_| mid_frames += 1);
        assert_eq!(
            mid_frames, 0,
            "200 input samples at 48 kHz should not fill a 480-sample 16 kHz frame"
        );

        let mut final_frames: Vec<Vec<f32>> = Vec::new();
        resampler.finish(|frame| final_frames.push(frame.to_vec()));
        assert!(
            !final_frames.is_empty(),
            "finish() must emit at least one frame"
        );
        let last = final_frames.last().expect("at least one frame");
        assert_eq!(last.len(), FRAME_SIZE, "padded final frame is FRAME_SIZE");
        // The TAIL of the last frame must be exact zeros (the synthetic
        // zero-padding we appended on finish()).
        let tail = &last[last.len() - 16..];
        for &s in tail {
            assert_eq!(
                s, 0.0,
                "trailing samples in the padded final frame are zero"
            );
        }
    }

    #[test]
    fn finish_emits_no_frames_when_nothing_was_pushed() {
        // Regression guard: the old `if pending > 0 || output_buffer.len()
        // < FRAME_SIZE` guard was structurally "always true after any
        // construction" (output_buffer starts at 0 < FRAME_SIZE), so it
        // would try to flush even from a virgin resampler. With `fed_any`
        // we bail BEFORE inflating the input buffer with synthetic silence
        // so finish() on a never-pushed resampler is a no-op.
        let mut resampler = FrameResampler::new(48_000).expect("construct");
        let mut frames = 0_usize;
        resampler.finish(|_| frames += 1);
        assert_eq!(
            frames, 0,
            "finish() with no prior push() must not emit synthetic-silence frames"
        );
    }

    #[test]
    fn finish_flushes_tail_when_output_buffer_would_be_frame_aligned() {
        // Drive enough audio through that after the steady-state push
        // loop, the FFT has emitted at least one full INPUT_CHUNK worth
        // of output samples — multiple output frames get drained out
        // mid-push, leaving output_buffer at some `< FRAME_SIZE` length
        // (the documented invariant of drain_full_frames). The old
        // finish() guard happened to be equivalent to "output_buffer is
        // strictly less than FRAME_SIZE", which is structurally always
        // true at this point — so a future refactor that left
        // output_buffer at exactly FRAME_SIZE would silently make
        // finish() a no-op. With `fed_any` the guard is "did we ever
        // see real input", which still triggers regardless of internal
        // buffer state.
        let mut resampler = FrameResampler::new(48_000).expect("construct");
        // 2 INPUT_CHUNK's worth = 2048 samples at 48 kHz → ~682 samples at
        // 16 kHz → at least one full 480-sample frame emitted mid-push.
        let input: Vec<f32> = (0..2048).map(|i| (i as f32) * 1e-4).collect();
        let mut mid_frames = 0_usize;
        resampler.push(&input, |_| mid_frames += 1);
        // We may or may not have emitted a frame mid-push depending on
        // FFT chunk boundaries; either way the invariant is the same:
        // finish() must still flush whatever tail remains (possibly
        // padded).
        let mut tail_frames = 0_usize;
        resampler.finish(|frame| {
            assert_eq!(frame.len(), FRAME_SIZE);
            tail_frames += 1;
        });
        assert!(
            mid_frames + tail_frames > 0,
            "between push() + finish() at least one frame must be emitted"
        );
        // A second finish() with no intervening push must be a no-op
        // (fed_any was reset).
        let mut after_reset = 0_usize;
        resampler.finish(|_| after_reset += 1);
        assert_eq!(
            after_reset, 0,
            "finish() after finish() with no new push must not re-emit"
        );
    }

    #[test]
    fn passthrough_at_native_rate_preserves_total_samples_approximately() {
        // At 16 kHz input → 16 kHz output, output should match input in total
        // sample count to within a small FFT boundary tolerance.
        let input = sine(16_000, 440.0, 1.0); // 16 000 samples
        let frames = collect_frames(16_000, &input);
        let total: usize = frames.iter().map(Vec::len).sum();
        let diff = (total as isize - input.len() as isize).abs();
        assert!(
            diff < FRAME_SIZE as isize * 2,
            "got {total} vs input {}",
            input.len()
        );
    }
}
