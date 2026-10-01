use super::{InjectMethod, InjectRequest, InjectResponse};
use crate::injection::paste::PasteShortcut;

#[test]
fn facade_retains_method_serde_shape() {
    assert_eq!(
        serde_json::to_string(&InjectMethod::Typing).unwrap(),
        r#""typing""#
    );
    assert_eq!(
        serde_json::to_string(&InjectMethod::Paste(Some(PasteShortcut::CtrlV))).unwrap(),
        r#"{"paste":"ctrl_v"}"#
    );
}

#[test]
fn facade_retains_default_request_and_partial_response_contract() {
    let request: InjectRequest =
        serde_json::from_str(r#"{"action":"inject","text":"fixture"}"#).unwrap();
    assert!(matches!(request, InjectRequest::Inject { .. }));
    let response = InjectResponse {
        ok: false,
        error: Some("fixture".into()),
        method: "paste:auto".into(),
        partial: true,
    };
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        serde_json::json!({"ok":false,"error":"fixture","method":"paste:auto","partial":true})
    );
}
