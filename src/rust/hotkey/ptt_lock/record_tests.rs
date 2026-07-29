//! Companion tests for `ptt_lock/record.rs`.

use super::record::{sanitize_token, HolderRecord, NO_VERB_TOKEN, RECORD_VERSION, UNKNOWN_TOKEN};

#[test]
fn a_record_round_trips_through_the_wire_form() {
    let record = HolderRecord::new(
        4242,
        "whisper-dictate-gui",
        "none",
        "win_registerhotkey",
        "f9",
    );
    let parsed = HolderRecord::parse(&record.encode()).expect("round trip");
    assert_eq!(parsed, record);
}

#[test]
fn the_wire_form_is_a_single_ascii_line() {
    // It is read straight into a console line and into the GUI diagnostic
    // file; a stray newline would truncate the record and a non-ASCII
    // byte would render as mojibake under cmd.exe (AGENTS.md).
    let encoded = HolderRecord::new(
        7,
        "whisper-dictate",
        "dictate-run",
        "rdev",
        "ctrl_l+shift_l",
    )
    .encode();
    assert!(encoded.is_ascii(), "record must be ASCII: {encoded}");
    assert!(
        !encoded.contains('\n'),
        "record must be one line: {encoded}"
    );
    assert!(encoded.starts_with(&format!("v={RECORD_VERSION} ")));
    assert!(encoded.contains("chord=ctrl_l+shift_l"));
}

#[test]
fn parsing_is_total_for_malformed_input() {
    // A contender reads this file WITHOUT any lock, so it can legitimately
    // observe a half-written record. Every failure must degrade to
    // "holder unknown" rather than panic or produce a wrong pid.
    assert!(HolderRecord::parse("").is_none());
    assert!(HolderRecord::parse("garbage").is_none());
    assert!(HolderRecord::parse("v=1 pid=notanumber exe=a verb=b driver=c chord=d").is_none());
    assert!(
        HolderRecord::parse("v=1 pid=5 exe=a verb=b driver=c").is_none(),
        "a missing field must not be silently defaulted"
    );
    // A truncated write: the tail of the line never made it to disk.
    assert!(HolderRecord::parse("v=1 pid=5 exe=whisper-dic").is_none());
}

#[test]
fn an_unknown_format_version_is_rejected_rather_than_guessed() {
    let raw = "v=99 pid=5 exe=a verb=b driver=c chord=d";
    assert!(
        HolderRecord::parse(raw).is_none(),
        "a future format must not be misread by an old binary"
    );
}

#[test]
fn unknown_keys_are_ignored_so_additive_fields_stay_compatible() {
    let raw = "v=1 pid=5 exe=a verb=b driver=c chord=d newfield=x";
    let parsed = HolderRecord::parse(raw).expect("additive field must not break parsing");
    assert_eq!(parsed.pid, 5);
}

#[test]
fn only_the_first_line_is_parsed() {
    let raw = "v=1 pid=5 exe=a verb=b driver=c chord=d\nleftover from a longer record\n";
    assert_eq!(HolderRecord::parse(raw).expect("first line").pid, 5);
}

#[test]
fn fields_are_sanitised_at_construction() {
    // A user's executable can live at any path and be named anything;
    // none of that may reach the `key=value` line unescaped.
    let record = HolderRecord::new(1, "whisper dictate", "run\nnow", "rd ev", "f9 ");
    assert_eq!(record.exe, "whisper_dictate");
    assert!(!record.verb.contains('\n'));
    assert!(record.encode().is_ascii());
    assert_eq!(
        HolderRecord::parse(&record.encode()).expect("sanitised record parses"),
        record
    );
}

#[test]
fn sanitize_token_keeps_the_characters_the_format_relies_on() {
    assert_eq!(sanitize_token("ctrl_l+shift_l"), "ctrl_l+shift_l");
    assert_eq!(sanitize_token("whisper-dictate-gui"), "whisper-dictate-gui");
    assert_eq!(sanitize_token("v1.22.0"), "v1.22.0");
}

#[test]
fn sanitize_token_replaces_everything_else_and_never_returns_empty() {
    assert_eq!(sanitize_token(""), UNKNOWN_TOKEN);
    assert_eq!(sanitize_token("   "), UNKNOWN_TOKEN);
    assert_eq!(sanitize_token("=\n "), UNKNOWN_TOKEN);
    assert!(
        sanitize_token("\u{4f60}\u{597d}") == UNKNOWN_TOKEN
            || sanitize_token("\u{4f60}\u{597d}").is_ascii()
    );
}

#[test]
fn sanitize_token_is_length_capped() {
    // A corrupted or hostile file must not be able to stretch a log line
    // without bound.
    let long = "a".repeat(500);
    assert!(sanitize_token(&long).len() <= 64);
}

#[test]
fn describe_leads_with_the_pid_and_omits_a_missing_verb() {
    // The pid is the one thing the user needs in order to act, so it goes
    // first in the message.
    let gui = HolderRecord::new(
        12345,
        "whisper-dictate-gui",
        NO_VERB_TOKEN,
        "win_registerhotkey",
        "f9",
    );
    let described = gui.describe();
    assert!(described.starts_with("pid 12345 "), "got {described}");
    assert!(described.contains("whisper-dictate-gui"));
    assert!(
        !described.contains(NO_VERB_TOKEN),
        "a GUI entry point has no subcommand to show: {described}"
    );

    let cli = HolderRecord::new(999, "whisper-dictate", "dictate-run", "rdev", "f9");
    assert!(cli.describe().contains("whisper-dictate dictate-run"));
}

#[test]
fn for_current_process_reports_this_process_id() {
    // Keyed on the act of registering, not on a hard-coded binary list:
    // the record describes whatever process called it, so a future entry
    // point is named correctly without being registered anywhere.
    let record = HolderRecord::for_current_process("f9", "rdev");
    assert_eq!(record.pid, std::process::id());
    assert_eq!(record.chord, "f9");
    assert_eq!(record.driver, "rdev");
    assert!(!record.exe.is_empty());
}
