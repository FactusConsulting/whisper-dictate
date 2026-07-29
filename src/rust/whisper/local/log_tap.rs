//! Tap on whisper.cpp's own log stream, so we can tell whether a GPU
//! backend ACTUALLY initialised.
//!
//! # Why a log tap and not an API call
//!
//! whisper.cpp exposes no "which backend did you pick" getter. The one
//! authoritative signal it emits is a log line at model-load time:
//!
//! ```text
//! whisper_backend_init_gpu: using Vulkan0 backend   <- GPU really came up
//! whisper_backend_init_gpu: no GPU found            <- silent CPU fallback
//! ```
//!
//! By default those go straight to stderr via whisper.cpp's built-in
//! callback -- visible to a human reading a terminal, invisible to the
//! program. `whisper_log_set` (surfaced by whisper-rs as
//! [`whisper_rs::set_log_callback`]) replaces that callback, and
//! whisper.cpp forwards the same callback to `ggml_log_set`, so ONE
//! install captures both the whisper and ggml streams.
//!
//! # Contract with the default behaviour
//!
//! Installing a callback SUPPRESSES whisper.cpp's built-in stderr write,
//! so this trampoline re-emits every line to stderr verbatim. Without
//! that, opting into provenance would silently delete the whisper.cpp
//! model-load banner that users and bug reports rely on today. The tap is
//! additive: same bytes on stderr, plus a machine-readable verdict in
//! [`crate::whisper::accel`].
//!
//! # Callback safety
//!
//! The trampoline runs on whatever thread whisper.cpp is loading on, from
//! C++, so it must not unwind. It therefore:
//!
//! * does no allocation-fallible work that can panic (`to_string_lossy`
//!   and the classifier are total),
//! * ignores stderr write errors instead of `expect`-ing them,
//! * wraps the whole body in `catch_unwind` as a backstop -- a panic
//!   crossing the FFI boundary is undefined behaviour, and a diagnostic
//!   tap must never be able to take down a dictation session.

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
///
/// Idempotent and cheap after the first call. MUST be called before
/// `WhisperContext::new_with_params` -- the backend-selection lines are
/// emitted during that call and a callback installed afterwards would
/// miss the only evidence there is.
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

/// whisper.cpp / ggml log callback: tee to stderr (preserving the
/// default behaviour we just displaced) and feed the line to the
/// accelerator classifier.
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
        crate::whisper::accel::global().note_log_line(&line);
    });
}
