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
///
/// Post-#669 additions (Codex post-merge P2s):
/// * `usable` — parallel to `device_names`, `false` when the device is
///   enumerated by cpal but `pick_config` cannot open it (see
///   [`device_supports_rust_capture`]). The picker's DirectSound-only
///   remediation hint checks the FULL enumerated name set (usable or
///   not) so a device that's visible-but-unopenable doesn't get the
///   false "only visible via Windows DirectSound" claim.
/// * `enumeration_error` — `Some` when the snapshot is a
///   placeholder for a host whose construction / `input_devices()`
///   call failed. Such placeholders keep the default host's identity
///   (for numeric-note wording) but MUST NOT be counted as
///   successfully searched hosts in the aggregate error.
pub struct HostSnapshot {
    pub host_id: HostId,
    pub host_label: &'static str,
    pub device_names: Vec<String>,
    pub usable: Vec<bool>,
    pub enumeration_error: Option<String>,
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
/// fails to construct or fails to enumerate produces a snapshot with an
/// `enumeration_error` set so callers can distinguish "host was tried
/// but no devices found" from "host could not be searched". Every
/// `device_names` entry gets a parallel `true` in `usable` — this
/// helper does not run the strict pick-config check
/// [`device_supports_rust_capture`] because it is a diagnostic /
/// listing shim, not a resolver input.
pub fn snapshot_all_hosts() -> Vec<HostSnapshot> {
    let mut out = Vec::new();
    for host_id in preferred_host_order() {
        let label = host_id.name();
        let (device_names, enumeration_error) = match cpal::host_from_id(host_id) {
            Ok(host) => match host.input_devices() {
                Ok(iter) => (iter.map(|d| d.to_string()).collect::<Vec<_>>(), None),
                Err(err) => (Vec::new(), Some(format!("input_devices() failed ({err})"))),
            },
            Err(err) => (Vec::new(), Some(format!("constructor failed ({err})"))),
        };
        let usable = vec![true; device_names.len()];
        out.push(HostSnapshot {
            host_id,
            host_label: label,
            device_names,
            usable,
            enumeration_error,
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
    // Codex P2 (#669 hosts.rs:212): the exact-match short-circuit
    // MUST consult the `usable` mask so a default-host mic whose
    // capture-openable same-named counterpart lives on a secondary
    // host doesn't hijack the resolution (the picker advertised the
    // secondary using the same strict filter this resolver uses).
    let default_slot = enumerate_host_slot_usable(default_host_id, &mut host_errors);
    let default_enumerated = default_slot.enumeration_error.is_none();
    let winning_default_idx: Option<usize> = if needle_lower.is_empty() {
        None
    } else {
        default_slot
            .names
            .iter()
            .zip(default_slot.usable.iter())
            .position(|(name, usable)| *usable && name.to_lowercase() == needle_lower)
    };
    if let Some(d_idx) = winning_default_idx {
        return Ok(pluck_single(default_slot, d_idx));
    }

    // No usable exact match on the default host — enumerate the
    // remaining hosts so the substring / numeric passes can see
    // everything. The default host's slot is preserved at index 0
    // (even when its enumeration failed) so numeric-note wording
    // keeps the correct host label — Codex P2 (#669 hosts.rs:193).
    let mut host_slots: Vec<HostSlot> = vec![default_slot];

    // Track SUCCESSFUL enumeration (enumeration_error.is_none()),
    // distinct from whether devices were found. Headless boxes /
    // no-mic setups enumerate cleanly to zero devices — that's the
    // "device not found" path, NOT the "enumerate input devices" path
    // (Codex P2 #669 hosts.rs:203).
    //
    // Codex P2 (#674 hosts.rs:222): even when a secondary host FAILS
    // to enumerate, its slot is PUSHED (with enumeration_error=Some)
    // so `build_not_found_error` can surface it in the aggregate
    // `enumeration failures:` clause. Dropping the failed slot
    // silently ate the diagnostic — a transient ASIO/JACK outage
    // then looked identical to a plain name miss.
    let mut any_host_succeeded = default_enumerated;
    for host_id in preferred_host_order().into_iter().skip(1) {
        let slot = enumerate_host_slot_usable(host_id, &mut host_errors);
        if slot.enumeration_error.is_none() {
            any_host_succeeded = true;
        }
        if should_push_secondary_slot(&slot) {
            host_slots.push(slot);
        }
    }

    if should_propagate_enumeration_failure(any_host_succeeded) {
        return Err(anyhow::anyhow!(
            "{}",
            no_searchable_hosts_error_message(&host_errors)
        ));
    }

    // Build the pure-resolver input: an empty string masks unusable
    // slots so the exact/substring/numeric passes skip them, while the
    // HostSnapshot fed to `build_not_found_error` still carries the
    // real names + usable mask for diagnostics and DirectSound-hint
    // suppression.
    let host_name_lists: Vec<Vec<String>> = host_slots
        .iter()
        .map(|slot| mask_names_for_resolver(&slot.names, &slot.usable))
        .collect();
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

/// Produce the "usable-only" view of a host's name list for the pure
/// resolver: real name kept when `usable[i]` is true, empty string
/// substituted otherwise. Split out so [`resolve_input`] can build the
/// resolver input in one line and so the mapping is testable in
/// isolation.
pub(crate) fn mask_names_for_resolver(names: &[String], usable: &[bool]) -> Vec<String> {
    names
        .iter()
        .zip(usable.iter())
        .map(|(name, is_usable)| {
            if *is_usable {
                name.clone()
            } else {
                String::new()
            }
        })
        .collect()
}

/// Construct + enumerate ONE cpal host into a [`HostSlot`]. Devices
/// are kept at their raw cpal enumeration position; the `usable`
/// parallel-vec marks slots the capture path cannot open (Codex P2
/// #669 hosts.rs:294 — real names are PRESERVED for diagnostics + the
/// DirectSound-hint suppression check even when the slot is
/// unusable). The pure resolver's match passes skip unusable slots
/// via that mask; numeric selectors additionally require a non-empty
/// name so a picker-published index maps to a real device.
///
/// On constructor / `input_devices()` failure, returns a placeholder
/// slot with `enumeration_error = Some(...)` (empty devices/names)
/// AND still pushes the message onto `host_errors` for the propagated
/// no-searchable-hosts error. The default-host caller preserves this
/// placeholder so numeric-note wording keeps the correct label; the
/// aggregate error uses `enumeration_error` to distinguish "searched"
/// from "failed" hosts (Codex P2 #669 hosts.rs:200).
fn enumerate_host_slot_usable(host_id: HostId, host_errors: &mut Vec<String>) -> HostSlot {
    let host_label = host_id.name();
    let host = match cpal::host_from_id(host_id) {
        Ok(h) => h,
        Err(err) => {
            let msg = format!("host {host_label}: constructor failed ({err})");
            eprintln!("[audio/hosts] {msg}; skipping");
            host_errors.push(msg.clone());
            return HostSlot {
                host_id,
                host_label,
                devices: Vec::new(),
                names: Vec::new(),
                usable: Vec::new(),
                enumeration_error: Some(msg),
            };
        }
    };
    let raw_devices: Vec<cpal::Device> = match host.input_devices() {
        Ok(iter) => iter.collect(),
        Err(err) => {
            let msg = format!("host {host_label}: input_devices() failed ({err})");
            eprintln!("[audio/hosts] {msg}; skipping");
            host_errors.push(msg.clone());
            return HostSlot {
                host_id,
                host_label,
                devices: Vec::new(),
                names: Vec::new(),
                usable: Vec::new(),
                enumeration_error: Some(msg),
            };
        }
    };
    let mut devices = Vec::with_capacity(raw_devices.len());
    let mut names = Vec::with_capacity(raw_devices.len());
    let mut usable = Vec::with_capacity(raw_devices.len());
    for device in raw_devices {
        // Preserve the REAL cpal name at the raw enumeration position,
        // even for devices capture cannot open — the parallel `usable`
        // mask (below) is what the resolver's match passes consult, and
        // `build_not_found_error`'s DirectSound-hint check needs to see
        // the real name so a "visible-but-unopenable" cpal device
        // doesn't get the false "only visible via Windows DirectSound"
        // remediation (Codex P2 #669 hosts.rs:294).
        let raw_name = device.to_string();
        let name = if raw_name.trim().is_empty() {
            String::new()
        } else {
            raw_name
        };
        let is_usable = !name.is_empty() && device_supports_rust_capture(&device);
        devices.push(device);
        names.push(name);
        usable.push(is_usable);
    }
    HostSlot {
        host_id,
        host_label,
        devices,
        names,
        usable,
        enumeration_error: None,
    }
}

/// Whether `device` has at least one input configuration that
/// [`crate::audio::capture::pick_config`] can actually open — i.e.
/// `supported_input_configs()` succeeds AND yields at least one F32 /
/// I16 / I32 config with usable channels. Devices that only satisfy
/// `default_input_config()` (fallback) OR only expose non-F32/I16/I32
/// formats (U16, F64, …) are EXCLUDED because live capture would fail
/// on them — Codex P2 (#669 devices.rs:271).
///
/// Also re-exported for `crate::devices::append_host_devices` to
/// apply the same filter to the sounddevice picker when Rust capture
/// is active, so the picker and resolver never disagree on which
/// mics can actually be opened.
pub(crate) fn device_supports_rust_capture(device: &cpal::Device) -> bool {
    use cpal::traits::DeviceTrait;
    let Ok(iter) = device.supported_input_configs() else {
        return false;
    };
    iter.into_iter()
        .any(|c| sample_config_is_rust_openable(c.sample_format(), c.channels()))
}

/// Pure predicate: does a single `supported_input_configs` entry meet
/// `capture::pick_config`'s open contract? Extracted so the
/// accept/reject matrix (F32/I16/I32 with channels > 0 vs everything
/// else) is exhaustively unit-testable without fabricating a cpal
/// `Device` — Codex P2 (#674 devices.rs:600).
pub(crate) fn sample_config_is_rust_openable(format: cpal::SampleFormat, channels: u16) -> bool {
    channels > 0
        && matches!(
            format,
            cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::I32
        )
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

/// Whether a SECONDARY-host slot should be pushed into `host_slots`
/// during resolver enumeration. Always true — even failed slots
/// (enumeration_error=Some) MUST reach `build_not_found_error` so
/// their failure message appears in the aggregate error's
/// `enumeration failures:` clause. Codex P2 (#674 hosts.rs:222)
/// regression pin: dropping failed slots ate the diagnostic and made
/// a transient ASIO/JACK outage indistinguishable from a plain name
/// miss.
fn should_push_secondary_slot(slot: &HostSlot) -> bool {
    // Documented invariant: retain regardless of success or failure.
    // Successful slots carry usable devices to search; failed slots
    // carry their enumeration_error for the aggregate diagnostic.
    let _ = slot;
    true
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
    // Codex P2 (#669 hosts.rs:424): a parseable numeric selector MUST
    // go straight to the default-host-only numeric pass — bypassing
    // the cross-host substring pass — so a secondary device named
    // e.g. "ASIO Input 2" cannot hijack selector "2" before the
    // numeric branch runs. The exact-match pass is still allowed to
    // fire (a device LITERALLY named "2" is a legitimate exact hit
    // and should win on any host); only substring is skipped.
    let selector_is_numeric = trimmed.parse::<usize>().is_ok();

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
    // resolver, just pooled across hosts. Skipped for numeric selectors
    // (see `selector_is_numeric` note above): a digit-bearing secondary
    // name must not steal a numeric selector before the default-host-
    // only numeric branch runs.
    let mut best: Option<(usize, usize, usize)> = None; // (h_idx, d_idx, name_len)
    if !needle_lower.is_empty() && !selector_is_numeric {
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
    //
    // Codex P2 (#669 hosts.rs:280): the default host's `names` vector
    // has empty-string placeholders at the raw cpal positions of
    // filtered-out (unusable / blank-name) devices, so `names[N]`
    // stays aligned with the picker's published `cpal_index`. Empty
    // slots are treated as "not published at that index" so a numeric
    // selector cannot open a device the picker never advertised.
    if let Ok(idx) = trimmed.parse::<usize>() {
        // Guard against an empty host slice: caller should have returned
        // early with the enumeration-failure error before reaching here,
        // but be defensive so the pure resolver never panics.
        if let Some(default_names) = hosts.first() {
            if let Some(name) = default_names.get(idx) {
                if !name.is_empty() {
                    return SelectorOutcome::Matched {
                        host: 0,
                        device: idx,
                    };
                }
                // idx is within the raw enumeration but the slot is a
                // usability-filter placeholder — fall through to the
                // out-of-range note, quoting the USABLE device count
                // (matches the picker's published set).
            }
            let usable_count = default_names.iter().filter(|n| !n.is_empty()).count();
            let note = format!(
                "index {idx} out of range on default host {default_host_label} \
                 ({usable_count} device(s)); numeric selectors resolve only against \
                 the default host - pick a device by name instead"
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
            usable: slot.usable,
            enumeration_error: slot.enumeration_error,
        })
        .collect()
}

/// Working entry used by [`resolve_input`] — holds one host's devices AND
/// their names side by side so we can index by position without keeping
/// two lists in sync.
///
/// Post-#669 additions (Codex post-merge P2s):
/// * `usable` — parallel bool mask. `names` always carries the REAL
///   cpal name (Codex P2 #669 hosts.rs:294); usable=false marks a
///   slot the resolver's match passes MUST skip (capture would fail
///   to open it) while still surfacing the name in diagnostics + the
///   DirectSound-hint suppression check.
/// * `enumeration_error` — `Some` when the slot is a placeholder for
///   a host whose enumeration failed. The default host always has a
///   slot (for numeric-note wording), but a failed slot MUST NOT be
///   counted as "successfully searched" in the aggregate error
///   (Codex P2 #669 hosts.rs:200).
struct HostSlot {
    host_id: HostId,
    host_label: &'static str,
    devices: Vec<cpal::Device>,
    names: Vec<String>,
    usable: Vec<bool>,
    enumeration_error: Option<String>,
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
///
/// The `cpal_enumerated_names` slice is the FULL list of names cpal
/// enumerated (across all hosts, usable + unusable). If the selector
/// matches ANY of those, the DirectSound hint is suppressed — the
/// device IS visible through cpal (just unopenable), so the "only
/// visible via DirectSound" remediation would be a false claim. Fix
/// for Codex P2 (#669 hosts.rs:294 — post-merge).
/// Pure predicate: does the selector match a name already enumerated
/// by any cpal host? Used by [`build_not_found_error`] (Windows only)
/// to suppress the DirectSound hint when cpal already surfaced the
/// device (usable or not) — otherwise the aggregate error would
/// falsely claim the mic is "only visible via Windows DirectSound"
/// even though it's enumerated through WASAPI. Codex P2 (#669
/// post-merge hosts.rs:294).
///
/// Cross-platform (no `cfg` restriction) so the invariant is
/// unit-testable on every OS. `#[cfg(any(windows, test))]` because
/// non-Windows production has no DirectSound path to suppress; the
/// test attribute keeps it callable from the cross-platform test
/// module without triggering a `dead_code` warning on stock Linux
/// clippy builds.
#[cfg(any(windows, test))]
pub(crate) fn selector_matches_any_cpal_name(selector: &str, cpal_names: &[&str]) -> bool {
    use crate::devices::name_matches;
    cpal_names
        .iter()
        .any(|n| !n.is_empty() && name_matches(n, selector))
}

#[cfg(windows)]
pub fn directsound_only_hint(selector: &str, cpal_enumerated_names: &[&str]) -> Option<String> {
    // Codex P2 (#669 post-merge hosts.rs:294): suppress the hint when
    // cpal already enumerated a name matching the selector — even
    // usability-filtered devices count. Otherwise a visible-but-
    // unopenable cpal device gets the false "only visible via
    // DirectSound" remediation.
    if selector_matches_any_cpal_name(selector, cpal_enumerated_names) {
        return None;
    }
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
pub fn directsound_only_hint(_selector: &str, _cpal_enumerated_names: &[&str]) -> Option<String> {
    None
}

/// Compose the aggregate "input device not found" error message that
/// [`resolve_input`] returns. Split out as a pure helper so the wording
/// (which tests pin) is easy to keep stable across refactors.
///
/// A snapshot with `enumeration_error = Some(...)` is a FAILED-host
/// placeholder — its label / device count MUST NOT be counted as
/// "searched" (Codex P2 #669 hosts.rs:200 post-merge). Instead the
/// failure appears in a separate `; enumeration failures: ...`
/// clause so a user diagnosing the error can distinguish "we searched
/// but didn't find your name" from "we couldn't ask this host at all".
fn build_not_found_error(
    selector: &str,
    host_snapshots: &[HostSnapshot],
    numeric_note: Option<String>,
) -> anyhow::Error {
    // Count USABLE devices (usable=true) to match the picker's
    // published set — unusable slots keep their real names for the
    // DirectSound-hint check but do not count as "searchable" mics.
    let searched_snapshots: Vec<&HostSnapshot> = host_snapshots
        .iter()
        .filter(|h| h.enumeration_error.is_none())
        .collect();
    let total_devices: usize = searched_snapshots
        .iter()
        .map(|h| h.usable.iter().filter(|u| **u).count())
        .sum();
    let hosts_tried: Vec<&str> = searched_snapshots.iter().map(|h| h.host_label).collect();
    let hosts_str = if hosts_tried.is_empty() {
        String::from("no hosts")
    } else {
        hosts_tried.join(", ")
    };
    let index_note = numeric_note.map(|e| format!("; {e}")).unwrap_or_default();
    // Failed-host errors surfaced in a separate clause so an outage
    // (e.g. WASAPI/ALSA hiccup) is separable from a stale device name
    // in the aggregate error text.
    let failure_clauses: Vec<String> = host_snapshots
        .iter()
        .filter_map(|h| h.enumeration_error.clone())
        .collect();
    let failure_note = if failure_clauses.is_empty() {
        String::new()
    } else {
        format!("; enumeration failures: {}", failure_clauses.join(", "))
    };
    // DirectSound-hint suppression: consult the FULL enumerated cpal
    // names (usable + unusable across every host) so a
    // visible-but-unopenable device doesn't get the false "only
    // visible via DirectSound" claim.
    let all_cpal_names: Vec<&str> = host_snapshots
        .iter()
        .flat_map(|h| h.device_names.iter().map(|s| s.as_str()))
        .collect();
    let ds_hint = directsound_only_hint(selector, &all_cpal_names).unwrap_or_default();
    anyhow::anyhow!(
        "input device not found: {selector:?} \
         (searched {total_devices} device(s) across {} host(s): {hosts_str}{index_note}{failure_note}{ds_hint})",
        searched_snapshots.len(),
    )
}

#[cfg(test)]
#[path = "hosts_tests.rs"]
mod tests;
