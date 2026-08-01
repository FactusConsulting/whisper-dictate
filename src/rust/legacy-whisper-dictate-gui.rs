#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[path = "whisper-dictate-gui.rs"]
mod wd_gui;

fn main() -> std::process::ExitCode {
    wd_gui::main()
}
