//! Cross-host cpal input-device resolution.
//!
//! The Rust capture path historically opened `cpal::default_host()` and only
//! searched its input-device list — WASAPI on Windows, ALSA on Linux,
//! CoreAudio on macOS. That silently loses any mic that a non-default cpal
//! host surfaces first: `available_hosts()` also returns ASIO (Windows,
//! `asio` feature) and JACK / PulseAudio / PipeWire (Linux, whichever
//! features are compiled in). The picker in [`crate::devices`] already
//! walks every host so the Settings UI can offer them; not doing the same
//! in the capture path is the exact "device shows up in the picker but
//! capture says 'input device not found'" bug users hit on rc.13.
//!
//! This module is that discipline in one place:
//!
//! * [`resolve_input`] walks `default_host` first, then the rest of
//!   `available_hosts()`, returning the first name match plus the host it
//!   came from so the caller can log which backend actually opened.
//! * [`HostSnapshot`] / [`snapshot_all_hosts`] expose the per-host device
//!   name lists so the CLI `devices list` verb and the mic picker can
//!   annotate entries with their host label (`[WASAPI]` vs `[ASIO]`) —
//!   the disambiguation matters when two hosts report the same mic under
//!   slightly different names.
//!
//! **cpal 0.18 Windows scope.** cpal 0.18 has NO DirectSound host — only
//! `Wasapi`, plus optional `Asio` / `Jack` behind cargo features that we
//! do not compile. On a stock Windows build `available_hosts()` therefore
//! returns exactly `[Wasapi]`, and this multi-host walk collapses to the
//! same single-host lookup we had before. The picker's separate native
//! `DirectSoundCaptureEnumerateW` pass (see
//! [`crate::devices::directsound_capture_names`]) can still surface a mic
//! that cpal never sees; opening one is out of reach until cpal grows a
//! DirectSound host (or we add a native fallback). See the module-level
//! comment on that helper for the constraint.

use cpal::traits::HostTrait;
use cpal::HostId;

use super::capture::{resolve_device_index, DeviceLookup};

/// One resolved cpal input device together with the host it came from.
/// The host label is cpal's own string (`"WASAPI"`, `"ALSA"`, `"CoreAudio"`,
/// …) so log lines line up 1:1 with what `cpal::HostId::name()` returns.
pub struct ResolvedInput {
    pub device: cpal::Device,
    pub host_id: HostId,
    pub host_label: &'static str,
}

/// The input devices reported by a single cpal host, in enumeration order.
/// Kept as bare names (via cpal's `Display` impl) — resolution walks these
/// with [`resolve_device_index`] so the "exact → longest substring →
/// numeric index" precedence matches the capture path exactly.
pub struct HostSnapshot {
    pub host_id: HostId,
    pub host_label: &'static str,
    pub device_names: Vec<String>,
}

/// Return every cpal host in preference order: the platform default first,
/// then the rest of `available_hosts()` in its native order. Deduplicated
/// so a host that happens to be the default is not walked twice.
pub fn preferred_host_order() -> Vec<HostId> {
    let default_id = cpal::default_host().id();
    let mut order = vec![default_id];
    for id in cpal::available_hosts() {
        if id != default_id {
            order.push(id);
        }
    }
    order
}

/// Snapshot every cpal host's input-device names. Best-effort: a host that
/// fails to construct or fails to enumerate contributes an empty
/// `device_names` vector so callers can still see the host was tried and
/// report that gap in error messages.
pub fn snapshot_all_hosts() -> Vec<HostSnapshot> {
    let mut out = Vec::new();
    for host_id in preferred_host_order() {
        let label = host_id.name();
        let device_names = match cpal::host_from_id(host_id) {
            Ok(host) => match host.input_devices() {
                Ok(iter) => iter.map(|d| d.to_string()).collect(),
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        out.push(HostSnapshot {
            host_id,
            host_label: label,
            device_names,
        });
    }
    out
}

/// Resolve a device selector across every cpal host. Empty selector picks
/// the default host's default input.
///
/// Lookup order per host is [`resolve_device_index`]'s precedence (exact
/// case-insensitive → bidirectional longest substring → numeric index).
/// The default host is tried first; on no match the walk falls through to
/// each remaining host in `available_hosts()` order. First host with a
/// match wins.
///
/// On failure the error message includes the total number of devices
/// searched and the number of hosts consulted, plus a Windows-specific
/// DirectSound hint when the requested selector matches a DirectSound-only
/// capture-endpoint name (cpal has no DirectSound host, so those names
/// cannot be opened even though they appear in the mic picker).
pub fn resolve_input(selector: &str) -> Result<ResolvedInput, anyhow::Error> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        let host = cpal::default_host();
        let host_id = host.id();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("no default input device available"))?;
        return Ok(ResolvedInput {
            device,
            host_id,
            host_label: host_id.name(),
        });
    }

    let mut host_snapshots: Vec<HostSnapshot> = Vec::new();
    let mut last_index_error: Option<String> = None;

    for host_id in preferred_host_order() {
        let host_label = host_id.name();
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(err) => {
                eprintln!(
                    "[audio/hosts] host {host_label}: constructor failed ({err}); \
                     skipping"
                );
                continue;
            }
        };
        let devices: Vec<cpal::Device> = match host.input_devices() {
            Ok(iter) => iter.collect(),
            Err(err) => {
                eprintln!(
                    "[audio/hosts] host {host_label}: input_devices() failed ({err}); \
                     skipping"
                );
                continue;
            }
        };
        let names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
        match resolve_device_index(&names, trimmed) {
            DeviceLookup::Matched(idx) => {
                let device = devices
                    .into_iter()
                    .nth(idx)
                    .expect("resolve_device_index returned an in-range index");
                return Ok(ResolvedInput {
                    device,
                    host_id,
                    host_label,
                });
            }
            DeviceLookup::IndexOutOfRange { wanted, available } => {
                // Numeric selector that outran this host — remember the
                // last one so the aggregate error can quote a concrete
                // range instead of just "not found".
                last_index_error = Some(format!(
                    "index {wanted} out of range on {host_label} ({available} device(s))"
                ));
                host_snapshots.push(HostSnapshot {
                    host_id,
                    host_label,
                    device_names: names,
                });
            }
            DeviceLookup::NotFound => {
                host_snapshots.push(HostSnapshot {
                    host_id,
                    host_label,
                    device_names: names,
                });
            }
        }
    }

    Err(build_not_found_error(
        trimmed,
        &host_snapshots,
        last_index_error,
    ))
}

/// Windows-specific hint: when the requested selector matches a name
/// reported by `DirectSoundCaptureEnumerateW` but not by any cpal host,
/// the mic is DirectSound-only and cpal cannot open it. Callers surface
/// this as part of the "not found" error so the user knows to pick the
/// WASAPI-visible variant (which cpal CAN open) instead of the
/// DirectSound one.
#[cfg(windows)]
pub fn directsound_only_hint(selector: &str) -> Option<String> {
    use crate::devices::name_matches;
    let ds_names = crate::devices::directsound_capture_names_public();
    let hit = ds_names.iter().any(|n| name_matches(n, selector));
    if hit {
        Some(format!(
            "; note: {selector:?} is only visible via Windows DirectSound, \
             which cpal 0.18 cannot open - pick the WASAPI variant in the mic \
             picker instead"
        ))
    } else {
        None
    }
}

#[cfg(not(windows))]
pub fn directsound_only_hint(_selector: &str) -> Option<String> {
    None
}

/// Compose the aggregate "input device not found" error message that
/// [`resolve_input`] returns. Split out as a pure helper so the wording
/// (which tests pin) is easy to keep stable across refactors.
fn build_not_found_error(
    selector: &str,
    host_snapshots: &[HostSnapshot],
    last_index_error: Option<String>,
) -> anyhow::Error {
    let total_devices: usize = host_snapshots.iter().map(|h| h.device_names.len()).sum();
    let hosts_tried: Vec<&str> = host_snapshots.iter().map(|h| h.host_label).collect();
    let hosts_str = if hosts_tried.is_empty() {
        String::from("no hosts")
    } else {
        hosts_tried.join(", ")
    };
    let index_note = last_index_error
        .map(|e| format!("; last numeric-index attempt: {e}"))
        .unwrap_or_default();
    let ds_hint = directsound_only_hint(selector).unwrap_or_default();
    anyhow::anyhow!(
        "input device not found: {selector:?} \
         (searched {total_devices} device(s) across {} host(s): {hosts_str}{index_note}{ds_hint})",
        host_snapshots.len(),
    )
}

#[cfg(test)]
#[path = "hosts_tests.rs"]
mod tests;
