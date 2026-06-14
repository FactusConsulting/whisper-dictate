use super::super::*;
use super::*;

pub(in crate::ui) fn gauge_color_for_position(position: f32, palette: UiPalette) -> egui::Color32 {
    if position < 0.68 {
        palette.ok_text
    } else if position < 0.86 {
        palette.warn_text
    } else {
        palette.error_text
    }
}

pub(in crate::ui) fn latest_metric_summary(log: &str, prefix: &str) -> String {
    latest_prefixed_line(log, prefix)
        .map(compact_diagnostic_title)
        .unwrap_or_else(|| "No data yet".to_owned())
}

pub(in crate::ui) fn latest_log_summary(log: &str, prefix: &str) -> String {
    latest_prefixed_line(log, prefix)
        .map(strip_log_prefix)
        .unwrap_or("No data yet")
        .to_owned()
}

pub(in crate::ui) fn live_audio_level_summary(
    raw_dbfs: Option<f32>,
    peak: Option<f32>,
    active: bool,
) -> String {
    if !active {
        return "Not recording".to_owned();
    }
    match (raw_dbfs, peak) {
        (Some(raw_dbfs), Some(peak)) => format!("raw={raw_dbfs:.1}dBFS  peak={peak:.3}"),
        (Some(raw_dbfs), None) => format!("raw={raw_dbfs:.1}dBFS"),
        _ => "Waiting for audio level".to_owned(),
    }
}

pub(in crate::ui) fn mic_label_char_budget(width: f32) -> usize {
    ((width / 7.0).floor() as usize).clamp(8, 34)
}

pub(in crate::ui) fn audio_device_label(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "Input pending".to_owned();
    }
    compact_label(value, max_chars.clamp(8, 34))
}

pub(in crate::ui) fn full_audio_device_label(value: &str) -> &str {
    let value = value.trim();
    if value.is_empty() {
        "Not reported yet"
    } else {
        value
    }
}

pub(in crate::ui) fn empty_as_auto(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "Auto"
    } else {
        trimmed
    }
}

pub(in crate::ui) fn empty_as_disabled(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "none" {
        "Disabled"
    } else {
        trimmed
    }
}

/// A pill that shows the currently configured hotkey/chord so it is always
/// visible while watching live dictation. The raw setting (e.g. `ctrl_r` or
/// `shift_l+ctrl_l`) is rendered as a human-friendly chord (`Ctrl (right)`).
/// When toggle mode is on the label reads "Toggle key" (press to start, press
/// again to stop); otherwise the usual "Push-to-talk".
/// Mode-labelled chord text — "Push-to-talk: <chord>" (hold mode) or
/// "Toggle key: <chord>" (toggle mode). Used for the sidebar key display's
/// hover text; kept pure so it is unit-testable without an egui context.
pub(in crate::ui) fn push_to_talk_badge_label(
    raw_keys: &str,
    toggle_mode: bool,
    raw_language: &str,
) -> String {
    let prefix = if toggle_mode {
        ui_text(raw_language, UiTextKey::Toggle)
    } else {
        ui_text(raw_language, UiTextKey::PushToTalk)
    };
    format!("{}: {}", prefix, format_push_to_talk_keys(raw_keys))
}

/// Render a raw hotkey setting (`ctrl_r`, `shift_l+ctrl_l`, …) as a friendly
/// chord. Empty input becomes `None`; unknown tokens are passed through
/// capitalized so custom keys still read sensibly.
pub(in crate::ui) fn format_push_to_talk_keys(raw: &str) -> String {
    let chord = raw
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(format_key_token)
        .collect::<Vec<_>>();
    if chord.is_empty() {
        return "None".to_owned();
    }
    chord.join(" + ")
}

fn format_key_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    let (base, side) = if let Some(base) = lower
        .strip_suffix("_l")
        .or_else(|| lower.strip_suffix("_left"))
    {
        (base, Some("left"))
    } else if let Some(base) = lower
        .strip_suffix("_r")
        .or_else(|| lower.strip_suffix("_right"))
    {
        (base, Some("right"))
    } else {
        (lower.as_str(), None)
    };
    let label = match base {
        "ctrl" | "control" => "Ctrl".to_owned(),
        "shift" => "Shift".to_owned(),
        "alt" | "option" => "Alt".to_owned(),
        "cmd" | "command" | "super" | "win" | "meta" => "Cmd/Win".to_owned(),
        "space" => "Space".to_owned(),
        other => capitalize_ascii(other),
    };
    match side {
        Some(side) => format!("{label} ({side})"),
        None => label,
    }
}

fn capitalize_ascii(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => token.to_owned(),
    }
}
