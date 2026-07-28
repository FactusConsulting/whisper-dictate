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

    // The default host's identity (label, id) is load-bearing: numeric
    // selectors resolve ONLY against it, and its exact-match short-
    // circuit avoids touching slow secondary backends (JACK/ASIO/Pulse)
    // for the common "picker-saved name matches the default host" case.
    // Track both up front — even if the default host fails to
    // enumerate we still want its label in the numeric-note wording so
    // the user isn't told "index N out of range on default host ASIO".
    let default_host_id = cpal::default_host().id();
    let default_host_label = default_host_id.name();
    let needle_lower = trimmed.to_lowercase();

    let mut host_errors: Vec<String> = Vec::new();

    // Codex P2 (#669 hosts.rs:149) short-circuit: enumerate ONLY the
    // default host first and check for an exact name match. On the
    // common case (picker-saved name of a default-host mic) we return
    // immediately, so a slow / unavailable secondary backend never
    // delays capture start. Only when no exact match fires here do we
    // widen the walk to every host for the longest-substring / numeric
    // passes.
    // Codex P2 (#669 hosts.rs:212): filter to devices the capture path
    // could actually open (i.e., at least one supported input config with
    // channels), matching `crate::devices::probe_device_config`. Without
    // this, the resolver's exact-match short-circuit could return a
    // default-host mic whose usable same-named counterpart lives on a
    // secondary host — the picker advertised the SECONDARY (its filter
    // is the same as ours) and capture would then fail on the default
    // host's unusable variant.
    let default_slot = enumerate_host_slot_usable(default_host_id, &mut host_errors);
    let default_enumerated = default_slot.is_some();
    let winning_default_idx: Option<usize> = if needle_lower.is_empty() {
        None
    } else {
        default_slot.as_ref().and_then(|slot| {
            slot.names
                .iter()
                .position(|name| name.to_lowercase() == needle_lower)
        })
    };
    if let Some(d_idx) = winning_default_idx {
        let slot = default_slot.expect("winning idx implies default_slot is Some");
        return Ok(pluck_single(slot, d_idx));
    }

    // No exact match on the default host - enumerate the remaining
    // hosts too so the substring / numeric passes can see everything.
    let mut host_slots: Vec<HostSlot> = Vec::new();
    // Codex P2 (#669 hosts.rs:193): always keep the default host at
    // index 0, even when enumeration failed. Numeric selectors resolve
    // against hosts[0], so a partial-failure default host that got
    // dropped from host_slots would leave a SECONDARY host holding
    // slot 0 — silently opening its nth microphone (the exact wrong-
    // device fallback fix 3 was meant to prevent). Insert an
    // enumeration-empty placeholder so hosts[0] is always the real
    // default: label stays correct, device count is 0, numeric branch
    // reports the honest "0 device(s) on default host" wording.
    host_slots.push(default_slot.unwrap_or_else(|| HostSlot {
        host_id: default_host_id,
        host_label: default_host_label,
        devices: Vec::new(),
        names: Vec::new(),
    }));

    // Track successful ENUMERATION (returned Some), distinct from
    // whether that enumeration found any devices. A headless box or one
    // with no mics enumerates cleanly to zero devices — that's the
    // "device not found" path, NOT the "enumerate input devices: no cpal
    // hosts available" backend-outage path (Codex P2 #669 hosts.rs:203).
    let mut any_host_succeeded = default_enumerated;
    for host_id in preferred_host_order().into_iter().skip(1) {
        if let Some(slot) = enumerate_host_slot_usable(host_id, &mut host_errors) {
            any_host_succeeded = true;
            host_slots.push(slot);
        }
    }

    if should_propagate_enumeration_failure(any_host_succeeded) {
        return Err(anyhow::anyhow!(
            "{}",
            no_searchable_hosts_error_message(&host_errors)
        ));
    }

    // Delegate the remaining precedence (longest substring across all
    // hosts, then numeric on the default host) to `resolve_over_host_names`
    // so the rule is unit-testable against synthetic host name lists —
    // the same logic, minus cpal's live device handles. The exact-match
    // pass in that helper is redundant here (we already short-circuited
    // above) but harmless: it re-checks the default host's exact match
    // and every secondary host's exact match, both of which the
    // short-circuit already ruled out for THIS invocation.
    let host_name_lists: Vec<Vec<String>> =
        host_slots.iter().map(|slot| slot.names.clone()).collect();
    match resolve_over_host_names(&host_name_lists, trimmed, default_host_label) {
        SelectorOutcome::Matched { host, device } => Ok(pluck(host_slots, host, device)),
        SelectorOutcome::NumericOutOfRange { note } => {
            let snapshots = into_snapshots(host_slots);
            Err(build_not_found_error(trimmed, &snapshots, Some(note)))
        }
        SelectorOutcome::NotFound => {
            let snapshots = into_snapshots(host_slots);
            Err(build_not_found_error(trimmed, &snapshots, None))
        }
    }
}

/// Construct + enumerate ONE cpal host into a [`HostSlot`], keeping
/// ONLY devices the capture path could actually open (i.e., at least
/// one supported input config with usable channels; see
/// [`device_is_usable`]). Returns `None` on constructor /
/// `input_devices()` failure, pushing the failure message onto
/// `host_errors` so [`resolve_input`] can surface it in the propagated
/// no-searchable-hosts error. An empty-but-successful enumeration
/// (headless box, no mics) returns `Some` with empty name / device
/// vectors — see [`should_propagate_enumeration_failure`] for why the
/// two outcomes must not be conflated.
fn enumerate_host_slot_usable(host_id: HostId, host_errors: &mut Vec<String>) -> Option<HostSlot> {
    let host_label = host_id.name();
    let host = match cpal::host_from_id(host_id) {
        Ok(h) => h,
        Err(err) => {
            let msg = format!("host {host_label}: constructor failed ({err})");
            eprintln!("[audio/hosts] {msg}; skipping");
            host_errors.push(msg);
            return None;
        }
    };
    let raw_devices: Vec<cpal::Device> = match host.input_devices() {
        Ok(iter) => iter.collect(),
        Err(err) => {
            let msg = format!("host {host_label}: input_devices() failed ({err})");
            eprintln!("[audio/hosts] {msg}; skipping");
            host_errors.push(msg);
            return None;
        }
    };
    let mut devices = Vec::with_capacity(raw_devices.len());
    let mut names = Vec::with_capacity(raw_devices.len());
    for device in raw_devices {
        if !device_is_usable(&device) {
            continue;
        }
        let name = device.to_string();
        if name.trim().is_empty() {
            // Blank names collide with the picker's "System default"
            // sentinel and can never legitimately match a user selector.
            continue;
        }
        names.push(name);
        devices.push(device);
    }
    Some(HostSlot {
        host_id,
        host_label,
        devices,
        names,
    })
}

/// Whether `device` has at least one usable input configuration.
/// Mirrors [`crate::devices::probe_device_config`]'s zero-channel
/// exclusion so the picker and resolver agree on which mics are
/// selectable — Codex P2 (#669 devices.rs:212).
fn device_is_usable(device: &cpal::Device) -> bool {
    use cpal::traits::DeviceTrait;
    if let Ok(iter) = device.supported_input_configs() {
        if iter.into_iter().any(|c| c.channels() > 0) {
            return true;
        }
    }
    device
        .default_input_config()
        .map(|c| c.channels() > 0)
        .unwrap_or(false)
}

/// Pure predicate deciding whether [`resolve_input`] should surface the
/// distinctive `enumerate input devices: ...` error or fall through to
/// the name-not-found path. Only true when NO cpal host actually
/// succeeded at enumeration — a host that enumerated cleanly to zero
/// devices (headless box, no mics connected) is NOT a backend outage
/// and must produce a `device not found`-shaped error, not the
/// misleading verbose enumeration-failure message. Codex P2 (#669
/// hosts.rs:203).
pub(crate) fn should_propagate_enumeration_failure(any_host_succeeded: bool) -> bool {
    !any_host_succeeded
}

/// Lift a single device out of a single [`HostSlot`]. The
/// short-circuit branch in [`resolve_input`] wins on the default host
/// alone, so it doesn't need the whole-list [`pluck`] machinery.
fn pluck_single(slot: HostSlot, d_idx: usize) -> ResolvedInput {
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

/// Compose the "enumerate input devices" propagated error the caller
/// sees when NO cpal host could actually be searched. Split out as a
/// pure helper so a regression against the pre-fix behavior — where
/// enumeration failures were masked as generic "input device not found"
/// with 0 hosts — is unit-testable without a real backend outage.
pub(crate) fn no_searchable_hosts_error_message(host_errors: &[String]) -> String {
    format!(
        "enumerate input devices: {}",
        if host_errors.is_empty() {
            "no cpal hosts available".to_owned()
        } else {
            host_errors.join("; ")
        }
    )
}

/// Result of the three-pass name-list resolution in
/// [`resolve_over_host_names`]. Modelled as an enum so the numeric
/// out-of-range case can carry its actionable "pick a device by name
/// instead" note without leaking into the `Matched` / `NotFound` arms.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SelectorOutcome {
    /// `host` is the index into the outer `hosts` slice;
    /// `device` is the index into that host's `names` slice.
    Matched { host: usize, device: usize },
    /// Numeric selector was out of range on the default host (index 0)
    /// and MUST NOT fall through to secondary hosts (see the doc-comment
    /// on the numeric-index pass in [`resolve_input`]).
    NumericOutOfRange { note: String },
    /// No exact, substring, or numeric match on any host.
    NotFound,
}

/// Pure resolver over per-host device-name lists. Extracted from
/// [`resolve_input`] so the three-pass precedence (exact across all
/// hosts → longest bidirectional substring across all hosts → numeric
/// on the default host only) is unit-testable without a live cpal
/// backend. `default_host_label` labels the default host (index 0) in
/// the numeric out-of-range note.
///
/// The default host is `hosts[0]` (see [`preferred_host_order`]).
pub(crate) fn resolve_over_host_names(
    hosts: &[Vec<String>],
    selector: &str,
    default_host_label: &str,
) -> SelectorOutcome {
    let trimmed = selector.trim();
    let needle_lower = trimmed.to_lowercase();

    // Pass 1: exact case-insensitive match across every host, in
    // preferred_host_order. First hit wins - but exactness always beats
    // any substring hit on any host, so a differently-named ASIO/JACK
    // entry on a secondary host cannot be hijacked by the default host's
    // shorter substring sibling.
    if !needle_lower.is_empty() {
        for (h_idx, names) in hosts.iter().enumerate() {
            for (d_idx, name) in names.iter().enumerate() {
                if name.to_lowercase() == needle_lower {
                    return SelectorOutcome::Matched {
                        host: h_idx,
                        device: d_idx,
                    };
                }
            }
        }
    }

    // Pass 2: bidirectional longest-substring match across every host.
    // Keep the LONGEST matching device name irrespective of host so a
    // truncated/generic saved value binds to its fullest sibling wherever
    // that lives - same longest-wins precedence as capture's single-host
    // resolver, just pooled across hosts.
    let mut best: Option<(usize, usize, usize)> = None; // (h_idx, d_idx, name_len)
    if !needle_lower.is_empty() {
        for (h_idx, names) in hosts.iter().enumerate() {
            for (d_idx, name) in names.iter().enumerate() {
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
            return SelectorOutcome::Matched {
                host: h_idx,
                device: d_idx,
            };
        }
    }

    // Pass 3: numeric selector. ONLY resolves against the default host
    // (hosts[0]) - non-default-host entries carry synthetic indices in
    // the published enumeration (see `devices::next_synthetic_from`) that
    // are unstable across host constellations, so accepting them by
    // number would let a stale numeric setting silently record from an
    // unrelated mic when an extra host comes online.
    if let Ok(idx) = trimmed.parse::<usize>() {
        // Guard against an empty host slice: caller should have returned
        // early with the enumeration-failure error before reaching here,
        // but be defensive so the pure resolver never panics.
        if let Some(default_names) = hosts.first() {
            if idx < default_names.len() {
                return SelectorOutcome::Matched {
                    host: 0,
                    device: idx,
                };
            }
            let note = format!(
                "index {idx} out of range on default host {default_host_label} \
                 ({} device(s)); numeric selectors resolve only against the \
                 default host - pick a device by name instead",
                default_names.len()
            );
            return SelectorOutcome::NumericOutOfRange { note };
        }
    }

    SelectorOutcome::NotFound
}

fn into_snapshots(host_slots: Vec<HostSlot>) -> Vec<HostSnapshot> {
    host_slots
        .into_iter()
        .map(|slot| HostSnapshot {
            host_id: slot.host_id,
            host_label: slot.host_label,
            device_names: slot.names,
        })
        .collect()
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
