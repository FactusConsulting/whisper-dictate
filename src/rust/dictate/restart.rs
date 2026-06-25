//! Restart-required diff: mirror of `Dictate._report_restart_required`.
//!
//! Some live-reloadable settings only take effect after a worker restart
//! (the model load + the key-backend binding happen exactly once per
//! launch). Python's `Dictate._report_restart_required` walks a fixed
//! key-set and reports the subset whose value differs between the
//! previously-effective config and the freshly-reloaded one — that's
//! the user-facing `[config] updated settings require restart/model
//! reload: …` warning.
//!
//! The set is small + frozen, so we keep it as a `const` slice here.
//! Add new keys to BOTH sides (Python + Rust) when the supervisor grows
//! another restart-required knob.

use std::collections::BTreeMap;

/// Setting keys that only take effect after a worker restart. Mirrors
/// `Dictate._report_restart_required` in
/// `src/python/whisper_dictate/vp_dictate.py`. **Sorted alphabetically**
/// to keep the order the Python loop emits (`sorted(restart_keys)`).
pub const RESTART_REQUIRED_KEYS: &[&str] = &[
    "compute_type",
    "device",
    "key",
    "model",
    "parakeet_model",
    "stt_backend",
];

/// Return the alphabetically-sorted subset of [`RESTART_REQUIRED_KEYS`]
/// whose value differs between `before` and `after`. Missing keys are
/// treated as the empty string on both sides — same as Python's
/// `dict.get(k)` defaulting to `None`, which never equals a present
/// string value, but DOES equal another missing key.
///
/// Both inputs are `BTreeMap<String, String>` for deterministic
/// behaviour; pass a `from_iter` of the live and reloaded settings.
pub fn changed_restart_keys(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut changed = Vec::new();
    for key in RESTART_REQUIRED_KEYS {
        let lhs = before.get(*key).map(String::as_str).unwrap_or("");
        let rhs = after.get(*key).map(String::as_str).unwrap_or("");
        if lhs != rhs {
            changed.push((*key).to_owned());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map<'a, I: IntoIterator<Item = (&'a str, &'a str)>>(items: I) -> BTreeMap<String, String> {
        items
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    #[test]
    fn no_change_returns_empty() {
        let before = map([("model", "tiny"), ("device", "cpu")]);
        let after = before.clone();
        assert!(changed_restart_keys(&before, &after).is_empty());
    }

    #[test]
    fn single_changed_key_is_reported() {
        let before = map([("model", "tiny")]);
        let after = map([("model", "large-v3-turbo")]);
        assert_eq!(changed_restart_keys(&before, &after), vec!["model"]);
    }

    #[test]
    fn multiple_changed_keys_are_returned_sorted() {
        let before = map([
            ("model", "tiny"),
            ("device", "cpu"),
            ("stt_backend", "whisper"),
        ]);
        let after = map([
            ("model", "large-v3-turbo"),
            ("device", "cuda"),
            ("stt_backend", "parakeet"),
        ]);
        // Sorted: device, model, stt_backend.
        assert_eq!(
            changed_restart_keys(&before, &after),
            vec!["device", "model", "stt_backend"],
        );
    }

    #[test]
    fn unrelated_keys_are_ignored() {
        // min_record_seconds is live-reloadable, not restart-required.
        let before = map([("min_record_seconds", "0.5")]);
        let after = map([("min_record_seconds", "0.9")]);
        assert!(changed_restart_keys(&before, &after).is_empty());
    }

    #[test]
    fn missing_to_present_counts_as_change() {
        // A key absent on the LHS and present on the RHS is a change.
        let before = BTreeMap::new();
        let after = map([("key", "shift_r+ctrl_r")]);
        assert_eq!(changed_restart_keys(&before, &after), vec!["key"]);
    }

    #[test]
    fn both_missing_is_not_a_change() {
        // Two missing-on-both-sides entries match (Python: None == None).
        let before = BTreeMap::new();
        let after = BTreeMap::new();
        assert!(changed_restart_keys(&before, &after).is_empty());
    }

    #[test]
    fn keys_list_is_sorted_alphabetically() {
        // The const must stay sorted to mirror Python's sorted(restart_keys).
        let mut sorted = RESTART_REQUIRED_KEYS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, RESTART_REQUIRED_KEYS);
    }
}
