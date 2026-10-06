//! The raw listener callback: counters, redaction, tap and guarded dispatch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::super::tracker::{KeyTracker, TrackerOutput};
use super::diagnostics::{
    enqueue_callback_trace, redact_event_type_for_debug, redact_raw_event_name,
    should_log_raw_event,
};
use super::keys::raw_from_rdev;
use super::RawTap;
use crate::hotkey::inject_guard::{dispatch_raw_event, InjectionGuard};

pub(super) struct CallbackContext<F, R> {
    pub listener_tracker: Arc<Mutex<KeyTracker>>,
    pub listener_sink: Arc<F>,
    pub listener_tap: Arc<R>,
    pub listener_guard: Arc<InjectionGuard>,
    pub listener_total: Arc<AtomicU64>,
    pub listener_since: Arc<AtomicU64>,
}

#[cfg(test)]
#[path = "callback_tests.rs"]
mod tests;

impl<F, R> CallbackContext<F, R>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    pub(super) fn handle_event(&self, event: rdev::Event) {
        let Self {
            listener_tracker,
            listener_sink,
            listener_tap,
            listener_guard,
            listener_total,
            listener_since,
        } = self;
        let debug = crate::diag::debug_enabled();
        // Debug: log every rdev event BEFORE name-filter, so
        // an F9 that rdev sees but key_to_name discards is
        // visible. Complements the parallel WH_KEYBOARD_LL
        // hook (raw hook = OS pump delivered vk; this line =
        // rdev's callback fired with a matching variant).
        //
        // Uses [`enqueue_callback_trace`] (bounded, non-blocking)
        // instead of `crate::diag::log!` so a stalled AppData
        // write does not cause Windows to unhook this callback
        // for exceeding the LL-hook time budget — the exact
        // wedge this instrumentation exists to diagnose.
        if debug {
            // Keep the redaction rule (`redact_event_type_for_debug`
            // returns a key identity for PTT-eligible events, else
            // `KeyPress(<redacted>)` / `KeyRelease(<redacted>)`,
            // mouse variants pass through verbatim) AND route
            // it through the off-callback queue so a stalled AppData write
            // cannot exceed Windows' LL-hook time budget and
            // silently unhook the callback.
            enqueue_callback_trace(format!(
                "[rdev/callback] raw={}",
                redact_event_type_for_debug(&event.event_type)
            ));
        }
        if let Some(raw) = raw_from_rdev(&event) {
            // Update counters BEFORE the guard / tracker check
            // the heartbeat records every raw event rdev delivered,
            // even ones self-injection filtering drops. If the
            // guard is armed and the event is swallowed the raw
            // count still moves; if the pump is dead the count
            // stays flat. That's the exact signal we need.
            let n = listener_total.fetch_add(1, Ordering::Relaxed) + 1;
            listener_since.fetch_add(1, Ordering::Relaxed);
            if crate::diag::info_enabled() && should_log_raw_event(n) {
                // Redact non-PTT-eligible names so ordinary desktop
                // typing (passwords/tokens/URLs) doesn't get sampled
                // into gui-diagnostic.log
                // `kind` (Press / Release) stays for the wedge-signal.
                // Off-callback write .
                enqueue_callback_trace(format!(
                    "[hotkey/rdev] raw event #{n}: name={:?} kind={:?}",
                    redact_raw_event_name(&raw.name),
                    raw.kind
                ));
            }
            // Debug: post-name-filter — the value the tracker
            // actually keys off. Mismatch with the raw= line
            // above pinpoints a bug in key_to_name. Redact
            // non-PTT names to keep passwords/tokens out of
            // debug/trace uploads .
            // Off-callback write .
            if debug {
                enqueue_callback_trace(format!(
                    "[rdev/callback] mapped_name={:?} kind={:?}",
                    redact_raw_event_name(&raw.name),
                    raw.kind
                ));
            }
            listener_tap.tap(&raw);
            // `dispatch_raw_event` short-circuits when the guard
            // is armed — the injector's own SendInput bursts get
            // dropped here instead of feeding back into the
            // tracker (Windows PTT wedge; module doc explains
            // why the check must live below the mutex acquire so
            // event ordering is preserved). Fast path when the
            // guard is inactive is two atomic loads + a compare
            // and does NOT allocate — matters because this
            // callback runs on the LL-hook thread for every
            // desktop-wide keydown/keyup.
            let mut t = listener_tracker.lock().expect("tracker poisoned");
            if let Some(out) = dispatch_raw_event(&listener_guard, &mut t, &raw) {
                (listener_sink)(out);
            }
        }
    }
}
