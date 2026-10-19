//! Shared input-config negotiation for the Rust audio path.
//!
//! [`pick_config`] is the single implementation of the F32 > I16 > I32
//! priority at a buildable rate/channel combination. Live capture
//! ([`crate::audio::capture`]) and the CLI device probe
//! ([`crate::audio::device_probe`]) both call it, so the probe reports
//! exactly what live capture will negotiate.

use cpal::traits::DeviceTrait;
use cpal::SampleFormat;

/// Pick the best supported input config for `device`: F32 > I16 > I32,
/// stereo preferred, at 48 kHz / 44.1 kHz when the winning range offers
/// them (resampling to the pipeline rate happens later).
///
/// # Errors
/// Fails when `supported_input_configs` fails or the device offers no
/// F32/I16/I32 input config.
pub(crate) fn pick_config(
    device: &cpal::Device,
) -> Result<cpal::SupportedStreamConfig, anyhow::Error> {
    let ranges: Vec<_> = device
        .supported_input_configs()
        .map_err(|err| anyhow::anyhow!("supported_input_configs: {err}"))?
        .collect();
    pick_from_ranges(&ranges)
}

/// Priority F32 > I16 > I32 over the device's supported config ranges,
/// stereo preferred among the winning format, then the rate from cpal's
/// own default heuristic — 48 kHz, then 44.1 kHz, else the highest rate
/// in the winning range. Pure helper so the selection rules are
/// unit-testable without a live cpal device; among same-format ranges
/// the last one listed wins, except a 2-channel range always beats a
/// non-stereo one.
///
/// The stereo + standard-rate preference is not cosmetic: ALSA's
/// conversion plugins advertise conversion ranges (1–64 channels, rates
/// 1 Hz–u32::MAX) for the same device, and a max-rate/max-channel pick
/// over those builds a stream no device can honour (observed on
/// sof-hda-dsp: F32 @ 4294967295 Hz @ 64 ch fails `snd_pcm_hw_params`
/// while I16 @ 48000 @ 2 ch opens).
///
/// # Errors
/// Fails when `supported` offers no F32/I16/I32 input config.
fn pick_from_ranges(
    supported: &[cpal::SupportedStreamConfigRange],
) -> Result<cpal::SupportedStreamConfig, anyhow::Error> {
    fn prefer_stereo_then_last<'a>(
        prev: Option<&'a cpal::SupportedStreamConfigRange>,
        cfg: &'a cpal::SupportedStreamConfigRange,
    ) -> Option<&'a cpal::SupportedStreamConfigRange> {
        match prev {
            Some(p) if p.channels() == 2 && cfg.channels() != 2 => Some(p),
            _ => Some(cfg),
        }
    }

    let mut best_f32: Option<&cpal::SupportedStreamConfigRange> = None;
    let mut best_i16: Option<&cpal::SupportedStreamConfigRange> = None;
    let mut best_i32: Option<&cpal::SupportedStreamConfigRange> = None;

    for cfg in supported {
        match cfg.sample_format() {
            SampleFormat::F32 => best_f32 = prefer_stereo_then_last(best_f32, cfg),
            SampleFormat::I16 => best_i16 = prefer_stereo_then_last(best_i16, cfg),
            SampleFormat::I32 => best_i32 = prefer_stereo_then_last(best_i32, cfg),
            _ => {}
        }
    }
    let picked = best_f32
        .or(best_i16)
        .or(best_i32)
        .ok_or_else(|| anyhow::anyhow!("no F32/I16/I32 input config supported"))?;
    // The rate: cpal's standard-rate heuristic first (48 kHz, then
    // 44.1 kHz — the range max is meaningless on ALSA conversion ranges),
    // else the highest rate within the winning range.
    Ok(picked
        .try_with_standard_sample_rate()
        .unwrap_or_else(|| picked.with_max_sample_rate()))
}

#[cfg(test)]
#[path = "device_pick_tests.rs"]
mod tests;
