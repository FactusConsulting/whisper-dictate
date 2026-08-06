use super::super::*;
use super::compact::{compact_mic_label_char_budget, compact_stage_label};

#[test]
fn compact_stage_label_exposes_each_active_pipeline_stage() {
    let palette = ui_palette("dark");
    for (stage, expected) in [
        ("recording", "Recording…"),
        ("transcribing", "Transcribing…"),
        ("post-processing", "Post-processing…"),
        ("injecting", "Injecting…"),
    ] {
        assert_eq!(
            compact_stage_label(Some(stage), palette, "en").map(|(label, _)| label),
            Some(expected)
        );
    }
    assert!(compact_stage_label(None, palette, "en").is_none());
}

#[test]
fn compact_mic_label_budget_stays_readable_when_narrow() {
    assert_eq!(compact_mic_label_char_budget(0.0), 8);
    assert_eq!(compact_mic_label_char_budget(40.0), 8);
    assert!(compact_mic_label_char_budget(140.0) > compact_mic_label_char_budget(40.0));
}
