//! Live `SendInput` coverage for the production enigo typing path.
//!
//! A low-level keyboard hook on a helper thread captures every event enigo
//! injects (tagged with `enigo::EVENT_MARKER`) and swallows it, so the test
//! needs no focused window and never types into the desktop. Physical and
//! foreign injected keys pass through untouched.

use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP,
    MSG, WH_KEYBOARD_LL, WM_QUIT,
};

const VK_PACKET: u32 = 0xE7;

/// `(vk, scan, is_key_up)` for every captured enigo event, in arrival order.
static CAPTURED: Mutex<Vec<(u32, u32, bool)>> = Mutex::new(Vec::new());

unsafe extern "system" fn capture_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = &*(lparam as *const KBDLLHOOKSTRUCT);
        if event.flags & LLKHF_INJECTED != 0 && event.dwExtraInfo == enigo::EVENT_MARKER as usize {
            CAPTURED
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((event.vkCode, event.scanCode, event.flags & LLKHF_UP != 0));
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

/// Owns the hook thread; dropping it unhooks and joins.
struct CaptureHook {
    thread_id: u32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CaptureHook {
    fn install() -> Self {
        CAPTURED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || unsafe {
            let hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(capture_hook), std::ptr::null_mut(), 0);
            ready_tx
                .send((GetCurrentThreadId(), !hook.is_null()))
                .expect("test thread waits for the hook");
            if hook.is_null() {
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(hook);
        });
        let (thread_id, installed) = ready_rx.recv().expect("hook thread reports readiness");
        assert!(installed, "SetWindowsHookExW(WH_KEYBOARD_LL) failed");
        Self {
            thread_id,
            handle: Some(handle),
        }
    }

    /// Wait until `count` events arrived or `timeout` elapsed.
    fn wait_for(&self, count: usize, timeout: Duration) -> Vec<(u32, u32, bool)> {
        let deadline = Instant::now() + timeout;
        loop {
            let captured = CAPTURED
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if captured.len() >= count || Instant::now() >= deadline {
                return captured;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for CaptureHook {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Characters that need Shift/AltGr on common layouts, layout-specific
/// punctuation, non-ASCII letters, and a surrogate pair must reach the target
/// as their exact UTF-16 code units, independent of the active layout.
/// Windows delivers the character on key-down; every key-down must still be
/// paired with a key-up so no key is left logically held.
#[test]
fn typing_injects_exact_utf16_code_units_regardless_of_layout() {
    let text = "After I'll Inject: \"Take\" (ÆØÅ) é@€ 351. \u{1F600}";
    let hook = CaptureHook::install();

    let mut backend = super::make_default_backend().expect("enigo backend");
    backend
        .type_text_cancellable(text, &|| true)
        .expect("typing succeeds");

    let units: Vec<u32> = text.encode_utf16().map(u32::from).collect();
    let captured = hook.wait_for(units.len() * 2, Duration::from_secs(5));
    let downs: Vec<(u32, u32)> = captured
        .iter()
        .filter(|(_, _, up)| !up)
        .map(|&(vk, scan, _)| (vk, scan))
        .collect();
    let ups = captured
        .iter()
        .filter(|(vk, _, up)| *up && *vk == VK_PACKET);
    let expected: Vec<(u32, u32)> = units.iter().map(|&unit| (VK_PACKET, unit)).collect();
    assert_eq!(downs, expected);
    assert_eq!(ups.count(), units.len(), "captured: {captured:?}");
}
