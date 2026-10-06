//! The hidden `devices` CLI sub-command: JSON request parsing and the
//! stdout response envelope.
use std::io::{self, IsTerminal, Read};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    default_input_device, find_device_by_name, list_input_devices,
    list_input_devices_with_directsound, DeviceInfo,
};

// ----- CLI handler ------------------------------------------------------------

/// JSON request envelope for the hidden `devices` sub-command. Mirrors the
/// shape `handle_health` uses (action-tagged enum) so external callers
/// can pick the operation it wants without parsing multiple positional args.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum DevicesRequest {
    /// List every input device. `include_directsound` (default false) asks for
    /// the Windows DirectSound-only inputs to be merged in — set ONLY by the
    /// sounddevice picker, which can open them; the standalone CLI / TTY / empty
    /// callers leave it false so every listed device is cpal-openable.
    List {
        #[serde(default)]
        include_directsound: bool,
    },
    /// Return the host's default input device (or `null`).
    Default,
    /// Resolve a saved name against the live device list.
    Find { query: String },
}

#[derive(Debug, Serialize)]
struct ListResponse {
    devices: Vec<DeviceInfo>,
}

#[derive(Debug, Serialize)]
struct DefaultResponse {
    device: Option<DeviceInfo>,
}

#[derive(Debug, Serialize)]
struct FindResponse {
    device: Option<DeviceInfo>,
}

/// Pure resolver for [`handle_devices`]. Given whether stdin is a TTY and,
/// when it isn't, the raw stdin body, decide which [`DevicesRequest`] to
/// serve. Split out so the TTY / pipe / bad-JSON branches are unit-testable
/// without a real console or a piped subprocess.
///
/// Contract:
/// * `stdin_is_tty = true` → always [`DevicesRequest::List`] (the interactive
///   convenience — see [`handle_devices`] doc for why).
/// * `stdin_is_tty = false` + empty body → [`DevicesRequest::List`]
///   (documented shorthand for external callers).
/// * `stdin_is_tty = false` + non-empty body → parse as JSON, propagate the
///   parse error.
fn resolve_devices_request(stdin_is_tty: bool, stdin_body: Option<&str>) -> Result<DevicesRequest> {
    if stdin_is_tty {
        return Ok(DevicesRequest::List {
            include_directsound: false,
        });
    }
    let trimmed = stdin_body.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(DevicesRequest::List {
            include_directsound: false,
        });
    }
    Ok(serde_json::from_str(trimmed)?)
}

/// Handler for the hidden `devices` sub-command. Reads a JSON request from
/// stdin and writes a JSON response on stdout.
///
/// Accepts an empty / missing stdin body as a shorthand for
/// `{"action":"list"}` so callers that just want the list can pipe nothing in.
///
/// When stdin is an interactive TTY (nothing piped in) we skip the blocking
/// read entirely and default to `List` — otherwise a user typing
/// `wd devices` from PowerShell would see the process hang
/// waiting for keyboard input until they hit Ctrl+Z. Piped callers and
/// `... | wd devices` pipelines still hit the read path because
/// their stdin is not a TTY.
pub fn handle_devices() -> Result<()> {
    let stdin = io::stdin();
    let stdin_is_tty = stdin.is_terminal();
    let raw = if stdin_is_tty {
        None
    } else {
        let mut buf = String::new();
        stdin.lock().read_to_string(&mut buf)?;
        Some(buf)
    };
    let request = resolve_devices_request(stdin_is_tty, raw.as_deref())?;
    match request {
        DevicesRequest::List {
            include_directsound,
        } => {
            let devices = if include_directsound {
                list_input_devices_with_directsound()
            } else {
                list_input_devices()
            };
            let resp = ListResponse { devices };
            println!("{}", serde_json::to_string(&resp)?);
        }
        DevicesRequest::Default => {
            let resp = DefaultResponse {
                device: default_input_device(),
            };
            println!("{}", serde_json::to_string(&resp)?);
        }
        DevicesRequest::Find { query } => {
            let resp = FindResponse {
                device: find_device_by_name(&query),
            };
            println!("{}", serde_json::to_string(&resp)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(index: usize, name: &str, default: bool) -> DeviceInfo {
        DeviceInfo {
            index,
            name: name.to_owned(),
            max_input_channels: 1,
            sample_rates: (16_000, 48_000),
            default,
        }
    }
    #[test]
    fn device_info_round_trips_as_json() {
        let dev = make(2, "Mic 2", true);
        let json = serde_json::to_string(&dev).unwrap();
        let back: DeviceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dev);
    }

    #[test]
    fn list_response_serialises_field_name() {
        let resp = ListResponse {
            devices: vec![make(0, "Mic", false)],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains("\"devices\""),
            "expected `devices` envelope key in {json}"
        );
    }

    #[test]
    fn devices_request_parses_list_action() {
        let parsed: DevicesRequest = serde_json::from_str("{\"action\":\"list\"}").unwrap();
        // Absent flag defaults to false → no DirectSound merge for plain callers.
        assert!(matches!(
            parsed,
            DevicesRequest::List {
                include_directsound: false
            }
        ));
    }

    #[test]
    fn devices_request_parses_list_with_directsound_flag() {
        // The sounddevice picker opts into the DirectSound merge explicitly.
        let parsed: DevicesRequest =
            serde_json::from_str("{\"action\":\"list\",\"include_directsound\":true}").unwrap();
        assert!(matches!(
            parsed,
            DevicesRequest::List {
                include_directsound: true
            }
        ));
    }

    #[test]
    fn devices_request_parses_find_action() {
        let parsed: DevicesRequest =
            serde_json::from_str("{\"action\":\"find\",\"query\":\"jabra\"}").unwrap();
        match parsed {
            DevicesRequest::Find { query } => assert_eq!(query, "jabra"),
            other => panic!("expected Find, got {other:?}"),
        }
    }

    #[test]
    fn resolve_defaults_to_list_when_stdin_is_a_tty() {
        // An interactive `wd devices` in PowerShell has no
        // piped body — we skip the blocking read and default to the list so
        // the user sees output instead of the process hanging on stdin.
        let request = resolve_devices_request(true, None).unwrap();
        assert!(matches!(request, DevicesRequest::List { .. }));
    }

    #[test]
    fn resolve_defaults_to_list_when_stdin_is_a_tty_even_with_body() {
        // Defensive: if a caller ever passes a body while claiming TTY,
        // the TTY branch still wins (matches the doc contract — TTY means
        // interactive convenience regardless of the body).
        let request =
            resolve_devices_request(true, Some(r#"{"action":"find","query":"x"}"#)).unwrap();
        assert!(matches!(request, DevicesRequest::List { .. }));
    }

    #[test]
    fn resolve_defaults_to_list_when_piped_stdin_is_empty() {
        // A caller may pipe nothing and expect a list
        // this is the documented shorthand for `{"action":"list"}`.
        assert!(matches!(
            resolve_devices_request(false, Some("")).unwrap(),
            DevicesRequest::List { .. }
        ));
        assert!(matches!(
            resolve_devices_request(false, Some("   \n  ")).unwrap(),
            DevicesRequest::List { .. }
        ));
        assert!(matches!(
            resolve_devices_request(false, None).unwrap(),
            DevicesRequest::List { .. }
        ));
    }

    #[test]
    fn resolve_parses_piped_json_body() {
        // A name lookup passes a `find` envelope.
        let body = r#"{"action":"find","query":"jabra"}"#;
        let request = resolve_devices_request(false, Some(body)).unwrap();
        match request {
            DevicesRequest::Find { query } => assert_eq!(query, "jabra"),
            other => panic!("expected Find, got {other:?}"),
        }
    }

    #[test]
    fn resolve_returns_error_on_invalid_piped_json() {
        // A malformed body from a broken caller must surface an error, not
        // be silently swallowed as `List` (that would mask a broken
        // integration where the caller thought it was asking for
        // something specific and got the wrong answer).
        let err = resolve_devices_request(false, Some("{not-json")).unwrap_err();
        // The exact wording is serde_json's business (it varies with the
        // input); just assert we surfaced SOMETHING with position info.
        let msg = err.to_string();
        assert!(!msg.is_empty(), "empty error message");
        assert!(
            msg.contains("line") || msg.contains("column"),
            "expected serde parse position info, got: {msg}"
        );
    }
}
