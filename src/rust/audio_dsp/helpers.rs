//! Numeric primitives shared by the metrics and silence-trim submodules.
//!
//! Kept private to the parent `audio_dsp` module — these are
//! implementation details (numpy-percentile substitute, RMS/peak math)
//! that callers shouldn't reach for directly. Re-exposed within the
//! crate via `pub(super)` so the sibling submodules can share them.

use super::FRAME_SAMPLES;

/// Python `float(x) or 1e-9` — replace exactly-zero (incl. -0.0) with
/// a tiny epsilon so the subsequent `log10` doesn't blow up. NaN is
/// truthy in Python's `or`, so it passes through unchanged.
pub(super) fn nonzero_or_eps(value: f64) -> f64 {
    if value == 0.0 {
        1e-9
    } else {
        value
    }
}

/// numpy.percentile(values, p) with the default (`"linear"`) interpolation
/// method. The percentile-noise-floor math relies on the exact interp
/// behaviour (10th + 90th of small frame counts land between bins), so
/// re-implementing it cheaply here avoids a numpy dep just to match it.
///
/// `values` is mutated in-place (sorted ascending) — callers that need to
/// preserve order should clone first. `percentile` is in 0..=100 and is
/// clamped to that range; an empty slice returns `0.0` to mirror numpy's
/// "all-nan" handling on an empty input (the caller's downstream `or 1e-9`
/// guard then takes over).
pub(super) fn percentile(values: &mut [f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    let p = percentile.clamp(0.0, 100.0);
    let pos = p / 100.0 * (n as f64 - 1.0);
    let low = pos.floor() as usize;
    let high = pos.ceil() as usize;
    if low == high {
        return values[low];
    }
    let frac = pos - low as f64;
    values[low] + frac * (values[high] - values[low])
}

/// Per-frame RMS values for the 30 ms framing the noise-floor + trim
/// math share. Returns one entry per full frame; callers that need to
/// score the trailing partial-frame remainder push it themselves.
pub(super) fn frame_rms(samples: &[f32]) -> Vec<f64> {
    let n = samples.len() / FRAME_SAMPLES;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let frame = &samples[i * FRAME_SAMPLES..(i + 1) * FRAME_SAMPLES];
        out.push(rms_f64(frame));
    }
    out
}

pub(super) fn rms_f64(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sum_sq = 0.0_f64;
    for &s in samples {
        let v = s as f64;
        sum_sq += v * v;
    }
    (sum_sq / samples.len() as f64).sqrt()
}

pub(super) fn peak_abs_f64(samples: &[f32]) -> f64 {
    let mut peak = 0.0_f64;
    for &s in samples {
        let abs = (s as f64).abs();
        if abs > peak {
            peak = abs;
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_matches_numpy_linear_interpolation() {
        // numpy.percentile([1,2,3,4,5,6,7,8,9,10], 10) == 1.9
        let mut v: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        assert!((percentile(&mut v, 10.0) - 1.9).abs() < 1e-12);
        // numpy.percentile([1..=10], 90) == 9.1
        let mut v: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        assert!((percentile(&mut v, 90.0) - 9.1).abs() < 1e-12);
    }

    #[test]
    fn nonzero_or_eps_replaces_zero() {
        assert_eq!(nonzero_or_eps(0.0), 1e-9);
        assert_eq!(nonzero_or_eps(-0.0), 1e-9);
        assert_eq!(nonzero_or_eps(0.5), 0.5);
        assert_eq!(nonzero_or_eps(-0.5), -0.5);
    }
}
