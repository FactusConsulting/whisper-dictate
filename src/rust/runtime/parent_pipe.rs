//! Parent-liveness pipe for process-isolated runtime helpers.

use std::io::Read;
use std::thread::{self, JoinHandle};

pub(super) fn start_stdin_eof_exit_watchdog() -> Result<(), String> {
    spawn_eof_watchdog(std::io::stdin(), || std::process::exit(0)).map(|_| ())
}

pub(super) fn spawn_eof_watchdog<R, F>(
    mut reader: R,
    on_closed: F,
) -> Result<JoinHandle<()>, String>
where
    R: Read + Send + 'static,
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new()
        .name("hotkey-probe-parent-watch".to_owned())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            loop {
                match reader.read(&mut byte) {
                    Ok(0) | Err(_) => {
                        on_closed();
                        return;
                    }
                    Ok(_) => {}
                }
            }
        })
        .map_err(|err| format!("could not start hotkey diagnostic parent watchdog: {err}"))
}
