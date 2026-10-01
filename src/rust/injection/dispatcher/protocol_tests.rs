use super::{method_label, resolve_method, InjectMethodSpec, InjectMode, InjectRequest};
use crate::injection::dispatcher::InjectMethod;
use crate::injection::paste::PasteShortcut;

#[test]
fn successful_response_has_no_error_and_always_emits_partial_false() {
    let response =
        super::response_for_outcome(InjectMethod::Typing, super::super::InjectOutcome::ok());
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({
            "ok": true, "method": "typing", "partial": false
        })
    );
}

#[test]
fn failed_response_preserves_error_and_explicit_method_without_partial_progress() {
    let response = super::response_for_outcome(
        InjectMethod::Paste(Some(PasteShortcut::CtrlV)),
        super::super::InjectOutcome::failed(anyhow::anyhow!("synthetic failure")),
    );
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({
            "ok": false, "error": "synthetic failure", "method": "paste:ctrl_v", "partial": false
        })
    );
}

#[test]
fn partial_response_preserves_the_retry_barrier_and_automatic_paste_label() {
    let response = super::response_for_outcome(
        InjectMethod::Paste(None),
        super::super::InjectOutcome::partial(anyhow::anyhow!("synthetic partial failure")),
    );
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({
            "ok": false, "error": "synthetic partial failure", "method": "paste:auto", "partial": true
        })
    );
}

#[test]
fn resolve_method_defaults_to_paste_with_no_explicit_shortcut() {
    // Default spec (no shortcut field) ⇒ None so the dispatcher picks
    // the platform-appropriate shortcut at runtime — including the
    // Linux terminal-aware heuristic. P3 #371 finding 2: must be
    // distinct from an explicit caller-supplied default.
    let spec = InjectMethodSpec::default();
    assert_eq!(resolve_method(&spec).unwrap(), InjectMethod::Paste(None));
}

#[test]
fn resolve_method_empty_string_treated_as_no_preference() {
    let spec = InjectMethodSpec {
        mode: InjectMode::Paste,
        shortcut: Some(String::new()),
    };
    assert_eq!(resolve_method(&spec).unwrap(), InjectMethod::Paste(None));
}

#[test]
fn resolve_method_typing_ignores_shortcut() {
    let spec = InjectMethodSpec {
        mode: InjectMode::Typing,
        shortcut: Some("shift_insert".to_owned()),
    };
    assert_eq!(resolve_method(&spec).unwrap(), InjectMethod::Typing);
}

#[test]
fn resolve_method_honours_explicit_paste_shortcut() {
    let spec = InjectMethodSpec {
        mode: InjectMode::Paste,
        shortcut: Some("ctrl_shift_v".to_owned()),
    };
    assert_eq!(
        resolve_method(&spec).unwrap(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV))
    );
}

#[test]
fn resolve_method_preserves_explicit_default_value() {
    // P3 #371 finding 2 regression guard: an explicitly-supplied
    // "ctrl_v" (which happens to equal PasteShortcut::default() on
    // Linux/Windows) must NOT collapse to None — the dispatcher
    // must see Some(CtrlV) and honour it rather than running the
    // terminal-paste heuristic.
    let spec = InjectMethodSpec {
        mode: InjectMode::Paste,
        shortcut: Some("ctrl_v".to_owned()),
    };
    assert_eq!(
        resolve_method(&spec).unwrap(),
        InjectMethod::Paste(Some(PasteShortcut::CtrlV))
    );
}

#[test]
fn resolve_method_rejects_unknown_shortcut() {
    let spec = InjectMethodSpec {
        mode: InjectMode::Paste,
        shortcut: Some("ctrl_alt_y".to_owned()),
    };
    assert!(resolve_method(&spec).is_err());
}

#[test]
fn json_envelope_parses_inject_request() {
    let req: InjectRequest = serde_json::from_str(
        r#"{"action":"inject","text":"hi","method":{"mode":"paste","shortcut":"ctrl_v"}}"#,
    )
    .unwrap();
    match req {
        InjectRequest::Inject {
            text,
            method,
            target_title,
            target_process,
            xkb_layout,
        } => {
            assert_eq!(text, "hi");
            assert_eq!(method.mode, InjectMode::Paste);
            assert_eq!(method.shortcut.as_deref(), Some("ctrl_v"));
            assert!(target_title.is_empty());
            assert!(target_process.is_empty());
            assert!(xkb_layout.is_empty());
        }
        _ => panic!("expected Inject"),
    }
}

#[test]
fn json_envelope_parses_probe_request() {
    let req: InjectRequest = serde_json::from_str(r#"{"action":"probe"}"#).unwrap();
    assert!(matches!(req, InjectRequest::Probe));
}

#[test]
fn method_label_includes_paste_shortcut_name() {
    assert_eq!(
        method_label(InjectMethod::Paste(Some(PasteShortcut::CtrlShiftV))),
        "paste:ctrl_shift_v"
    );
    assert_eq!(method_label(InjectMethod::Typing), "typing");
}

#[test]
fn method_label_uses_auto_for_no_explicit_shortcut() {
    // `paste:auto` distinguishes "caller did not pin a shortcut, the
    // dispatcher picked one at runtime" from an explicit caller-pinned
    // shortcut in the response JSON. P3 #371 finding 2 surface.
    assert_eq!(method_label(InjectMethod::Paste(None)), "paste:auto");
}
