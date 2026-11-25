//! Preserve whisper.cpp logging and capture synchronous GPU evidence from
//! the specific model-load or state-creation call that produced it.

use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr};
use std::io::Write;
use std::panic::catch_unwind;
use std::sync::Once;

use whisper_rs::whisper_rs_sys::ggml_log_level;

/// Guards the one-time `whisper_log_set` install. whisper.cpp keeps the
/// callback in process-global state, so a second install would be
/// harmless but pointless; `Once` also makes the call safe to make from
/// every `LocalWhisper::with_policy` without synchronising callers.
static INSTALL: Once = Once::new();

/// Redirect whisper.cpp + ggml logging through [`log_trampoline`].
pub(crate) fn install() {
    INSTALL.call_once(|| {
        // SAFETY: `log_trampoline` is `extern "C"`, does not unwind (its
        // body is wrapped in `catch_unwind`), and does not dereference
        // `user_data` -- which we pass as null. whisper.cpp stores the
        // pointer in process-global state and may invoke it from any
        // thread for the rest of the process lifetime; a `'static` fn
        // item satisfies that.
        unsafe { whisper_rs::set_log_callback(Some(log_trampoline), std::ptr::null_mut()) };
    });
}

#[derive(Clone, Copy, Default)]
pub(super) struct GpuEvidence {
    pub(super) resident_mib: u32,
    pub(super) state: GpuState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum GpuState {
    #[default]
    Unknown,
    Vulkan,
    Failed,
}

fn vulkan_index(name: &str) -> Option<i32> {
    name.strip_prefix("Vulkan")?.parse().ok()
}

impl GpuEvidence {
    fn note(&mut self, index: i32, line: &str) {
        if let Some(message) = line.strip_prefix("whisper_backend_init_gpu: ") {
            let message = message.trim_end();
            if message == "no GPU found" || message.starts_with("failed to initialize ") {
                self.state = GpuState::Failed;
            } else if self.state != GpuState::Failed
                && message
                    .strip_prefix("using ")
                    .and_then(|name| name.strip_suffix(" backend"))
                    .and_then(vulkan_index)
                    == Some(index)
            {
                self.state = GpuState::Vulkan;
            }
        }
        if let Some(size) = line
            .strip_prefix("whisper_model_load: ")
            .and_then(|line| line.trim_start().split_once(" total size = "))
            .filter(|(device, _)| vulkan_index(device) == Some(index))
            .map(|(_, size)| size)
            .and_then(|size| size.trim().strip_suffix(" MB"))
            .and_then(|size| size.trim().parse::<f64>().ok())
        {
            // Upstream logs decimal MB rounded to two places. Credit only
            // whole MiB and subtract half a rounding unit first.
            let mib = ((size - 0.005) * (1_000_000.0 / 1_048_576.0)) as u32;
            self.resident_mib = self.resident_mib.saturating_add(mib);
        }
    }
}

thread_local! {
    static CAPTURE: Cell<Option<(i32, GpuEvidence)>> = const { Cell::new(None) };
}

/// Collect only logs from this call and this thread. Restore the previous
/// scope even if the native wrapper panics.
pub(super) fn observe<T>(index: i32, f: impl FnOnce() -> T) -> (T, GpuEvidence) {
    struct Restore(Option<(i32, GpuEvidence)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAPTURE.set(self.0);
        }
    }

    install();
    let _restore = Restore(CAPTURE.replace(Some((index, GpuEvidence::default()))));
    let result = f();
    let (_, evidence) = CAPTURE.get().expect("observation scope is active");
    (result, evidence)
}

fn note(line: &str) {
    CAPTURE.with(|slot| {
        if let Some((index, mut evidence)) = slot.get() {
            evidence.note(index, line);
            slot.set(Some((index, evidence)));
        }
    });
}

/// Tee whisper.cpp / ggml logging to stderr and the current capture scope.
unsafe extern "C" fn log_trampoline(
    _level: ggml_log_level,
    text: *const c_char,
    _user_data: *mut c_void,
) {
    // A panic unwinding into C++ is UB; swallow anything that escapes.
    let _ = catch_unwind(|| {
        if text.is_null() {
            return;
        }
        // SAFETY: whisper.cpp always passes a NUL-terminated C string
        // owned by its own formatting buffer, valid for the duration of
        // the callback. We copy what we need before returning.
        let raw = unsafe { CStr::from_ptr(text) };
        let line = raw.to_string_lossy();
        // Tee first so a classifier change can never cost us the human-
        // readable banner. Errors are dropped on purpose: stderr may be
        // closed (GUI subsystem) or redirected to DEVNULL (the
        // `transcribe-server` helper), neither of which is actionable
        // from inside a log callback.
        let _ = std::io::stderr().write_all(line.as_bytes());
        note(&line);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "whisper_backend_init_gpu: using Vulkan1 backend\n";

    #[test]
    fn state_requires_the_selected_vulkan_backend_in_the_current_scope() {
        let (_, wrong_device) = observe(0, || note(DEVICE));
        assert_eq!(wrong_device.state, GpuState::Unknown);

        let (_, found) = observe(1, || note(DEVICE));
        assert_eq!(found.state, GpuState::Vulkan);

        note(DEVICE);
        let (_, fresh) = observe(1, || std::thread::spawn(|| note(DEVICE)).join().unwrap());
        assert_eq!(fresh.state, GpuState::Unknown);
    }

    #[test]
    fn state_fallback_overrides_an_attempted_gpu_initialization() {
        for failure in [
            "whisper_backend_init_gpu: failed to initialize Vulkan1 backend\n",
            "whisper_backend_init_gpu: no GPU found\n",
        ] {
            let (_, evidence) = observe(1, || {
                note(DEVICE);
                note(failure);
                note(DEVICE);
            });
            assert_eq!(evidence.state, GpuState::Failed);
        }
    }

    #[test]
    fn model_load_credits_only_verified_device_local_weights() {
        let (_, evidence) = observe(1, || {
            note("whisper_model_load:      Vulkan0 total size =   100.00 MB\n");
            note("whisper_model_load:          CPU total size =   100.00 MB\n");
            note("whisper_model_load:      Vulkan1 total size =   100.00 MB\n");
        });
        assert_eq!(evidence.resident_mib, 95);
        assert_eq!(evidence.state, GpuState::Unknown);
    }
}
