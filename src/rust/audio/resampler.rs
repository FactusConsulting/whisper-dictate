//! Fixed-frame resampler that converts arbitrary-rate mono audio into the
//! 30 ms / 480-sample frames at 16 kHz that the Silero VAD and downstream
//! consumers expect.
//!
//! Why a wrapper around `rubato::FftFixedIn`:
//! * The capture callback gives us bursts of mono samples whose size depends
//!   on the device buffer (CPAL doesn't promise a fixed callback size, and on
//!   Windows WASAPI it's commonly 480–960 frames at 48 kHz). We need exactly
//!   480-sample frames at 16 kHz to feed Silero, regardless of what the
//!   device hands us.
//! * `FftFixedIn` does the actual rate conversion but it expects a fixed
//!   *input* chunk size per call and produces a variable output chunk size.
//!   This wrapper buffers partial input until we have a full input chunk,
//!   then chops the resampler's output into fixed 480-sample 30 ms frames.
//! * On stop we call [`FrameResampler::finish`] which zero-pads any leftover
//!   audio so the last partial frame is still emitted — otherwise the VAD
//!   could lose up to ~30 ms of trailing speech at the end of an utterance.
//!
//! The design is deliberately tiny and callback-based so it can be unit
//! tested without any audio device or threading.

use rubato::{FftFixedIn, Resampler};

/// Output sample rate fed to Silero. 16 kHz is what the v5 ONNX model expects.
pub const OUTPUT_RATE: usize = 16_000;
/// Output samples per emitted frame. 30 ms × 16 kHz = 480 samples.
pub const FRAME_SIZE: usize = 480;
/// Fixed input chunk size handed to `FftFixedIn`. 1024 is a good balance
/// between resampler latency (≈ 21 ms at 48 kHz input) and FFT efficiency.
pub const INPUT_CHUNK: usize = 1024;

/// Mono resampler that emits exactly [`FRAME_SIZE`]-sample frames at
/// [`OUTPUT_RATE`] via a `push(&[f32], callback)` API.
pub struct FrameResampler {
    inner: FftFixedIn<f32>,
    input_rate: usize,
    /// Pending raw input samples that haven't filled an [`INPUT_CHUNK`] yet.
    input_buffer: Vec<f32>,
    /// Pending resampled samples that haven't filled a [`FRAME_SIZE`]-sample
    /// output frame yet.
    output_buffer: Vec<f32>,
    /// Pre-allocated scratch handed to `FftFixedIn::process_into_buffer`
    /// every call. Sized at construction so we never reallocate on the audio
    /// thread.
    process_scratch: Vec<Vec<f32>>,
}

impl FrameResampler {
    /// Build a resampler that converts mono `input_rate` Hz audio to mono
    /// [`OUTPUT_RATE`] in fixed [`FRAME_SIZE`]-sample frames.
    pub fn new(input_rate: usize) -> Result<Self, rubato::ResamplerConstructionError> {
        // FftFixedIn args: input_rate, output_rate, chunk_size_in, sub_chunks, channels
        // 1 sub-chunk = vanilla FFT resampling (no chunked decimation). 1 channel
        // because we already downmix to mono inside the capture callback.
        let inner = FftFixedIn::<f32>::new(input_rate, OUTPUT_RATE, INPUT_CHUNK, 1, 1)?;
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
        self.input_buffer.extend_from_slice(samples);

        // Resample as many full input chunks as we currently hold.
        while self.input_buffer.len() >= INPUT_CHUNK {
            // Drain the first INPUT_CHUNK samples out of input_buffer into a
            // scratch input vec. We use the canonical Vec<Vec<f32>> shape
            // rubato wants (one inner vec per channel; we're mono).
            let chunk: Vec<f32> = self.input_buffer.drain(..INPUT_CHUNK).collect();
            let in_buffers: [&[f32]; 1] = [&chunk];

            // Process into the pre-allocated scratch and append the produced
            // samples onto our pending output buffer.
            if let Ok((_in_len, out_len)) =
                self.inner
                    .process_into_buffer(&in_buffers, &mut self.process_scratch, None)
            {
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
        // FftFixedIn buffers input internally — a single full-INPUT_CHUNK
        // process call doesn't always emit anything (the FFT operates on
        // `fft_size_in` which can be slightly larger than our INPUT_CHUNK,
        // e.g. 1026 vs 1024 for 48 kHz → 16 kHz). To drain a leftover
        // partial input and force the internal saved frames out, we
        // push two zero chunks: the first pads our partial input up to
        // INPUT_CHUNK and the second is pure silence to flush the FFT
        // buffer. Both add only synthetic zeros AFTER the real signal
        // ended, so no real samples are lost or duplicated.
        let pending = self.input_buffer.len();
        if pending > 0 || self.output_buffer.len() < FRAME_SIZE {
            // First flush chunk: pad pending input with zeros to INPUT_CHUNK.
            if pending > 0 {
                self.input_buffer.resize(INPUT_CHUNK, 0.0);
            } else {
                self.input_buffer
                    .extend(std::iter::repeat_n(0.0, INPUT_CHUNK));
            }
            self.drain_one_chunk();
            // Second flush chunk: pure-silence buffer to push the FFT's
            // internal saved frames through to the output.
            self.input_buffer
                .extend(std::iter::repeat_n(0.0, INPUT_CHUNK));
            self.drain_one_chunk();
        }

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
        let in_buffers: [&[f32]; 1] = [&chunk];
        if let Ok((_in_len, out_len)) =
            self.inner
                .process_into_buffer(&in_buffers, &mut self.process_scratch, None)
        {
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
