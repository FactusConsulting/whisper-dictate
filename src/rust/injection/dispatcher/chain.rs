//! Linux helper selection and the no-retry barrier after possible progress.

use super::outcome::InjectOutcome;
use crate::injection::fallback::{locate_on_path, HelperError, LinuxSession};
use anyhow::anyhow;

/// Try each available helper, in chain order, until one succeeds.
///
/// Candidates come from [`available_helpers`], the same list the single-pick
/// selectors delegate to, so the runtime fallback cannot disagree with them
/// about what is eligible or in which order.
///
/// Fallback safety has two independent gates:
///
/// 1. **Progress signal** ([`HelperError::partial`]): the ydotool path
///    tracks how many evdev ops landed before the failure and marks
///    `partial: true` when any keystroke reached the compositor. That kind
///    of failure is NEVER retried -- the next helper would re-type the
///    successful prefix on top of it.
/// 2. **Text signal** ([`is_safe_to_try_next_helper`]) for subprocess
///    helpers whose partial state we can't observe from Rust. Only KNOWN
///    startup / capability signatures whitelist a retry; anything
///    unrecognised stops the chain, matching the pre-tracking behaviour.
#[cfg(target_os = "linux")]
pub(super) fn try_helpers<A>(
    session: LinuxSession,
    mut attempt: A,
    paste_capable_only: bool,
    what: &str,
) -> InjectOutcome
where
    A: FnMut(&str) -> std::result::Result<(), HelperError>,
{
    let candidates =
        crate::injection::fallback::available_helpers(session, locate_on_path, paste_capable_only);
    if candidates.is_empty() {
        return InjectOutcome::failed(anyhow!(
            "no Linux {what} helper found on PATH (tried: {:?})",
            crate::injection::fallback::fallback_chain(session)
        ));
    }
    try_helpers_over(&candidates, &mut attempt, what)
}

/// Pure walk over a candidate list. Split from [`try_helpers`] so tests can
/// exercise the retry / partial-failure logic against a fake `attempt`
/// closure without touching `$PATH` or spawning subprocesses.
/// dispatcher.rs:521 -- runtime fallback needs a regression test.
#[cfg(target_os = "linux")]
fn try_helpers_over<A>(candidates: &[&str], attempt: &mut A, what: &str) -> InjectOutcome
where
    A: FnMut(&str) -> std::result::Result<(), HelperError>,
{
    if candidates.is_empty() {
        return InjectOutcome::failed(anyhow!("no Linux {what} helper available"));
    }

    let mut last_err: Option<anyhow::Error> = None;
    for (idx, helper) in candidates.iter().copied().enumerate() {
        match attempt(helper) {
            Ok(()) => {
                if idx > 0 {
                    // Worth a line: the helper the operator would expect to
                    // be used did not work, and knowing WHICH one carried
                    // the text is the difference between "it works" and
                    // "it works for a reason I can reproduce".
                    // dispatcher.rs:531 -- surface fallback diagnostics.
                    eprintln!(
                        "[inject] {what}: {helper} succeeded after {:?} failed",
                        &candidates[..idx]
                    );
                }
                return InjectOutcome::ok();
            }
            Err(HelperError {
                err, partial: true, ..
            }) => {
                // The helper had already pushed at least one keystroke
                // through the compositor when it died. Falling back to
                // the next helper would re-type the successful prefix on
                // top of it, silently corrupting the user's document.
                // dispatcher.rs:540 -- suppress fallback on
                // any observable partial injection.
                //
                // The `partial=true` flag is propagated all the way up to
                // `InjectResponse.partial` so the caller's outer fallback
                // can also stand down instead of
                // re-typing the transcript on top of the successful prefix.
                eprintln!(
                    "[inject] {what}: {helper} failed AFTER typing keys ({err:#}); \
                     suppressing fallback to avoid double-typing"
                );
                return InjectOutcome::partial(err);
            }
            Err(HelperError {
                err,
                partial: false,
                known_no_progress,
            }) => {
                let text = format!("{err:#}");
                if !crate::injection::fallback::is_safe_to_try_next_helper(&text) {
                    // Unrecognised subprocess failure: it may have typed
                    // part of the text. Stop rather than risk duplicating.
                    //
                    // Once we're past
                    // the first candidate the chain has necessarily
                    // *invoked* one or more subprocess helpers, so any
                    // later opaque failure is "possibly partial":
                    // we cannot prove nothing
                    // reached the compositor, and the outer fallback
                    // would re-type the whole transcript on top. Stamp
                    // `partial=true` in that case so the caller's
                    // fallback stands down. For `idx == 0` the failed
                    // helper never typed anything --
                    // return without the partial stamp so the caller's
                    // fallback can safely re-type the transcript.
                    //
                    // `known_no_progress` overrides the idx>0 assumption:
                    // when the current helper positively proved nothing
                    // reached the compositor (ydotool `sent == 0`), we
                    // MUST NOT stamp `partial=true`, or the outer
                    // fallback stands down and the transcript is lost
                    // even though we know it never landed anywhere.
                    return if idx > 0 && !known_no_progress {
                        InjectOutcome::partial(err)
                    } else {
                        InjectOutcome::failed(err)
                    };
                }
                eprintln!("[inject] {what}: {helper} unusable ({text}); trying next helper");
                last_err = Some(err);
            }
        }
    }

    InjectOutcome::failed(
        last_err
            .unwrap_or_else(|| anyhow!("no Linux {what} helper produced a result"))
            .context(format!(
                "every Linux {what} helper failed (tried: {candidates:?})"
            )),
    )
}

/// Convert a `wayland::type_text_tracked` failure into the
/// [`HelperError`] shape the fallback chain consumes. Always stamps
/// `partial: true` because the failure could carry a partial write
/// regardless of the ops-completed count.
///
/// `sent > 0` proves at least one prior evdev op succeeded — the
/// compositor already has keystrokes, so the outer fallback would
/// double-type. `sent == 0` looks like "nothing landed" but is not:
/// the single failing `ydotool type -- <buffer>` subprocess can
/// stream part of its Unicode payload through ydotoold before
/// exiting nonzero, and `run_ydotool` sees only the whole-process
/// exit status — that is indistinguishable from a spawn-time
/// refusal, so we must assume a partial write to stay safe.
///
/// The stamp matters at any candidate index. When ydotool is the
/// only installed helper (`available_helpers` places it at idx=0),
/// an `opaque` failure slips past `try_helpers_over` s `idx > 0`
/// gate and the caller's outer fallback then double-types on top of
/// whatever leaked into the compositor. Split into a free function
/// so the invariant is unit-testable without a live ydotool
/// subprocess.
///
/// review r3663766083.
#[cfg(target_os = "linux")]
pub(super) fn ydotool_failure_to_helper_error(err: anyhow::Error, _sent: usize) -> HelperError {
    HelperError::partial(err)
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
