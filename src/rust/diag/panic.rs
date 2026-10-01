//! Independent panic-report channel and one process-wide hook.

use super::write_line;
use std::sync::{mpsc, Once, OnceLock};
use std::thread;
use std::time::Duration;

/// Install one process-wide panic hook for the GUI binary.
///
/// A Rust panic normally reaches the hidden GUI process's discarded stderr,
/// which makes an unexpected window exit look like a silent crash. Record the
/// payload, thread name, and source location in the existing diagnostic sink
/// first, then
/// delegate to the hook that was active before startup.
pub fn install_gui_panic_hook() {
    static GUI_PANIC_HOOK: Once = Once::new();
    GUI_PANIC_HOOK.call_once(|| {
        // Set up the dedicated panic writer before publishing the hook.
        // Panic records must not compete with high-rate callback tracing.
        let _ = ensure_panic_writer();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map(|location| (location.file(), location.line(), location.column()));
            // A panic must not wait for the tee mutex OR synchronously touch
            // a potentially wedged diagnostic volume. The dedicated,
            // unbounded channel cannot be filled by ordinary trace traffic;
            // its writer owns file I/O.
            enqueue_panic_report(format_panic_report(info.payload(), location));
            previous(info);
        }));
    });
}

/// Dedicated sender for panic records. This is intentionally separate from
/// the bounded callback-trace queue: preserving the one record that explains
/// a crash matters more than applying trace backpressure to it.
enum PanicRecord {
    Line(String),
    Drain(mpsc::Sender<()>),
}

static PANIC_QUEUE_TX: OnceLock<Result<mpsc::Sender<PanicRecord>, String>> = OnceLock::new();

fn ensure_panic_writer() -> Result<(), String> {
    PANIC_QUEUE_TX
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<PanicRecord>();
            thread::Builder::new()
                .name("vp-panic-diag".to_owned())
                .spawn(move || {
                    while let Ok(record) = rx.recv() {
                        match record {
                            PanicRecord::Line(message) => write_line(&message),
                            PanicRecord::Drain(ack) => {
                                let _ = ack.send(());
                            }
                        }
                    }
                })
                .map(|_| tx)
                .map_err(|err| format!("[panic] diagnostic writer unavailable: {err}"))
        })
        .as_ref()
        .map(|_| ())
        .map_err(Clone::clone)
}

/// Offer a crash record without taking the tee mutex or performing file I/O
/// on the panicking thread. The channel is independent of regular trace
/// traffic so a saturated callback queue cannot discard a panic report.
fn enqueue_panic_report(message: String) {
    if ensure_panic_writer().is_ok() {
        if let Some(Ok(tx)) = PANIC_QUEUE_TX.get() {
            let _ = tx.send(PanicRecord::Line(message));
        }
    }
}

/// Drain panic records already accepted by the dedicated writer, bounded so a
/// wedged diagnostic volume cannot delay process exit indefinitely.
pub fn drain_panic_reports(deadline: Duration) -> bool {
    let Some(Ok(tx)) = PANIC_QUEUE_TX.get() else {
        return true;
    };
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(PanicRecord::Drain(ack_tx)).is_ok() && ack_rx.recv_timeout(deadline).is_ok()
}

pub(crate) fn format_panic_report(
    payload: &(dyn std::any::Any + Send),
    location: Option<(&str, u32, u32)>,
) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    let message = escape_panic_field(message);
    let thread = escape_panic_field(std::thread::current().name().unwrap_or("unnamed"));
    match location {
        Some((file, line, column)) => {
            format!("[panic] Rust panic thread={thread} at {file}:{line}:{column}: {message}")
        }
        None => format!("[panic] Rust panic thread={thread}: {message}"),
    }
}

/// Keep a panic report as one physical diagnostic record. Panic payloads are
/// allowed to contain newlines (for example assertion messages), but an
/// unescaped newline would make continuation text look like a separate log
/// record without its timestamp and category.
fn escape_panic_field(value: &str) -> String {
    value.replace('\r', "\\r").replace('\n', "\\n")
}
