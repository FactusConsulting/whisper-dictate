//! Tests for [`super`] -- the shared per-utterance profile override slots.

use std::collections::BTreeMap;

use super::*;

fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn slots() -> ProfileOverrideSlots {
    ProfileOverrideSlots::new("test model is fixed")
}

#[test]
fn prompt_falls_back_to_base_and_static_terms_without_override() {
    let slots = slots();
    let terms = vec!["Codex".to_owned()];

    assert_eq!(
        slots.prompt(Some("base"), &terms),
        (Some("base".to_owned()), terms.clone())
    );
    assert_eq!(slots.prompt(None, &[]), (None, Vec::new()));
}

#[test]
fn prompt_override_wins_over_base_and_drops_terms() {
    let slots = slots();
    slots.apply(&settings(&[("initial_prompt", "  Project Aurora  ")]));

    assert_eq!(
        slots.prompt(Some("base"), &["Codex".to_owned()]),
        (Some("Project Aurora".to_owned()), Vec::new())
    );
}

#[test]
fn blank_prompt_override_clears_a_previous_override() {
    let slots = slots();
    slots.apply(&settings(&[("initial_prompt", "Project Aurora")]));
    slots.apply(&settings(&[("initial_prompt", "   ")]));

    assert_eq!(slots.prompt(Some("base"), &[]).0.as_deref(), Some("base"));
}

#[test]
fn prompt_override_is_replaced_when_the_next_profile_omits_it() {
    let slots = slots();
    slots.apply(&settings(&[("initial_prompt", "Project Aurora")]));
    slots.apply(&BTreeMap::new());

    assert_eq!(slots.prompt(Some("base"), &[]).0.as_deref(), Some("base"));
}

#[test]
fn absent_language_keeps_the_configured_hint() {
    let slots = slots();
    slots.apply(&BTreeMap::new());

    assert_eq!(slots.language(), None);
    assert_eq!(
        slots.language_or(Some("da".to_owned())).as_deref(),
        Some("da")
    );
}

#[test]
fn explicit_empty_language_resets_to_auto() {
    let slots = slots();
    slots.apply(&settings(&[("lang", "")]));

    assert_eq!(slots.language(), Some(None));
    assert_eq!(slots.language_or(Some("da".to_owned())), None);
}

#[test]
fn explicit_language_value_overrides_the_configured_hint() {
    let slots = slots();
    slots.apply(&settings(&[("language", " en ")]));

    assert_eq!(slots.language(), Some(Some("en".to_owned())));
    assert_eq!(
        slots.language_or(Some("da".to_owned())).as_deref(),
        Some("en")
    );
}

#[test]
fn lang_alias_is_honoured_when_language_is_absent() {
    let slots = slots();
    slots.apply(&settings(&[("lang", "fr")]));

    assert_eq!(slots.language(), Some(Some("fr".to_owned())));
}

#[test]
fn language_override_is_dropped_when_the_next_profile_omits_it() {
    let slots = slots();
    slots.apply(&settings(&[("lang", "fr")]));
    slots.apply(&BTreeMap::new());

    assert_eq!(slots.language(), None);
}

#[test]
fn model_warning_fires_once_per_value_and_resets_when_absent() {
    let slots = slots();
    let model = settings(&[("model", "alternate.bin")]);

    assert_eq!(slots.warned_model(), None);
    slots.apply(&model);
    assert_eq!(slots.warned_model().as_deref(), Some("alternate.bin"));
    // Repeating the same value keeps the dedupe slot (no second warning).
    slots.apply(&model);
    assert_eq!(slots.warned_model().as_deref(), Some("alternate.bin"));
    // A different value re-warns and replaces the slot.
    slots.apply(&settings(&[("model", "other.bin")]));
    assert_eq!(slots.warned_model().as_deref(), Some("other.bin"));
    // Absent (or blank) clears it so the next pin warns again.
    slots.apply(&settings(&[("model", " ")]));
    assert_eq!(slots.warned_model(), None);
}

#[test]
fn guards_fold_profile_thresholds_only_when_bound() {
    let unbound = slots();
    unbound.apply(&settings(&[("min_input_dbfs", "-70")]));
    assert_ne!(unbound.guards().thresholds.min_input_dbfs, -70.0);

    let bound = slots();
    bound.set_guards(TranscriptionGuards::from_lookup(|_| None));
    bound.apply(&settings(&[("min_input_dbfs", "-70")]));
    assert_eq!(bound.guards().thresholds.min_input_dbfs, -70.0);
    // The preview-shared handle observes the same live values.
    assert_eq!(
        bound
            .shared_guards()
            .lock()
            .unwrap()
            .unwrap()
            .thresholds
            .min_input_dbfs,
        -70.0
    );
}

#[test]
fn shared_language_handle_observes_live_overrides() {
    let slots = slots();
    let shared = slots.shared_language();
    slots.apply(&settings(&[("lang", "")]));

    assert_eq!(*shared.lock().unwrap(), Some(None));
}
