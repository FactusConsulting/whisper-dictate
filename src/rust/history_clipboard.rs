use anyhow::{anyhow, Result};
#[cfg(any(not(target_os = "windows"), test))]
use std::process::{Command, Stdio};

#[cfg(not(target_os = "windows"))]
pub(super) fn run(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    run_command(command, text)
}

#[cfg(any(not(target_os = "windows"), test))]
fn run_command(mut command: Command, text: &str) -> Result<()> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    let output = crate::bounded_process::run_clipboard_owner(
        &mut command,
        text.as_bytes().to_vec(),
        crate::bounded_process::HELPER_TIMEOUT,
    )?;
    if output.completion != crate::bounded_process::Completion::Exited {
        return Err(anyhow!("clipboard helper timed out after 10s"));
    }
    if let Some(error) = output.stdin_error {
        return Err(error.into());
    }
    if !output.status.success() {
        return Err(anyhow!("exit status {}", output.status));
    }
    Ok(())
}

/// Publish `text` as `CF_UNICODETEXT` through the Win32 clipboard API.
/// `clip.exe` cannot round-trip non-ASCII text: it decodes stdin through the
/// console code page unless the input starts with a UTF-16 byte-order mark,
/// and then keeps that mark on the clipboard as U+FEFF.
#[cfg(target_os = "windows")]
pub(super) fn set_unicode_text(text: &str) -> Result<()> {
    use windows_sys::Win32::Foundation::{GetLastError, GlobalFree};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    let units = unicode_text_units(text);
    // SAFETY: the allocation is sized for `units`, written only while locked,
    // and freed here unless SetClipboardData succeeded and the system owns it.
    unsafe {
        let failed = |step: &str| anyhow!("{step} failed; GetLastError=0x{:08x}", GetLastError());
        let handle = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(units.as_slice()));
        if handle.is_null() {
            return Err(failed("GlobalAlloc"));
        }
        let target = GlobalLock(handle).cast::<u16>();
        if target.is_null() {
            let error = failed("GlobalLock");
            GlobalFree(handle);
            return Err(error);
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
        GlobalUnlock(handle);
        // Another process (clipboard managers, RDP, the app being pasted into)
        // can hold the clipboard open briefly; retry for up to ~500 ms.
        let mut opened = OpenClipboard(std::ptr::null_mut()) != 0;
        for _ in 0..25 {
            if opened {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            opened = OpenClipboard(std::ptr::null_mut()) != 0;
        }
        if !opened {
            let error = failed("OpenClipboard");
            GlobalFree(handle);
            return Err(error);
        }
        let result = if EmptyClipboard() == 0 {
            Err(failed("EmptyClipboard"))
        } else if SetClipboardData(u32::from(CF_UNICODETEXT), handle).is_null() {
            Err(failed("SetClipboardData"))
        } else {
            Ok(())
        };
        CloseClipboard();
        if result.is_err() {
            GlobalFree(handle);
        }
        result
    }
}

/// `CF_UNICODETEXT` payload: UTF-16 code units plus the terminating NUL the
/// format requires.
#[cfg(any(target_os = "windows", test))]
fn unicode_text_units(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
#[path = "history_clipboard_tests.rs"]
mod tests;
