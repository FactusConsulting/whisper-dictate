//! Terminal output formatting for the native dictation driver.

use crate::runtime::{RuntimeEvent, WorkerEvent};

const OUTPUT_PREFIX: &str = "[dictate-run]";

pub(super) fn ready_line(json: bool, chord: &str, driver: &'static str) -> String {
    if json {
        serde_json::json!({
            "kind": "ready",
            "ready": true,
            "engine": "rust",
            "chord": chord,
            "driver": driver,
        })
        .to_string()
    } else {
        format!("{OUTPUT_PREFIX} ready (engine=rust, driver={driver}, chord={chord})")
    }
}

pub(super) fn event_json_value(event: &RuntimeEvent) -> serde_json::Value {
    match event {
        RuntimeEvent::Worker(WorkerEvent { event, payload, .. }) if event == "utterance" => {
            payload.clone()
        }
        RuntimeEvent::Worker(w) => serde_json::json!({
            "kind": "worker",
            "event": w.event,
            "state": w.state,
            "payload": w.payload,
        }),
        RuntimeEvent::Started { command } => {
            serde_json::json!({"kind": "started", "command": command})
        }
        RuntimeEvent::Stdout(line) => serde_json::json!({"kind": "stdout", "line": line}),
        RuntimeEvent::Stderr(line) => serde_json::json!({"kind": "stderr", "line": line}),
        RuntimeEvent::Exited { code } => serde_json::json!({"kind": "exited", "code": code}),
        RuntimeEvent::Error(msg) => serde_json::json!({"kind": "error", "message": msg}),
    }
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(super) fn emit_ready(json: bool, chord: &str, driver: &'static str) {
    println!("{}", ready_line(json, chord, driver));
    flush_stdout();
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(super) fn emit_shutdown(json: bool, reason: &str) {
    if json {
        println!(
            "{}",
            serde_json::json!({"kind": "shutdown", "reason": reason})
        );
    } else {
        println!("{OUTPUT_PREFIX} shutdown (reason={reason})");
    }
    flush_stdout();
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
pub(super) fn emit_event(json: bool, event: &RuntimeEvent) {
    if json {
        println!("{}", event_json_value(event));
    } else {
        match event {
            RuntimeEvent::Worker(w) => println!(
                "{OUTPUT_PREFIX} worker event={} state={:?}",
                w.event, w.state
            ),
            RuntimeEvent::Started { command } => println!("{OUTPUT_PREFIX} started ({command})"),
            RuntimeEvent::Stdout(line) => println!("{OUTPUT_PREFIX} stdout: {line}"),
            RuntimeEvent::Stderr(line) => println!("{OUTPUT_PREFIX} stderr: {line}"),
            RuntimeEvent::Exited { code } => println!("{OUTPUT_PREFIX} exited (code={code:?})"),
            RuntimeEvent::Error(msg) => println!("{OUTPUT_PREFIX} error: {msg}"),
        }
    }
    flush_stdout();
}

#[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
fn flush_stdout() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
#[path = "dictate_run_output_tests.rs"]
mod tests;
