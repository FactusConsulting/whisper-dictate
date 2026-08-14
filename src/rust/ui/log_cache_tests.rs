use super::app::{trim_runtime_log, RUNTIME_LOG_MAX_CHARS, TRIM_MARKER};
use super::*;

fn append_cached_line(cache: &mut RuntimeLogCache, raw: &mut String, line: &str) -> bool {
    cache.append(line);
    if !raw.is_empty() {
        raw.push('\n');
    }
    raw.push_str(line);
    let trimmed = raw.len() > RUNTIME_LOG_MAX_CHARS;
    trim_runtime_log(raw);
    cache.finish_append(raw, trimmed);
    trimmed
}

fn near_capacity_log(prefix: &str, reserve: usize) -> String {
    let filler = format!("[ui] {}", "x".repeat(88));
    let mut raw = prefix.to_owned();
    while raw.len() + filler.len() + 1 < RUNTIME_LOG_MAX_CHARS - reserve {
        raw.push('\n');
        raw.push_str(&filler);
    }
    raw
}

fn representative_log() -> String {
    [
        "[post] clean via groq",
        "[inject] strategy: paste text=\"legacy preview\"",
        "[cap] raw=-31dBFS peak=0.4 input=0.5 snr=22dB",
        "[gate] raw=-29dBFS peak=0.5 snr=24dB",
        "[stt] dur=2.0s compute=0.4s rtf=0.20",
        "[health] mic -31dBFS SNR 22dB good | grade=good",
        r#"[utterance] {"text":"Hele den strukturerede tekst","recording_s":2.0,"compute_s":0.4,"real_time_factor":0.2,"post_mode":"clean","post_processor":"groq","inject_strategy":"paste","dictionary_terms":["cache"],"dictionary_replacements":[]}"#,
        "[worker] status=ready",
    ]
    .join("\n")
}

#[test]
fn cached_views_and_cards_match_the_existing_renderers() {
    let log = representative_log();
    let cache = RuntimeLogCache::from_log(&log);

    for mode in LogViewMode::ALL {
        assert_eq!(cache.text(mode), log_view_text(&log, mode));
        assert_eq!(cache.cards(mode), runtime_log_cards(&log, mode));
    }
    assert_eq!(
        cache.latest_capture(),
        tabs::latest_metric_summary(&log, "[cap]")
    );
    assert_eq!(
        cache.latest_gate(),
        tabs::latest_metric_summary(&log, "[gate]")
    );
    assert_eq!(
        cache.latest_stt(),
        tabs::latest_metric_summary(&log, "[stt]")
    );
    assert_eq!(
        cache.latest_injection(),
        tabs::latest_log_summary(&log, "[inject] strategy:")
    );
}

#[test]
fn a_line_is_parsed_once_and_quiet_frames_only_borrow_cached_views() {
    let mut cache = RuntimeLogCache::default();
    let line = r#"[utterance] {"text":"Parse me once","post_mode":"raw"}"#;
    cache.append(line);
    cache.finish_append(line, false);
    let after_append = cache.stats();
    assert_eq!(after_append.1, 1);

    for _ in 0..100 {
        for mode in LogViewMode::ALL {
            let _ = cache.text(mode);
            let _ = cache.cards(mode);
        }
        let _ = cache.latest_capture();
        let _ = cache.latest_gate();
        let _ = cache.latest_stt();
        let _ = cache.latest_injection();
    }
    assert_eq!(cache.stats(), after_append);

    assert_ne!(
        cache.view_key(LogViewMode::Minimal),
        cache.view_key(LogViewMode::Debug),
        "mode is part of the cached-view key"
    );
    assert_eq!(
        cache.view_key(LogViewMode::Minimal).0,
        cache.view_key(LogViewMode::Debug).0,
        "switching modes must not reparse retained lines"
    );
}

#[test]
fn incremental_append_stays_equivalent_across_structured_transition() {
    let lines = representative_log();
    let mut raw = String::new();
    let mut cache = RuntimeLogCache::default();

    for line in lines.lines() {
        cache.append(line);
        if !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(line);
        cache.finish_append(&raw, false);

        for mode in LogViewMode::ALL {
            assert_eq!(cache.text(mode), log_view_text(&raw, mode));
            assert_eq!(cache.cards(mode), runtime_log_cards(&raw, mode));
        }
    }
    assert_eq!(cache.stats().1, lines.lines().count() as u64);
}

#[test]
fn near_capacity_trim_reuses_parsed_entries() {
    let line = "[OK] retained runtime status";
    let initial = std::iter::repeat_n(line, RUNTIME_LOG_MAX_CHARS / (line.len() + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let mut raw = initial.clone();
    let mut cache = RuntimeLogCache::from_log(&raw);
    let before = cache.stats();

    let appended = "[stt] dur=1.0s compute=0.2s rtf=0.20";
    cache.append(appended);
    raw.push('\n');
    raw.push_str(appended);
    let trimmed = raw.len() > RUNTIME_LOG_MAX_CHARS;
    trim_runtime_log(&mut raw);
    cache.finish_append(&raw, trimmed);

    let after = cache.stats();
    assert!(trimmed);
    assert!(raw.starts_with(TRIM_MARKER));
    assert_eq!(
        after.1 - before.1,
        2,
        "only the appended line and synthetic trim marker are parsed"
    );
    assert_eq!(cache.latest_stt(), "dur=1.0s  compute=0.2s  rtf=0.20");
    assert!(cache.cards(LogViewMode::Diagnostic).len() <= RUNTIME_LOG_MAX_CARDS);

    let quiet = cache.stats();
    for _ in 0..100 {
        let _ = cache.text(LogViewMode::Debug);
        let _ = cache.cards(LogViewMode::Diagnostic);
    }
    assert_eq!(cache.stats(), quiet);
}

#[test]
fn malformed_utterance_is_counted_without_entering_cached_output() {
    let mut cache = RuntimeLogCache::default();
    let malformed = "[utterance] {contains private transcript";
    cache.append(malformed);
    cache.finish_append(malformed, false);

    assert_eq!(cache.stats(), (1, 1, 1));
    assert_eq!(cache.text(LogViewMode::Debug), malformed);
}

#[test]
fn textless_utterances_preserve_the_inject_copy_fallback() {
    for utterance in [
        "[utterance] {malformed",
        r#"[utterance] {"text":"","text_preview":""}"#,
    ] {
        let log = format!("[inject] strategy: paste text=\"Fallback text\"\n{utterance}");
        let cache = RuntimeLogCache::from_log(&log);
        assert_eq!(cache.text(LogViewMode::Minimal), "Fallback text");
        assert_eq!(
            cache.text(LogViewMode::Minimal),
            log_view_text(&log, LogViewMode::Minimal)
        );
    }
}

#[test]
fn trimming_recomputes_retained_inject_card_post_context() {
    let mut raw = near_capacity_log("[post] clean via groq", 8);
    let mut cache = RuntimeLogCache::from_log(&raw);
    let inject = "[inject] strategy: paste text=\"Retained output\"";

    assert!(append_cached_line(&mut cache, &mut raw, inject));
    assert!(!raw.contains("[post]"));
    let card = cache
        .cards(LogViewMode::Diagnostic)
        .iter()
        .find(|card| card.title == "Retained output")
        .expect("retained inject card");
    assert_eq!(card.detail, "Final output");
}

#[test]
fn blank_appends_remain_visible_when_a_later_line_arrives() {
    let mut raw = "first".to_owned();
    let mut cache = RuntimeLogCache::from_log(&raw);

    append_cached_line(&mut cache, &mut raw, "");
    assert_eq!(
        cache.text(LogViewMode::Debug),
        log_view_text(&raw, LogViewMode::Debug)
    );
    append_cached_line(&mut cache, &mut raw, "second");

    assert_eq!(raw, "first\n\nsecond");
    assert_eq!(cache.text(LogViewMode::Debug), "first\n\nsecond");
    assert_eq!(
        cache.text(LogViewMode::Debug),
        log_view_text(&raw, LogViewMode::Debug)
    );
    assert_eq!(cache.stats().1, 3);

    let mut leading_raw = String::new();
    let mut leading_cache = RuntimeLogCache::default();
    append_cached_line(&mut leading_cache, &mut leading_raw, "\n");
    assert_eq!(
        leading_cache.text(LogViewMode::Debug),
        log_view_text(&leading_raw, LogViewMode::Debug)
    );
    append_cached_line(&mut leading_cache, &mut leading_raw, "after blanks");
    assert_eq!(leading_raw, "\n\nafter blanks");
    assert_eq!(leading_cache.text(LogViewMode::Debug), "\n\nafter blanks");
}

#[test]
fn post_cap_appends_do_not_rebuild_retained_history_each_time() {
    let mut raw = near_capacity_log("[ui] beginning", 8);
    let mut cache = RuntimeLogCache::from_log(&raw);
    assert!(append_cached_line(
        &mut cache,
        &mut raw,
        &format!("[ui] crosses the cap {}", "z".repeat(128))
    ));
    let before = cache.stats();

    for index in 0..50 {
        assert!(
            !append_cached_line(
                &mut cache,
                &mut raw,
                &format!("[ui] incremental line {index:02} {}", "y".repeat(72))
            ),
            "trim headroom should amortize subsequent appends"
        );
    }

    let after = cache.stats();
    assert_eq!(
        after.1 - before.1,
        50,
        "each new line is parsed once with no repeated trim-marker rebuild"
    );
}
