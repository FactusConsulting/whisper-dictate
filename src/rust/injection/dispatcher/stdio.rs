//! Stdin/stdout orchestration for the native injection CLI.

use super::protocol::{resolve_method, response_for_outcome};
use super::{InjectRequest, Injector, ProbeResponse};
#[cfg(target_os = "linux")]
use crate::injection::fallback::{locate_on_path, select_helper, LinuxSession};
use anyhow::Result;
use std::io::{self, Read};

/// Entry point for the `whisper-dictate inject` subcommand.
pub fn handle_inject() -> Result<()> {
    let request = read_request()?;
    match request {
        InjectRequest::Probe => {
            let body = probe_backend();
            println!("{}", serde_json::to_string(&body)?);
        }
        InjectRequest::Inject {
            text,
            method,
            target_title,
            target_process,
            xkb_layout,
        } => {
            let mut injector = Injector::new()
                .with_target(&target_title, &target_process)
                .with_xkb_layout(&xkb_layout);
            let method = resolve_method(&method)?;
            let outcome = injector.inject_text_ex(&text, method);
            let response = response_for_outcome(method, outcome);
            println!("{}", serde_json::to_string(&response)?);
        }
    }
    Ok(())
}

fn probe_backend() -> ProbeResponse {
    #[cfg(target_os = "linux")]
    let (session, helper) = {
        let s = LinuxSession::detect();
        (
            Some(format!("{s:?}")),
            select_helper(s, locate_on_path).map(str::to_owned),
        )
    };
    #[cfg(not(target_os = "linux"))]
    let (session, helper) = (None, None);

    ProbeResponse {
        platform: std::env::consts::OS.to_owned(),
        linux_session: session,
        linux_helper: helper,
        feature_enabled: cfg!(feature = "rust-injection"),
    }
}

fn read_request() -> Result<InjectRequest> {
    read_request_from(io::stdin())
}

fn read_request_from(mut input: impl Read) -> Result<InjectRequest> {
    let mut raw = String::new();
    input.read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
