//! Unit tests for the pure selection rules in [`device_pick`]: format
//! priority, rate selection, and the rejection of devices that only
//! offer non-negotiable formats.
//!
//! These pin the contract live capture and the CLI device probe both
//! rely on: the probe must be able to report exactly what capture will
//! negotiate, which only holds while both call the same helper.

use cpal::SampleFormat;
use cpal::SupportedBufferSize;
use cpal::SupportedStreamConfigRange;

use super::pick_from_ranges;

fn range(min: u32, max: u32, format: SampleFormat) -> SupportedStreamConfigRange {
    range_ch(1, min, max, format)
}

fn range_ch(channels: u16, min: u32, max: u32, format: SampleFormat) -> SupportedStreamConfigRange {
    SupportedStreamConfigRange::new(
        channels,
        min,
        max,
        SupportedBufferSize::Range { min: 64, max: 1024 },
        format,
    )
}

fn picked_format(config: &cpal::SupportedStreamConfig) -> SampleFormat {
    config.sample_format()
}

fn picked_rate(config: &cpal::SupportedStreamConfig) -> u32 {
    config.sample_rate()
}

#[test]
fn f32_beats_i16_and_i32() {
    let supported = [
        range(16000, 48000, SampleFormat::I32),
        range(16000, 96000, SampleFormat::I16),
        range(8000, 44100, SampleFormat::F32),
    ];
    let picked = pick_from_ranges(&supported).expect("f32 range present");
    assert_eq!(picked_format(&picked), SampleFormat::F32);
    assert_eq!(picked_rate(&picked), 44100);
}

#[test]
fn i16_beats_i32_when_f32_absent() {
    let supported = [
        range(16000, 48000, SampleFormat::I32),
        range(16000, 96000, SampleFormat::I16),
    ];
    let picked = pick_from_ranges(&supported).expect("i16 range present");
    assert_eq!(picked_format(&picked), SampleFormat::I16);
    // The winning range contains 48 kHz, so cpal's standard-rate
    // heuristic applies before the max-rate fallback.
    assert_eq!(picked_rate(&picked), 48000);
}

#[test]
fn selects_the_highest_rate_when_no_standard_rate_fits() {
    // 16 kHz-only range (the SOF DMIC): neither 48 kHz nor 44.1 kHz fits,
    // so the highest rate in the range wins.
    let supported = [range(8000, 16000, SampleFormat::I16)];
    let picked = pick_from_ranges(&supported).expect("i16 range present");
    assert_eq!(picked_rate(&picked), 16000);
}

#[test]
fn last_listed_range_wins_among_same_format_ranges() {
    let supported = [
        range(8000, 44100, SampleFormat::F32),
        range(8000, 96000, SampleFormat::F32),
    ];
    let picked = pick_from_ranges(&supported).expect("f32 range present");
    // Last listed wins, and the winning range contains 48 kHz.
    assert_eq!(picked_rate(&picked), 48000);
}

#[test]
fn stereo_range_beats_other_channel_counts_within_a_format() {
    // ALSA conversion plugins enumerate 1–64 channel ranges for the same
    // device; a 2-channel range must win within the winning format, and
    // the last 2-channel range wins among ties.
    let supported = [
        range_ch(64, 1, u32::MAX, SampleFormat::F32),
        range_ch(1, 1, u32::MAX, SampleFormat::F32),
        range_ch(2, 1, u32::MAX, SampleFormat::F32),
        range_ch(64, 1, u32::MAX, SampleFormat::F32),
    ];
    let picked = pick_from_ranges(&supported).expect("f32 range present");
    assert_eq!(picked.channels(), 2);
    assert_eq!(picked_rate(&picked), 48000);
}

#[test]
fn alsa_conversion_rate_range_never_yields_the_range_max() {
    // ALSA conversion plugins advertise 1 Hz..u32::MAX; a max-rate pick
    // over those builds a stream no device can honour (observed on
    // sof-hda-dsp: F32 @ 4294967295 Hz @ 64 ch fails snd_pcm_hw_params
    // while 48 kHz stereo opens). The standard-rate heuristic must clamp
    // the pick back to 48 kHz.
    let supported = [range_ch(2, 1, u32::MAX, SampleFormat::F32)];
    let picked = pick_from_ranges(&supported).expect("f32 range present");
    assert_eq!(picked_rate(&picked), 48000);
}

#[test]
fn formats_outside_the_negotiable_set_are_rejected() {
    let supported = [
        range(16000, 48000, SampleFormat::U8),
        range(16000, 48000, SampleFormat::U16),
    ];
    let err = pick_from_ranges(&supported).expect_err("no negotiable format");
    assert!(err
        .to_string()
        .contains("no F32/I16/I32 input config supported"));
}

#[test]
fn an_empty_range_list_is_rejected() {
    let err = pick_from_ranges(&[]).expect_err("no ranges at all");
    assert!(err
        .to_string()
        .contains("no F32/I16/I32 input config supported"));
}
