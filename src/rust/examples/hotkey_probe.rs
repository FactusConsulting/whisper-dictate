//! Manual verification harness for the PTT hotkey listener.
//!
//! Proves the OS key listener (rdev on X11, evdev on Wayland — chosen by
//! `hotkey::manager::spawn`) actually observes the configured chord and drives
//! the coordinator. Prints the backend it picked and every coordinator action
//! (StartRecording / StopAndTranscribe / CancelRecording) as you press keys.
//!
//! Build & run (needs the system X11 headers rdev links — `libxi-dev`,
//! `libxtst-dev`, `libx11-dev` — even on Wayland, because the feature always
//! compiles the rdev backend):
//!
//! ```sh
//! PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig \
//!   cargo run --features rust-hotkeys --example hotkey_probe -- shift_l+ctrl_l
//! ```
//!
//! With no argument it reads `key` from `~/.config/whisper-dictate/config.json`.
//! Press the chord a few times; each press should log `StartRecording` and each
//! release `StopAndTranscribe`. Ctrl-C to quit.

#[cfg(not(feature = "rust-hotkeys"))]
fn main() {
    eprintln!("rebuild with `--features rust-hotkeys` to run this probe");
    std::process::exit(2);
}

#[cfg(feature = "rust-hotkeys")]
fn main() {
    use std::io::Read;
    use whisper_dictate_app::hotkey::coordinator::{CoordinatorAction, CoordinatorEvent};
    use whisper_dictate_app::hotkey::{install_hotkey, HotkeyConfig};

    // Chord: CLI arg wins, else the user's saved config, else the default.
    let chord = std::env::args().nth(1).unwrap_or_else(read_config_key);
    let key_names: Vec<String> = chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();

    eprintln!("[probe] session: XDG_SESSION_TYPE={:?} WAYLAND_DISPLAY={:?}",
        std::env::var("XDG_SESSION_TYPE").ok(),
        std::env::var("WAYLAND_DISPLAY").ok());
    eprintln!("[probe] installing listener for chord: {key_names:?}");

    // Bridge StopAndTranscribe → a channel so a helper thread can simulate the
    // host finishing transcription (`ProcessingFinished`). Without it the
    // coordinator parks in `Processing` after the first chord and later presses
    // are correctly ignored — real transcription in the worker sends this.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<u64>();
    let handle = match install_hotkey(HotkeyConfig::hold_to_talk(key_names), move |action| {
        // The sink runs on the coordinator thread; keep it cheap.
        println!("[probe] coordinator action: {action:?}");
        if let CoordinatorAction::StopAndTranscribe(id) = action {
            let _ = done_tx.send(id);
        }
    }) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[probe] install failed: {e}");
            std::process::exit(1);
        }
    };

    // Helper: after each StopAndTranscribe, wait a beat (mock transcription)
    // then release the Processing stage so the NEXT chord fires again.
    let coord = handle.coordinator_handle();
    std::thread::spawn(move || {
        for id in done_rx {
            std::thread::sleep(std::time::Duration::from_millis(300));
            println!("[probe] (mock) transcription {id} done → ProcessingFinished");
            coord.send(CoordinatorEvent::ProcessingFinished(id));
        }
    });

    eprintln!(
        "[probe] listener installed. Press your chord now — you should see \
         StartRecording on press and StopAndTranscribe on release. Ctrl-C to quit."
    );

    // Block forever; the listener + coordinator run on their own threads.
    let mut buf = [0u8; 1];
    let _ = std::io::stdin().read(&mut buf);
    handle.shutdown();
}

/// Read the `key` field from the saved config, defaulting to the shipping
/// default chord if the file is missing or unparseable.
#[cfg(feature = "rust-hotkeys")]
fn read_config_key() -> String {
    let default = "shift_r+ctrl_r".to_owned();
    let Some(home) = std::env::var_os("HOME") else {
        return default;
    };
    let path = std::path::Path::new(&home).join(".config/whisper-dictate/config.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return default;
    };
    // Tiny hand-parse to avoid pulling serde into the example: find "key": "...".
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"key\"") {
            if let Some(colon) = rest.find(':') {
                let val = rest[colon + 1..].trim().trim_end_matches(',').trim();
                let val = val.trim_matches('"');
                if !val.is_empty() {
                    return val.to_owned();
                }
            }
        }
    }
    default
}
