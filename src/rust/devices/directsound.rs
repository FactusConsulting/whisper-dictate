//! Windows DirectSound capture enumeration and the merge of
//! DirectSound-only endpoint names into the picker list.
//!
//! The FFI pass is best-effort and Windows-only; every other target gets an
//! empty list so callers degrade to the cpal enumeration.
use super::enumeration::next_synthetic_from;
use super::lookup::name_matches;
use super::DeviceInfo;

/// Merge externally-enumerated capture-device names (currently Windows
/// DirectSound) into `out`, skipping any that already correspond to a device
/// cpal reported.
///
/// cpal on Windows enumerates WASAPI only, but the sounddevice picker
/// deliberately surfaces DirectSound-only inputs — a freshly docked/hot-plugged
/// USB mic can be visible on DirectSound before it appears on WASAPI. Without
/// this merge those mics would silently vanish from the picker once it defaults
/// to the Rust helper, even though the sounddevice capture path can still open
/// them. De-duplication uses the bidirectional-substring rule (not an exact
/// match) because DirectSound and WASAPI report slightly different name strings
/// for the same physical device.
///
/// Appended entries get synthetic indices after the existing range and a
/// nominal channel count: they exist so the NAME reaches the picker, and the
/// sounddevice capture path resolves the real device (and its true channel
/// sample-rate shape) from that name. This mirrors how the picker already
/// treats non-default-host entries as name-addressable, not index-addressable.
pub(crate) fn append_extra_named_devices(
    extra: &[String],
    out: &mut Vec<DeviceInfo>,
    seen_names: &mut Vec<String>,
) {
    let mut next = next_synthetic_from(out);
    for name in extra {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen_names.iter().any(|seen| name_matches(seen, trimmed)) {
            continue;
        }
        out.push(DeviceInfo {
            index: next,
            name: trimmed.to_owned(),
            max_input_channels: 1,
            sample_rates: (0, 0),
            default: false,
        });
        seen_names.push(trimmed.to_owned());
        next += 1;
    }
}

/// Enumerate Windows DirectSound *capture* device descriptions.
///
/// cpal is WASAPI-only on Windows, so this native `DirectSoundCaptureEnumerateW`
/// pass is the only way to see DirectSound-exclusive inputs (see
/// [`append_extra_named_devices`]). Best-effort: any failure yields an empty
/// list, so the picker degrades to the WASAPI set rather than erroring. On
/// non-Windows targets there is no DirectSound, so this is a no-op returning an
/// empty list.
#[cfg(windows)]
pub(super) fn directsound_capture_names() -> Vec<String> {
    ffi::capture_device_names()
}

#[cfg(not(windows))]
pub(super) fn directsound_capture_names() -> Vec<String> {
    Vec::new()
}

/// Public wrapper so `audio::hosts::directsound_only_hint` can consult the
/// same enumeration without duplicating the FFI shim. Same shape as the
/// private helper — Windows returns the `DirectSoundCaptureEnumerateW`
/// results, every other target returns an empty vector.
///
/// Callers should treat this as a diagnostic aid only: cpal 0.18 cannot
/// open DirectSound endpoints, so a name that appears here but nowhere in
/// cpal's WASAPI/ASIO enumeration is a mic the capture path cannot use.
pub fn directsound_capture_names_public() -> Vec<String> {
    directsound_capture_names()
}

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    use windows::core::{BOOL, GUID, PCWSTR};
    use windows::Win32::Media::Audio::DirectSound::DirectSoundCaptureEnumerateW;

    /// C callback invoked once per DirectSound capture device. `context` is a
    /// `*mut Vec<String>` we thread through so each description is collected.
    /// The first callback carries a NULL GUID (the "Primary Sound Capture
    /// Driver" alias for the default device); we keep it too, since the picker
    /// de-duplicates by name against the WASAPI list anyway.
    unsafe extern "system" fn enum_callback(
        guid: *mut GUID,
        description: PCWSTR,
        _module: PCWSTR,
        context: *mut c_void,
    ) -> BOOL {
        // The first callback carries a NULL GUID: the "Primary Sound Capture
        // Driver" alias for the system default. It has no stable physical-device
        // name and, since it can't match a real WASAPI entry, would surface as a
        // redundant picker option that merely re-selects the default. Skip it
        // the legacy DirectSound path filters this alias too.
        if !guid.is_null() && !context.is_null() && !description.is_null() {
            // SAFETY: `context` is the `&mut Vec<String>` we passed to
            // DirectSoundCaptureEnumerateW; the enumeration is synchronous so
            // the borrow is valid for the duration of every callback.
            let names = unsafe { &mut *(context as *mut Vec<String>) };
            if let Ok(text) = unsafe { description.to_string() } {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    names.push(trimmed.to_owned());
                }
            }
        }
        // TRUE → continue enumerating the remaining devices.
        BOOL(1)
    }

    pub(super) fn capture_device_names() -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let context = &mut names as *mut Vec<String> as *mut c_void;
        // SAFETY: `enum_callback` matches the LPDSENUMCALLBACKW signature and
        // `context` outlives the synchronous enumeration call.
        unsafe {
            let _ = DirectSoundCaptureEnumerateW(Some(enum_callback), Some(context));
        }
        // Opt-in diagnostic (`VOICEPI_DEBUG_DIRECTSOUND=1`): report the raw
        // DirectSound capture names on stderr BEFORE the picker de-duplicates
        // them against the WASAPI list. In steady state these mirror the cpal
        // devices (so they dedupe away); this makes it possible to confirm the
        // enumeration actually returned devices rather than silently finding
        // nothing. stderr keeps the JSON stdout envelope clean.
        if std::env::var("VOICEPI_DEBUG_DIRECTSOUND")
            .map(|v| !matches!(v.trim(), "" | "0" | "false" | "no" | "off"))
            .unwrap_or(false)
        {
            eprintln!(
                "[devices:directsound] enumerated {} capture device(s): {:?}",
                names.len(),
                names
            );
        }
        names
    }
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
    fn append_extra_adds_directsound_only_devices() {
        // WASAPI reported one mic; DirectSound also sees a freshly-docked USB
        // mic WASAPI hasn't surfaced yet. That one must be appended; the
        // already-present one (same physical device, slightly different name)
        // must be de-duplicated away.
        let mut out = vec![make(0, "Headset Microphone (Jabra Evolve 65 TE)", true)];
        let mut seen = vec!["Headset Microphone (Jabra Evolve 65 TE)".to_owned()];
        let ds = vec![
            "Headset Microphone (Jabra Evolve 65 TE)".to_owned(), // dup of WASAPI
            "Microphone (USB Docking Station)".to_owned(),        // DirectSound-only
            "   ".to_owned(),                                     // blank → skipped
        ];
        append_extra_named_devices(&ds, &mut out, &mut seen);

        assert_eq!(out.len(), 2);
        let added = &out[1];
        assert_eq!(added.name, "Microphone (USB Docking Station)");
        assert_eq!(added.index, 1); // synthetic index after the WASAPI range
        assert!(!added.default);
        assert!(added.max_input_channels >= 1);
    }

    #[test]
    fn append_extra_dedups_truncated_directsound_name() {
        // DirectSound truncates/annotates differently; the bidirectional
        // substring rule must treat it as the same device and NOT re-add it.
        let mut out = vec![make(0, "Microphone (Realtek(R) Audio)", true)];
        let mut seen = vec!["Microphone (Realtek(R) Audio)".to_owned()];
        let ds = vec!["Microphone (Realtek(R) Aud".to_owned()]; // truncated
        append_extra_named_devices(&ds, &mut out, &mut seen);
        assert_eq!(
            out.len(),
            1,
            "truncated DirectSound name must not duplicate the WASAPI entry"
        );
    }

    #[test]
    fn append_extra_into_empty_list_uses_zero_index() {
        // If cpal enumerated nothing (WASAPI hiccup) but DirectSound sees a
        // mic, it still reaches the picker starting at synthetic index 0.
        let mut out: Vec<DeviceInfo> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        append_extra_named_devices(&["Only DirectSound Mic".to_owned()], &mut out, &mut seen);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].index, 0);
        assert_eq!(out[0].name, "Only DirectSound Mic");
    }
}
