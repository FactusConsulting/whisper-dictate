//! Shared input-config negotiation for the Rust audio path.
//!
//! [`pick_config`] is the single implementation of the F32 > I16 > I32
//! priority at the device's native rate. Live capture
//! ([`crate::audio::capture`]) and the CLI device probe
//! ([`crate::audio::device_probe`]) both call it, so the probe reports
//! exactly what live capture will negotiate.

use cpal::traits::DeviceTrait;
use cpal::SampleFormat;

/// Pick the best supported input config for `device`: F32 > I16 > I32,
/// always at the device's natively supported maximum rate (resampling to
/// the pipeline rate happens later).
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
/// always at the highest natively-supported rate within the winning
/// range. Pure helper so the selection rules are unit-testable without a
/// live cpal device; among ranges of the same winning format the last
/// one listed wins.
///
/// # Errors
/// Fails when `supported` offers no F32/I16/I32 input config.
fn pick_from_ranges(
    supported: &[cpal::SupportedStreamConfigRange],
) -> Result<cpal::SupportedStreamConfig, anyhow::Error> {
    let mut best_f32: Option<&cpal::SupportedStreamConfigRange> = None;
    let mut best_i16: Option<&cpal::SupportedStreamConfigRange> = None;
    let mut best_i32: Option<&cpal::SupportedStreamConfigRange> = None;

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

#[cfg(test)]
#[path = "device_pick_tests.rs"]
mod tests;
