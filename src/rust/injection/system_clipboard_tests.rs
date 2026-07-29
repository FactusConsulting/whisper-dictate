use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use super::paste::Clipboard;
#[cfg(target_os = "linux")]
use super::system_clipboard::linux_candidates;
use super::system_clipboard::{
    decode_text, run_write, write_program, Candidate, CommandRunner, SystemClipboard,
};

#[derive(Default)]
struct FakeState {
    reads: Vec<String>,
    writes: Vec<String>,
    read_results: HashMap<&'static str, Option<String>>,
    write_results: HashMap<&'static str, VecDeque<bool>>,
}

struct FakeRunner {
    state: Arc<Mutex<FakeState>>,
}

impl CommandRunner for FakeRunner {
    fn read(&mut self, candidate: Candidate) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        state.reads.push(candidate.program.to_owned());
        state.read_results.get(candidate.program).cloned().flatten()
    }

    fn write(&mut self, candidate: Candidate, value: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        state.writes.push(format!("{}:{value}", candidate.program));
        state
            .write_results
            .get_mut(candidate.program)
            .and_then(VecDeque::pop_front)
            .unwrap_or(false)
    }
}

fn clipboard_with(state: Arc<Mutex<FakeState>>, candidates: Vec<Candidate>) -> SystemClipboard {
    SystemClipboard::with_runner(candidates, Box::new(FakeRunner { state }))
}

#[test]
#[cfg(target_os = "linux")]
fn wayland_prefers_native_clipboard_tools() {
    let names: Vec<_> = linux_candidates(true)
        .into_iter()
        .map(|candidate| candidate.program)
        .collect();
    assert_eq!(names, ["wl-paste", "xclip", "xsel"]);
}

#[test]
fn write_program_pairs_wayland_reader_with_writer() {
    let wl = Candidate {
        program: "wl-paste",
        read_args: &[],
        write_args: &[],
    };
    let xclip = Candidate {
        program: "xclip",
        read_args: &[],
        write_args: &[],
    };
    assert_eq!(write_program(wl), "wl-copy");
    assert_eq!(write_program(xclip), "xclip");
}

#[test]
fn invalid_utf8_clipboard_payload_is_not_claimed_as_text() {
    assert_eq!(decode_text(vec![0xff, 0xfe, 0xfd]), None);
    assert_eq!(
        decode_text("æøå".as_bytes().to_vec()).as_deref(),
        Some("æøå")
    );
}

#[test]
#[cfg(target_os = "linux")]
fn unreadable_selection_is_never_overwritten() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    let mut clipboard = clipboard_with(state.clone(), linux_candidates(true));

    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("dictated"));
    assert!(
        state.lock().unwrap().writes.is_empty(),
        "an unreadable binary selection must remain untouched"
    );
}

#[test]
#[cfg(unix)]
fn failed_helper_is_waited_and_reported_as_failure() {
    let candidate = Candidate {
        program: "sh",
        read_args: &[],
        write_args: &["-c", "exec 0<&-; exit 7"],
    };
    assert!(!run_write(candidate, "dictated"));
}

#[test]
#[cfg(target_os = "linux")]
fn successful_read_caches_backend_for_the_next_write() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    {
        let mut data = state.lock().unwrap();
        data.read_results.insert("wl-paste", None);
        data.read_results
            .insert("xclip", Some("previous".to_owned()));
        data.write_results.insert("xclip", VecDeque::from([true]));
    }
    let mut clipboard = clipboard_with(state.clone(), linux_candidates(true));

    assert_eq!(clipboard.read().as_deref(), Some("previous"));
    assert!(clipboard.write("æøå"));

    let data = state.lock().unwrap();
    assert_eq!(data.reads, ["wl-paste", "xclip"]);
    assert_eq!(data.writes, ["xclip:æøå"]);
}

#[test]
#[cfg(target_os = "linux")]
fn failed_cached_write_falls_back_and_caches_successful_backend() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    {
        let mut data = state.lock().unwrap();
        data.read_results
            .insert("wl-paste", Some("previous".to_owned()));
        data.write_results
            .insert("wl-paste", VecDeque::from([false]));
        data.write_results
            .insert("xclip", VecDeque::from([true, true]));
    }
    let mut clipboard = clipboard_with(state.clone(), linux_candidates(true));

    assert_eq!(clipboard.read().as_deref(), Some("previous"));
    assert!(clipboard.write("first"));
    assert!(clipboard.write("second"));

    let data = state.lock().unwrap();
    assert_eq!(
        data.writes,
        ["wl-paste:first", "xclip:first", "xclip:second"]
    );
}

#[test]
#[cfg(not(target_os = "linux"))]
fn non_linux_clipboard_is_explicitly_unavailable() {
    let state = Arc::new(Mutex::new(FakeState::default()));
    let mut clipboard = clipboard_with(state.clone(), Vec::new());

    assert_eq!(clipboard.read(), None);
    assert!(!clipboard.write("text"));
    let data = state.lock().unwrap();
    assert!(data.reads.is_empty());
    assert!(data.writes.is_empty());
}
