//! Env-flag parsing helpers used by the native runtime.
//!
//! Pure: callers pass the already-fetched env values (or `None`),
//! so the functions stay testable without mutating the process env.

/// `True` when the value is non-empty and not one of the "off-ish"
/// strings config flags accept. Rule:
/// strip, lowercase, reject the disable set
/// `("", "0", "false", "no", "off")`.
pub fn is_truthy(value: Option<&str>) -> bool {
    let trimmed = value.unwrap_or("").trim().to_lowercase();
    !matches!(trimmed.as_str(), "" | "0" | "false" | "no" | "off")
}

/// Normalise a JSON config value the same way
/// `config::schema::value_to_env_string` does, so a `bool` or a string
/// feeds the same env-style parsing. `null` and empty strings are treated
/// as unset so callers fall through to the env layer.
pub(crate) fn value_as_env_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(v) if v.is_empty() => None,
        serde_json::Value::String(v) => Some(v.clone()),
        serde_json::Value::Bool(true) => Some("1".to_owned()),
        serde_json::Value::Bool(false) => Some("0".to_owned()),
        other => Some(other.to_string()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_recognises_non_empty_value() {
        assert!(is_truthy(Some("1")));
        assert!(is_truthy(Some("on")));
        assert!(is_truthy(Some("yes")));
        assert!(is_truthy(Some("true")));
        // Anything not in the disable set counts as truthy.
        assert!(is_truthy(Some("anything-else")));
    }

    #[test]
    fn falsy_inputs_are_rejected() {
        for v in ["", "0", "false", "FALSE", " False ", "no", "off"] {
            assert!(!is_truthy(Some(v)), "expected {v:?} to be falsy");
        }
        assert!(!is_truthy(None));
    }

    #[test]
    fn trim_and_case_insensitive() {
        assert!(!is_truthy(Some("  Off  ")));
        assert!(is_truthy(Some("  YES  ")));
    }
}
