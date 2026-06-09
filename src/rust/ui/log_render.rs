//! Runtime-log presentation: the log-view mode enum, the structured "card"
//! model, and the parsers that turn raw worker log lines into minimal/diagnostic
//! views and metric summaries.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum LogViewMode {
    Minimal,
    Diagnostic,
    Debug,
}

impl LogViewMode {
    pub(in crate::ui) const ALL: [LogViewMode; 3] = [
        LogViewMode::Minimal,
        LogViewMode::Diagnostic,
        LogViewMode::Debug,
    ];

    pub(in crate::ui) fn label(self, raw_language: &str) -> &'static str {
        match self {
            LogViewMode::Minimal => ui_text(raw_language, UiTextKey::Minimal),
            LogViewMode::Diagnostic => ui_text(raw_language, UiTextKey::Diagnostic),
            LogViewMode::Debug => ui_text(raw_language, UiTextKey::Debug),
        }
    }

    pub(in crate::ui) fn from_raw(raw: &str) -> Self {
        match raw {
            "diagnostic" => Self::Diagnostic,
            "debug" => Self::Debug,
            _ => Self::Minimal,
        }
    }

    pub(in crate::ui) fn id(self) -> &'static str {
        match self {
            LogViewMode::Minimal => "minimal",
            LogViewMode::Diagnostic => "diagnostic",
            LogViewMode::Debug => "debug",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum RuntimeLogCardKind {
    FinalText,
    Status,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct RuntimeLogCard {
    pub(in crate::ui) kind: RuntimeLogCardKind,
    pub(in crate::ui) title: String,
    pub(in crate::ui) detail: String,
    pub(in crate::ui) badge: String,
}

pub(in crate::ui) fn log_view_text(log: &str, mode: LogViewMode) -> String {
    match mode {
        LogViewMode::Minimal => final_output_text(log),
        LogViewMode::Debug => log.to_owned(),
        LogViewMode::Diagnostic => log
            .lines()
            .filter(|line| is_diagnostic_log_line(line))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

pub(in crate::ui) fn runtime_log_cards(log: &str, mode: LogViewMode) -> Vec<RuntimeLogCard> {
    if matches!(mode, LogViewMode::Debug) {
        return Vec::new();
    }

    let has_structured_utterance = log.lines().any(|line| line.starts_with("[utterance] "));
    let mut cards = Vec::new();
    for line in log.lines() {
        if let Some(card) = runtime_log_card_for_line(line, mode, log, has_structured_utterance) {
            cards.push(card);
        }
    }
    cards
}

/// Classify a single log line into at most one card. Returns `None` when the
/// line produces no card in this mode (filtered, skipped, or unrecognised).
fn runtime_log_card_for_line(
    line: &str,
    mode: LogViewMode,
    log: &str,
    has_structured_utterance: bool,
) -> Option<RuntimeLogCard> {
    match mode {
        LogViewMode::Debug => None,
        LogViewMode::Minimal => minimal_log_card(line),
        LogViewMode::Diagnostic => diagnostic_log_card(line, log, has_structured_utterance),
    }
}

/// Minimal mode shows only the final injected text.
fn minimal_log_card(line: &str) -> Option<RuntimeLogCard> {
    let text = extract_inject_preview(line)?;
    Some(RuntimeLogCard {
        kind: RuntimeLogCardKind::FinalText,
        title: text,
        detail: String::new(),
        badge: "Final".to_owned(),
    })
}

/// Diagnostic mode: structured utterance summaries, final text, status and
/// per-stage diagnostic detail cards. When structured `[utterance]` lines are
/// present they supersede the redundant `[inject]`/`[post]`/detail lines.
fn diagnostic_log_card(
    line: &str,
    log: &str,
    has_structured_utterance: bool,
) -> Option<RuntimeLogCard> {
    if let Some(card) = structured_utterance_card(line) {
        return Some(card);
    }

    if let Some(text) = extract_inject_preview(line) {
        if has_structured_utterance {
            return None;
        }
        return Some(RuntimeLogCard {
            kind: RuntimeLogCardKind::FinalText,
            title: text,
            detail: latest_previous_post_detail(log, line)
                .unwrap_or_else(|| "Final output".to_owned()),
            badge: "Final".to_owned(),
        });
    }

    if has_structured_utterance
        && (line.starts_with("[post]") || is_diagnostic_detail_line(line))
    {
        return None;
    }

    if let Some(card) = status_card(line) {
        return Some(card);
    }

    if is_diagnostic_detail_line(line) {
        return Some(RuntimeLogCard {
            kind: RuntimeLogCardKind::Diagnostic,
            title: compact_diagnostic_title(line),
            detail: diagnostic_detail_label(line).to_owned(),
            badge: diagnostic_badge(line).to_owned(),
        });
    }

    None
}

/// `[post]` / `[worker] status=` / `[OK]` / `[ERROR]` status lines (shown in
/// both diagnostic and debug-adjacent views).
fn status_card(line: &str) -> Option<RuntimeLogCard> {
    if line.starts_with("[post]") {
        return Some(RuntimeLogCard {
            kind: RuntimeLogCardKind::Status,
            title: strip_log_prefix(line).to_owned(),
            detail: "Post-processing".to_owned(),
            badge: "Post".to_owned(),
        });
    }
    if line.starts_with("[worker] status=") {
        return Some(RuntimeLogCard {
            kind: RuntimeLogCardKind::Status,
            title: line.trim_start_matches("[worker] status=").to_owned(),
            detail: "Worker state".to_owned(),
            badge: "Worker".to_owned(),
        });
    }
    if line.starts_with("[OK]") || line.starts_with("[ERROR]") {
        return Some(RuntimeLogCard {
            kind: RuntimeLogCardKind::Status,
            title: line.to_owned(),
            detail: "Runtime message".to_owned(),
            badge: "Status".to_owned(),
        });
    }
    None
}

fn final_output_text(log: &str) -> String {
    let injected = log
        .lines()
        .filter_map(extract_inject_preview)
        .collect::<Vec<_>>();
    if !injected.is_empty() {
        return injected.join("\n");
    }
    log.lines()
        .filter_map(extract_utterance_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_diagnostic_status_line(line: &str) -> bool {
    line.starts_with("[worker] status=")
        || line.starts_with("[post]")
        || line.starts_with("[inject]")
        || line.starts_with("[OK]")
        || line.starts_with("[ERROR]")
}

fn is_diagnostic_log_line(line: &str) -> bool {
    is_diagnostic_status_line(line) || is_diagnostic_detail_line(line)
}

fn is_diagnostic_detail_line(line: &str) -> bool {
    line.starts_with("[gate]")
        || line.starts_with("[cap]")
        || line.starts_with("[stt]")
        || line.starts_with("[stt-debug]")
        || line.starts_with("[utterance] ")
}

fn structured_utterance_card(line: &str) -> Option<RuntimeLogCard> {
    let payload = parse_utterance_payload(line)?;
    let title = extract_utterance_text(line).unwrap_or_else(|| "Utterance".to_owned());
    let mut details = Vec::new();

    let recording = format_metric_seconds(&payload, "recording_s", "recording");
    let raw = format_metric_dbfs(&payload, "audio_raw_dbfs", "raw");
    let peak = format_metric_float(&payload, "audio_peak", "peak", 3);
    let noise = format_metric_dbfs(&payload, "audio_noise_dbfs", "noise");
    let snr = format_metric_db(&payload, "audio_snr_db", "snr");
    let gain = format_metric_gain(&payload, "audio_gain", "gain");
    let post_boost = format_metric_dbfs(&payload, "post_boost_dbfs", "post-boost");
    push_joined(
        &mut details,
        [recording, raw, peak, noise, snr, gain, post_boost],
    );

    let compute = format_metric_seconds(&payload, "compute_s", "compute");
    let rtf = format_metric_float(&payload, "real_time_factor", "rtf", 2);
    push_joined(&mut details, [compute, rtf]);

    push_joined(
        &mut details,
        [
            worker_event_string(&payload, "stt_backend").map(|value| format!("backend={value}")),
            worker_event_string(&payload, "model").map(|value| format!("model={value}")),
            worker_event_string(&payload, "device").map(|value| format!("device={value}")),
        ],
    );

    let dictionary_terms = json_array_len(&payload, "dictionary_terms");
    let dictionary_replacements = json_array_len(&payload, "dictionary_replacements");
    details.push(format!(
        "dictionary terms={} replacements={}",
        dictionary_terms.unwrap_or(0),
        dictionary_replacements.unwrap_or(0)
    ));

    push_joined(
        &mut details,
        [
            worker_event_string(&payload, "post_mode").map(|value| format!("post={value}")),
            worker_event_string(&payload, "post_processor")
                .map(|value| format!("provider={value}")),
            worker_event_string(&payload, "post_model").map(|value| format!("post_model={value}")),
            format_metric_ms(&payload, "post_latency_ms", "post"),
            worker_event_bool(&payload, "post_changed").map(|value| format!("changed={value}")),
            worker_event_bool(&payload, "post_fallback").map(|value| format!("fallback={value}")),
        ],
    );

    push_joined(
        &mut details,
        [
            worker_event_string(&payload, "inject_strategy")
                .or_else(|| worker_event_string(&payload, "inject_mode"))
                .map(|value| format!("inject={value}")),
            worker_event_string(&payload, "target_title")
                .map(|value| format!("target={}", compact_runtime_text(&value, 48))),
        ],
    );

    Some(RuntimeLogCard {
        kind: RuntimeLogCardKind::Diagnostic,
        title: compact_runtime_text(&title, 140),
        detail: details.join("  |  "),
        badge: "Utterance".to_owned(),
    })
}

fn parse_utterance_payload(line: &str) -> Option<serde_json::Value> {
    serde_json::from_str(line.strip_prefix("[utterance] ")?).ok()
}

fn extract_utterance_text(line: &str) -> Option<String> {
    let payload = parse_utterance_payload(line)?;
    worker_event_string(&payload, "text_preview")
        .or_else(|| worker_event_string(&payload, "text"))
        .filter(|value| !value.trim().is_empty())
}

fn push_joined<const N: usize>(details: &mut Vec<String>, values: [Option<String>; N]) {
    let line = values.into_iter().flatten().collect::<Vec<_>>().join("  ");
    if !line.is_empty() {
        details.push(line);
    }
}

fn format_metric_seconds(payload: &serde_json::Value, key: &str, label: &str) -> Option<String> {
    worker_event_f32(payload, key).map(|value| format!("{label}={value:.1}s"))
}

fn format_metric_ms(payload: &serde_json::Value, key: &str, label: &str) -> Option<String> {
    worker_event_f32(payload, key).map(|value| format!("{label}={value:.0}ms"))
}

fn format_metric_dbfs(payload: &serde_json::Value, key: &str, label: &str) -> Option<String> {
    worker_event_f32(payload, key).map(|value| format!("{label}={value:.0}dBFS"))
}

fn format_metric_db(payload: &serde_json::Value, key: &str, label: &str) -> Option<String> {
    worker_event_f32(payload, key).map(|value| format!("{label}={value:.0}dB"))
}

fn format_metric_gain(payload: &serde_json::Value, key: &str, label: &str) -> Option<String> {
    worker_event_f32(payload, key).map(|value| format!("{label}={value:.1}x"))
}

fn format_metric_float(
    payload: &serde_json::Value,
    key: &str,
    label: &str,
    decimals: usize,
) -> Option<String> {
    worker_event_f32(payload, key).map(|value| format!("{label}={value:.decimals$}"))
}

fn json_array_len(payload: &serde_json::Value, key: &str) -> Option<usize> {
    payload.get(key)?.as_array().map(Vec::len)
}

fn compact_runtime_text(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    let mut chars = value.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return value.to_owned();
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn extract_inject_preview(line: &str) -> Option<String> {
    if !line.starts_with("[inject]") {
        return None;
    }
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    let text = rest[..end].trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn latest_previous_post_detail(log: &str, current_line: &str) -> Option<String> {
    let mut previous = None;
    for line in log.lines() {
        if line == current_line {
            break;
        }
        if line.starts_with("[post]") {
            previous = Some(strip_log_prefix(line).to_owned());
        }
    }
    previous
}

pub(in crate::ui) fn strip_log_prefix(line: &str) -> &str {
    line.split_once(']').map_or(line, |(_, rest)| rest.trim())
}

fn diagnostic_badge(line: &str) -> &str {
    if line.starts_with("[gate]") {
        "Gate"
    } else if line.starts_with("[cap]") {
        "Capture"
    } else if line.starts_with("[stt-debug]") {
        "STT debug"
    } else if line.starts_with("[stt]") {
        "STT"
    } else {
        "Diag"
    }
}

fn diagnostic_detail_label(line: &str) -> &str {
    if line.starts_with("[gate]") {
        "Voice gate"
    } else if line.starts_with("[cap]") {
        "Audio input"
    } else if line.starts_with("[stt-debug]") {
        "Backend detail"
    } else if line.starts_with("[stt]") {
        "Transcription"
    } else {
        "Diagnostic"
    }
}

pub(in crate::ui) fn compact_diagnostic_title(line: &str) -> String {
    if line.starts_with("[stt]") {
        let dur = extract_metric_token(line, "dur=").unwrap_or("duration=?");
        let compute = extract_metric_token(line, "compute=").unwrap_or("compute=?");
        let rtf = extract_metric_token(line, "rtf=").unwrap_or("rtf=?");
        return format!("{dur}  {compute}  {rtf}");
    }
    if line.starts_with("[cap]") || line.starts_with("[gate]") {
        let raw = extract_metric_token(line, "raw=").unwrap_or("raw=?");
        let snr = extract_metric_token(line, "snr=").unwrap_or("snr=?");
        let peak = extract_metric_token(line, "peak=");
        let input = extract_metric_token(line, "input=");
        return match (peak, input) {
            (Some(peak), Some(input)) => format!("{raw}  {peak}  {input}  {snr}"),
            (Some(peak), None) => format!("{raw}  {peak}  {snr}"),
            (None, Some(input)) => format!("{raw}  {input}  {snr}"),
            (None, None) => format!("{raw}  {snr}"),
        };
    }
    strip_log_prefix(line).to_owned()
}

fn extract_metric_token<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)?;
    let token = line[start..]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches(',');
    (!token.is_empty()).then_some(token)
}

pub(in crate::ui) fn latest_prefixed_line<'a>(log: &'a str, prefix: &str) -> Option<&'a str> {
    log.lines().rev().find(|line| line.starts_with(prefix))
}
