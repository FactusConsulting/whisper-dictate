//! Real subprocess fixtures without depending on shell startup or installed tools.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(test)]
pub(crate) fn command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "bounded_process::fixtures::helper_process_fixture",
            "--nocapture",
        ])
        .env("WD_HELPER_TEST_ROLE", role)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn helper_process_fixture() {
    let Ok(role) = std::env::var("WD_HELPER_TEST_ROLE") else {
        return;
    };
    if let Some(path) = std::env::var_os("WD_HELPER_TEST_PID_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    match role.as_str() {
        "echo" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            std::io::stdout().write_all(&input).unwrap();
        }
        "sleep" => std::thread::sleep(Duration::from_secs(60)),
        "blocked-input" => std::thread::sleep(Duration::from_secs(2)),
        "short-sleep" => std::thread::sleep(Duration::from_millis(800)),
        "flood" => {
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();
            for _ in 0..128 {
                stdout.write_all(&[b'o'; 8192]).unwrap();
                stderr.write_all(&[b'e'; 8192]).unwrap();
            }
            stdout.write_all(b"stdout-finished").unwrap();
            stderr.write_all(b"stderr-finished").unwrap();
        }
        "descendant" | "clipboard-owner" => {
            if role == "clipboard-owner" {
                std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
            }
            let mut child = command(if role == "clipboard-owner" {
                "short-sleep"
            } else {
                "sleep"
            });
            child.stdin(Stdio::null());
            if role == "clipboard-owner" {
                child.stdout(Stdio::null()).stderr(Stdio::null());
            } else {
                child.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            }
            child.env_remove("WD_HELPER_TEST_PID_FILE");
            // Deliberately outlive the parent: the runner must terminate it,
            // or preserve it only for a successful clipboard publication.
            #[allow(clippy::zombie_processes)]
            let descendant = child.spawn().unwrap();
            if let Some(path) = std::env::var_os("WD_HELPER_TEST_DESCENDANT_FILE") {
                std::fs::write(path, descendant.id().to_string()).unwrap();
            }
            // Deliberately leave a descendant and inherited pipes alive.
        }
        "fail" => {
            std::io::stderr().write_all(b"fixture-failed").unwrap();
            std::process::exit(7);
        }
        other => panic!("unexpected helper fixture role: {other}"),
    }
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    std::process::exit(0);
}

#[cfg(test)]
pub(crate) fn pid_file(path: &std::path::Path) -> u32 {
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

#[cfg(test)]
pub(crate) fn is_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: read-only wait access to this fixture's recorded child PID.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return false;
        }
        let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
        unsafe { CloseHandle(process) };
        running
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, tail)| tail.to_owned()))
            .is_some_and(|tail| !tail.starts_with('Z') && !tail.starts_with('X'))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: signal zero checks existence without sending a signal.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}
