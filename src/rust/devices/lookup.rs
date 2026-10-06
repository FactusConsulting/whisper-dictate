//! Pure name-matching helpers for the microphone picker: the picker
//! precedence lookup and the bidirectional substring rule shared with the
//! DirectSound de-duplication pass.

use super::DeviceInfo;

/// Pure name lookup. Exposed so the test suite can exercise it against a
/// hand-rolled device list without depending on a live audio backend.
///
/// See `find_device_by_name` (in the module root) for the precedence rules; the longest-substring
/// tie-breaker matters because PortAudio's MME path truncates names to 31
/// chars, and a saved MME value must bind to its full WASAPI sibling — not to
/// a generic prefix like "Microphone".
pub fn find_in<'a>(devices: &'a [DeviceInfo], query: &str) -> Option<&'a DeviceInfo> {
    let needle = query.trim();
    if needle.is_empty() {
        return None;
    }
    let folded = needle.to_lowercase();
    // 1. exact case-insensitive match wins
    if let Some(hit) = devices.iter().find(|d| d.name.to_lowercase() == folded) {
        return Some(hit);
    }
    // 2. bidirectional substring match — either side may be the prefix.
    //    Iterate the whole list and keep the entry with the LONGEST matching
    //    name, so a truncated MME value maps to the fullest WASAPI sibling
    //    rather than to a shorter generic match.
    let mut best: Option<&DeviceInfo> = None;
    for d in devices {
        let lower = d.name.to_lowercase();
        if lower.is_empty() {
            continue;
        }
        if !(lower.contains(&folded) || folded.contains(&lower)) {
            continue;
        }
        match best {
            None => best = Some(d),
            Some(prev) if d.name.len() > prev.name.len() => best = Some(d),
            _ => {}
        }
    }
    best
}

/// Case-insensitive bidirectional substring match — either string may contain
/// DirectSound names against WASAPI names for the same physical mic, since the
/// two Windows host APIs annotate the same device slightly differently.
pub(crate) fn name_matches(a: &str, b: &str) -> bool {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.contains(&b) || b.contains(&a)
}

#[cfg(test)]
mod tests {
    use super::{find_in, name_matches};
    use crate::devices::DeviceInfo;

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
    fn find_in_empty_query_returns_none() {
        let devs = vec![make(0, "Microphone", false)];
        assert!(find_in(&devs, "").is_none());
        assert!(find_in(&devs, "   ").is_none());
    }

    #[test]
    fn find_in_exact_match_wins_over_substring() {
        // "Microphone" is a clean prefix of "Microphone Array" — the exact
        // hit must bind to the second entry, not the longer sibling.
        let devs = vec![
            make(0, "Microphone Array", false),
            make(1, "Microphone", false),
        ];
        let hit = find_in(&devs, "Microphone").expect("exact match");
        assert_eq!(hit.index, 1);
    }

    #[test]
    fn find_in_is_case_insensitive() {
        let devs = vec![make(0, "Headset Microphone (Jabra Evolve 65 TE)", false)];
        let hit = find_in(&devs, "HEADSET microphone (jabra evolve 65 te)").expect("hit");
        assert_eq!(hit.index, 0);
    }

    #[test]
    fn find_in_substring_match_either_direction() {
        // Saved name is the truncated MME 31-char value; device name is the
        // full WASAPI name. The bidirectional substring rule must still match.
        let devs = vec![make(0, "Headset Microphone (Jabra Evolve 65 TE)", false)];
        let saved = "Headset Microphone (Jabra Evolv"; // truncated to 31 chars
        let hit = find_in(&devs, saved).expect("truncated match");
        assert_eq!(hit.index, 0);

        // Reverse direction: saved is longer than device name.
        let devs2 = vec![make(0, "Microphone", false)];
        let saved_long = "Microphone (Realtek)";
        let hit2 = find_in(&devs2, saved_long).expect("longer-saved match");
        assert_eq!(hit2.index, 0);
    }

    #[test]
    fn find_in_prefers_longest_substring_match() {
        // Regression for the truncated-MME hijack bug: when a saved value is
        // a substring of MULTIPLE device names, we must bind to the LONGEST
        // (fullest) one — not the first match in iteration order. Without
        // this, a saved "Headset Microphone (Jabra Evolv" would resolve to
        // the generic "Headset Microphone" sibling and capture would record
        // from the wrong physical device.
        let devs = vec![
            make(0, "Headset Microphone", false),
            make(1, "Headset Microphone (Jabra Evolve 65 TE)", false),
            make(2, "Headset Microphone (USB)", false),
        ];
        let saved = "Headset Microphone (Jabra Evolv"; // truncated MME
        let hit = find_in(&devs, saved).expect("longest match");
        assert_eq!(hit.index, 1);
    }

    #[test]
    fn find_in_returns_none_when_no_match() {
        let devs = vec![make(0, "Built-in Microphone", false)];
        assert!(find_in(&devs, "Webcam").is_none());
    }

    #[test]
    fn name_matches_is_bidirectional_and_case_insensitive() {
        assert!(name_matches("Microphone (Realtek)", "microphone (realtek)"));
        // WASAPI full name contains the DirectSound truncation and vice versa.
        assert!(name_matches(
            "Headset Microphone (Jabra Evolve 65 TE)",
            "Headset Microphone"
        ));
        assert!(name_matches(
            "Headset Microphone",
            "Headset Microphone (Jabra Evolve 65 TE)"
        ));
        assert!(!name_matches("Webcam Mic", "Headset Microphone"));
        assert!(!name_matches("", "anything"));
        assert!(!name_matches("anything", "   "));
    }
}
