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
/// Resolution is a THREE-pass walk across the host list, so match-quality
/// wins over host-order:
///
///   1. **Exact case-insensitive name match** across every host. Prevents
///      a shorter default-host name (`USB Mic`) from bidirectionally
///      substring-matching a saved selector that's ALSO the exact name of
///      a differently-named entry on a secondary host (`USB Mic ASIO`) —
///      the exact match on the secondary host wins.
///   2. **Bidirectional longest substring** across every host. Same
///      precedence as [`crate::audio::capture::resolve_device_index`]
///      (longest device name wins), but pooled across all hosts so the
///      fullest match wins irrespective of which host reported it.
///   3. **Numeric index** — resolved ONLY against the default host. The
///      published enumeration in [`crate::devices::list_input_devices`]
///      gives non-default-host entries SYNTHETIC indices that depend on
///      runtime enumeration state, so a numeric selector that's out of
///      range on the default host is rejected outright rather than
///      silently opening some secondary host's nth microphone.
///
/// Host enumeration failures (backend outage, permission error) are
/// propagated: if NO host could successfully be searched, the first
/// underlying error is returned instead of a generic "not found".
///
/// On name-not-found the aggregate error carries the total device count
/// and per-host breadcrumbs, plus a Windows-specific DirectSound hint
/// when the selector matches a DirectSound-only endpoint (cpal has no
/// DirectSound host, so those names cannot be opened).
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

    // Enumerate every host up front so we can do exact-match / longest-
    // substring passes across the FULL device set. Preserve construction
    // and enumeration failures so an outage can be propagated when it
    // means we couldn't search anything.
    let mut host_slots: Vec<HostSlot> = Vec::new();
    let mut host_errors: Vec<String> = Vec::new();

    for host_id in preferred_host_order() {
        let host_label = host_id.name();
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(err) => {
                let msg = format!("host {host_label}: constructor failed ({err})");
                eprintln!("[audio/hosts] {msg}; skipping");
                host_errors.push(msg);
                continue;
            }
        };
        let devices: Vec<cpal::Device> = match host.input_devices() {
            Ok(iter) => iter.collect(),
            Err(err) => {
                let msg = format!("host {host_label}: input_devices() failed ({err})");
                eprintln!("[audio/hosts] {msg}; skipping");
                host_errors.push(msg);
                continue;
            }
        };
        let names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
        host_slots.push(HostSlot {
            host_id,
            host_label,
            devices,
            names,
        });
    }

    // No host successfully enumerated → propagate the underlying failure
    // instead of masking it as "input device not found: 0 hosts". Otherwise
    // an audio-backend outage looks identical to a bad saved microphone
    // name, hiding the root cause from the diagnostic log.
    if host_slots.is_empty() {
        return Err(anyhow::anyhow!(
            "enumerate input devices: {}",
            if host_errors.is_empty() {
                "no cpal hosts available".to_owned()
            } else {
                host_errors.join("; ")
            }
        ));
    }

    let needle_lower = trimmed.to_lowercase();

    // Pass 1: exact case-insensitive match across every host, in
    // preferred_host_order. First hit wins — but exactness always beats
    // any substring hit on any host, so a differently-named ASIO/JACK
    // entry on a secondary host cannot be hijacked by the default host's
    // shorter substring sibling.
    for (h_idx, slot) in host_slots.iter().enumerate() {
        for (d_idx, name) in slot.names.iter().enumerate() {
            if name.to_lowercase() == needle_lower {
                return Ok(pluck(host_slots, h_idx, d_idx));
            }
        }
    }

    // Pass 2: bidirectional longest-substring match across every host.
    // Keep the LONGEST matching device name irrespective of host so a
    // truncated/generic saved value binds to its fullest sibling wherever
    // that lives — same longest-wins precedence as capture's single-host
    // resolver, just pooled across hosts.
    let mut best: Option<(usize, usize, usize)> = None; // (h_idx, d_idx, name_len)
    if !needle_lower.is_empty() {
        for (h_idx, slot) in host_slots.iter().enumerate() {
            for (d_idx, name) in slot.names.iter().enumerate() {
                let lower = name.to_lowercase();
                if lower.is_empty()
                    || !(lower.contains(&needle_lower) || needle_lower.contains(&lower))
                {
                    continue;
                }
                let name_len = name.len();
                match best {
                    None => best = Some((h_idx, d_idx, name_len)),
                    Some((_, _, prev_len)) if name_len > prev_len => {
                        best = Some((h_idx, d_idx, name_len));
                    }
                    _ => {}
                }
            }
        }
        if let Some((h_idx, d_idx, _)) = best {
            return Ok(pluck(host_slots, h_idx, d_idx));
        }
    }

    // Pass 3: numeric selector. ONLY resolves against the default host —
    // non-default-host entries carry synthetic indices in the published
    // enumeration (see `devices::next_synthetic_from`) that are unstable
    // across host constellations, so accepting them by number would let
    // a stale numeric setting silently record from an unrelated mic when
    // an extra host comes online.
    let mut numeric_note: Option<String> = None;
    if let Ok(idx) = trimmed.parse::<usize>() {
        let default_slot = &host_slots[0];
        if idx < default_slot.names.len() {
            return Ok(pluck(host_slots, 0, idx));
        }
        numeric_note = Some(format!(
            "index {idx} out of range on default host {} ({} device(s)); \
             numeric selectors resolve only against the default host - \
             pick a device by name instead",
            default_slot.host_label,
            default_slot.names.len()
        ));
    }

    let snapshots: Vec<HostSnapshot> = host_slots
        .into_iter()
        .map(|slot| HostSnapshot {
            host_id: slot.host_id,
            host_label: slot.host_label,
            device_names: slot.names,
        })
        .collect();
    Err(build_not_found_error(trimmed, &snapshots, numeric_note))
}

/// Working entry used by [`resolve_input`] — holds one host's devices AND
/// their names side by side so we can index by position without keeping
/// two lists in sync.
struct HostSlot {
    host_id: HostId,
    host_label: &'static str,
    devices: Vec<cpal::Device>,
    names: Vec<String>,
}

/// Consume the host list and lift out the winning device. Split out so
/// each match-pass in [`resolve_input`] can `return Ok(pluck(...))` on
/// its first hit without duplicating the `into_iter().nth(...)` dance.
fn pluck(host_slots: Vec<HostSlot>, h_idx: usize, d_idx: usize) -> ResolvedInput {
    let mut iter = host_slots.into_iter();
    let slot = iter
        .nth(h_idx)
        .expect("winning host index inside enumerated range");
    let device = slot
        .devices
        .into_iter()
        .nth(d_idx)
        .expect("winning device index inside enumerated range");
    ResolvedInput {
        device,
        host_id: slot.host_id,
        host_label: slot.host_label,
    }
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
    numeric_note: Option<String>,
) -> anyhow::Error {
    let total_devices: usize = host_snapshots.iter().map(|h| h.device_names.len()).sum();
    let hosts_tried: Vec<&str> = host_snapshots.iter().map(|h| h.host_label).collect();
    let hosts_str = if hosts_tried.is_empty() {
        String::from("no hosts")
    } else {
        hosts_tried.join(", ")
    };
    let index_note = numeric_note.map(|e| format!("; {e}")).unwrap_or_default();
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
