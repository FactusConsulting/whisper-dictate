use super::InjectMethod;
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
