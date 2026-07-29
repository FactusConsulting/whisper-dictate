//! Companion tests for `ptt_lock/report.rs` — the refusal message and the
//! slot that carries it to the GUI banner.

use super::record::HolderRecord;
use super::report::{self, PttConflict};

fn conflict_with_holder() -> PttConflict {
    PttConflict {
        chord: "f9".to_owned(),
        holder: Some(HolderRecord::new(
            12345,
            "whisper-dictate-gui",
            "none",
            "win_registerhotkey",
            "f9",
        )),
        lock_path: "/run/user/1000/whisper-dictate-ptt-alice.lock".to_owned(),
    }
}

#[test]
fn the_message_names_the_chord_the_holder_and_the_consequence() {
    // The 2026-07-29 report had none of these three: no error, no
    // warning, nothing in either log. Each assertion below is one of the
    // things the user needed and did not get.
    let message = conflict_with_holder().message();
    assert!(message.contains("f9"), "must name the chord: {message}");
    assert!(
        message.contains("pid 12345"),
        "must name the pid to quit: {message}"
    );
    assert!(
        message.contains("whisper-dictate-gui"),
        "must name the holding program: {message}"
    );
    assert!(
        message.contains("interleaving"),
        "must state the corruption it prevented: {message}"
    );
    assert!(
        message.contains("Quit the other whisper-dictate process"),
        "must say what to do about it: {message}"
    );
    assert!(
        message.contains("/run/user/1000/whisper-dictate-ptt-alice.lock"),
        "must name the contended lock: {message}"
    );
}

#[test]
fn the_message_is_ascii_and_single_line() {
    // It goes to stderr under PowerShell / cmd.exe on a legacy code page
    // and into the GUI diagnostic file, which is read line by line.
    let message = conflict_with_holder().message();
    assert!(
        message.is_ascii(),
        "console output must be ASCII: {message}"
    );
    assert!(!message.contains('\n'), "must be one line: {message}");
}

#[test]
fn a_localized_profile_path_cannot_make_the_message_non_ascii() {
    // Codex P2 #688. The old assertion only ever used `/tmp/...`, so it
    // passed vacuously for the common case: a Danish / German / Japanese
    // Windows profile puts non-ASCII straight into the lock path, and
    // this line goes to PowerShell and cmd.exe stderr where a legacy code
    // page renders those bytes as mojibake.
    let mut conflict = conflict_with_holder();
    conflict.lock_path = "C:\\Users\\J\u{00f8}rgen \u{00c5}\\AppData\\Local\\ptt.lock".to_owned();
    let message = conflict.message();
    assert!(
        message.is_ascii(),
        "console output must be ASCII: {message}"
    );
    // Degraded, but still identifiable: the structure survives so a
    // support thread can tell WHICH lock was contended.
    assert!(message.contains("AppData"), "{message}");
    assert!(message.contains("ptt.lock"), "{message}");
    // The part the user acts on is untouched.
    assert!(message.contains("pid 12345"), "{message}");
}

#[test]
fn the_unnamed_holder_branch_is_ascii_too() {
    // The other arm interpolates the path a second time; it needs the
    // same guard, and a test that only exercised the named-holder branch
    // would not have caught a regression here.
    let conflict = PttConflict {
        chord: "f9".to_owned(),
        holder: None,
        lock_path: "/tmp/\u{4f60}\u{597d}/ptt.lock".to_owned(),
    };
    let message = conflict.message();
    assert!(message.is_ascii(), "{message}");
    assert!(message.contains("ptt.lock"), "{message}");
}

#[test]
fn a_non_ascii_chord_cannot_leak_into_the_console_line() {
    // The chord comes from user config, so it is user-influenced input on
    // the same output surface.
    let mut conflict = conflict_with_holder();
    conflict.chord = "ctrl_l+\u{00e6}".to_owned();
    assert!(conflict.message().is_ascii());
}

#[test]
fn ascii_path_degrades_rather_than_dropping_information() {
    use super::report::ascii_path;
    assert_eq!(ascii_path("/tmp/ptt.lock"), "/tmp/ptt.lock");
    assert_eq!(ascii_path("C:\\Users\\J\u{00f8}rgen"), "C:\\Users\\J?rgen");
    // One replacement per character, so path structure (separators,
    // extension) is preserved even when the name is entirely non-ASCII.
    assert_eq!(ascii_path("/a/\u{4f60}\u{597d}/b.lock"), "/a/??/b.lock");
    assert_eq!(ascii_path(""), "");
}

#[test]
fn an_unknown_holder_still_produces_an_actionable_message() {
    // The lock is held but the advisory record was lost. We cannot name
    // the pid, so the message must tell the user how to find it instead
    // of trailing off.
    let conflict = PttConflict {
        chord: "ctrl_r".to_owned(),
        holder: None,
        lock_path: "C:\\Users\\a\\AppData\\Local\\WhisperDictate\\ptt.lock".to_owned(),
    };
    let message = conflict.message();
    assert_eq!(conflict.holder_pid(), None);
    assert!(message.contains("ctrl_r"), "{message}");
    assert!(
        message.contains("Task Manager"),
        "must tell the user how to find the holder: {message}"
    );
    assert!(message.contains("interleaving"), "{message}");
    assert!(message.is_ascii(), "{message}");
}

#[test]
fn holder_description_degrades_without_naming_a_wrong_process() {
    let conflict = PttConflict {
        chord: "f9".to_owned(),
        holder: None,
        lock_path: "lock".to_owned(),
    };
    let described = conflict.holder_description();
    assert!(described.contains("pid unknown"), "{described}");
    assert_eq!(conflict_with_holder().holder_pid(), Some(12345));
}

#[test]
fn the_slot_publishes_retracts_and_last_writer_wins() {
    let _slot = report::TEST_SLOT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    report::clear();
    assert!(report::current().is_none());

    report::record(conflict_with_holder());
    assert_eq!(report::current().and_then(|c| c.holder_pid()), Some(12345));

    // A newer refusal replaces the older one: the GUI banner must show
    // the CURRENT blocker, not the first one ever seen.
    let mut newer = conflict_with_holder();
    newer.holder = Some(HolderRecord::new(
        999,
        "whisper-dictate",
        "dictate-run",
        "rdev",
        "f9",
    ));
    report::record(newer);
    assert_eq!(report::current().and_then(|c| c.holder_pid()), Some(999));

    // A later successful install must be able to take the banner down.
    report::clear();
    assert!(report::current().is_none());
}
