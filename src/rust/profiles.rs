use std::collections::BTreeMap;
use std::io::{self, Read};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct ApplyProfileRequest {
    base: BTreeMap<String, String>,
    profiles: Value,
    title: Option<String>,
    process: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileMatch {
    pub name: Option<String>,
    pub settings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyProfileResult {
    pub config: BTreeMap<String, String>,
    pub name: Option<String>,
}

pub fn handle_apply_profile() -> Result<()> {
    let request = read_request()?;
    let result = apply_profile_settings(
        request.base,
        &request.profiles,
        request.title.as_deref(),
        request.process.as_deref(),
    );
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn apply_profile_settings(
    base: BTreeMap<String, String>,
    profiles: &Value,
    title: Option<&str>,
    process: Option<&str>,
) -> ApplyProfileResult {
    let matched = match_profile(profiles, title, process);
    if matched.settings.is_empty() {
        return ApplyProfileResult {
            config: base,
            name: matched.name,
        };
    }
    let mut config = base;
    config.extend(matched.settings);
    ApplyProfileResult {
        config,
        name: matched.name,
    }
}

pub fn match_profile(profiles: &Value, title: Option<&str>, process: Option<&str>) -> ProfileMatch {
    let Some(items) = profiles.as_array() else {
        return ProfileMatch {
            name: None,
            settings: BTreeMap::new(),
        };
    };

    for profile in items {
        let Some(profile_object) = profile.as_object() else {
            continue;
        };
        let Some(match_object) = profile_object.get("match").and_then(Value::as_object) else {
            continue;
        };
        if !contains_any(title, match_object.get("title")) {
            continue;
        }
        if !contains_any(process, match_object.get("process")) {
            continue;
        }
        let name = profile_object
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("unnamed")
            .to_owned();
        let settings = profile_object
            .get("settings")
            .and_then(Value::as_object)
            .map(|settings| {
                settings
                    .iter()
                    .filter_map(|(key, value)| {
                        setting_value(value).map(|value| (key.clone(), value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return ProfileMatch {
            name: Some(name),
            settings,
        };
    }

    ProfileMatch {
        name: None,
        settings: BTreeMap::new(),
    }
}

fn contains_any(haystack: Option<&str>, needles: Option<&Value>) -> bool {
    let values = values(needles)
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return true;
    }
    let text = haystack.unwrap_or_default().to_casefold();
    values
        .iter()
        .any(|value| text.contains(&value.to_casefold()))
}

fn values(raw: Option<&Value>) -> Vec<String> {
    match raw {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(value)) => vec![value.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(setting_value)
            .filter(|value| !value.trim().is_empty())
            .collect(),
        Some(value) => setting_value(value).into_iter().collect(),
    }
}

fn setting_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) if value.is_empty() => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(true) => Some("True".to_owned()),
        Value::Bool(false) => Some("False".to_owned()),
        value => Some(value.to_string()),
    }
}

trait Casefold {
    fn to_casefold(&self) -> String;
}

impl Casefold for str {
    fn to_casefold(&self) -> String {
        self.to_lowercase()
    }
}

fn read_request() -> Result<ApplyProfileRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_match_by_title_and_process_applies_settings() {
        let profiles = serde_json::json!([{
            "name": "Claude terminal",
            "match": {"title": "Claude Code", "process": "WindowsTerminal"},
            "settings": {"inject_mode": "paste", "lang": "en"}
        }]);
        let base = BTreeMap::from([
            ("inject_mode".to_owned(), "auto".to_owned()),
            ("lang".to_owned(), "da".to_owned()),
        ]);

        let result = apply_profile_settings(
            base,
            &profiles,
            Some("Claude Code - repo"),
            Some("WindowsTerminal.exe"),
        );

        assert_eq!(result.name.as_deref(), Some("Claude terminal"));
        assert_eq!(result.config["inject_mode"], "paste");
        assert_eq!(result.config["lang"], "en");
    }

    #[test]
    fn first_matching_profile_wins_and_empty_match_values_match_anything() {
        let profiles = serde_json::json!([
            {"name": "first", "match": {"title": []}, "settings": {"lang": "en"}},
            {"name": "second", "match": {"title": "Editor"}, "settings": {"lang": "da"}}
        ]);

        let matched = match_profile(&profiles, Some("Editor"), Some("Code.exe"));

        assert_eq!(matched.name.as_deref(), Some("first"));
        assert_eq!(matched.settings["lang"], "en");
    }

    #[test]
    fn ignores_invalid_profiles_and_empty_settings_values() {
        let profiles = serde_json::json!([
            "bad",
            {"name": "bad match", "match": "nope", "settings": {"lang": "en"}},
            {"name": "valid", "match": {"process": "code"}, "settings": {"lang": "da", "empty": "", "none": null}}
        ]);

        let matched = match_profile(&profiles, Some("anything"), Some("Code.exe"));

        assert_eq!(matched.name.as_deref(), Some("valid"));
        assert_eq!(matched.settings.len(), 1);
        assert_eq!(matched.settings["lang"], "da");
    }
}
