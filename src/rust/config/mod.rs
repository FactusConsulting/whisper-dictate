//! Configuration: the typed [`AppSettings`] model and everything that loads,
//! validates, persists, and maps it to the worker environment.
//!
//! The module is split into focused submodules:
//! - [`schema`] — the embedded `settings_schema.json` (single source of truth)
//!   and the derived worker environment.
//! - [`settings`] — the [`AppSettings`] struct and its defaults.
//! - [`load`] / [`save`] / [`validate`] — `from_value`, `apply_to_object`, and
//!   `validate` as separate `impl AppSettings` blocks.
//! - [`keys`] — the owned-key catalogs and restart-impact comparison.
//! - [`io`] — config-file location, read/write, dictionary/history helpers, and
//!   opening paths in the platform file manager.
//!
//! Everything below is re-exported so existing `crate::config::NAME` paths keep
//! working regardless of which submodule now defines a given item.

mod io;
mod keys;
mod load;
mod save;
mod schema;
mod settings;
mod validate;

use anyhow::Result;

use crate::cli::ConfigCommand;

pub use io::{
    config_path, default_history_path, ensure_dictionary_file, load_raw_config, load_settings,
    open_dictionary, open_existing_path, platform_config_dir, save_settings, save_settings_to_path,
};
pub use keys::restart_required_keys;
pub use schema::{effective_runtime_env, numeric_bounds, worker_env_overrides, NumericBounds};
pub use settings::AppSettings;

/// Dispatch the `config` CLI subcommand (print the path or the resolved config).
pub fn handle_command(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            println!("{}", config_path().display());
            Ok(())
        }
        ConfigCommand::Show => {
            let value = load_raw_config()?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keys::RESTART_KEYS;
    use super::schema::SETTINGS_SCHEMA_JSON;
    use super::AppSettings;
    use serde_json::{Map, Value};

    #[test]
    fn restart_keys_match_non_live_schema_settings_plus_provider() {
        // RESTART_KEYS must stay consistent with the schema's `live` flag.
        // stt_provider is the one UI-only restart key not exported to the worker.
        let schema: Value = serde_json::from_str(SETTINGS_SCHEMA_JSON).unwrap();
        let mut expected: Vec<String> = schema["settings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| !s["live"].as_bool().unwrap_or(true))
            .map(|s| s["key"].as_str().unwrap().to_owned())
            .collect();
        expected.push("stt_provider".to_owned());
        expected.sort();
        let mut actual: Vec<String> = RESTART_KEYS.iter().map(|k| (*k).to_owned()).collect();
        actual.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn every_schema_setting_is_wired_into_app_settings() {
        // Adding a setting to settings_schema.json without wiring it into the
        // typed AppSettings (read by from_value, written by apply_to_object) is a
        // silent bug. Guard both halves so a forgotten field fails CI loudly.
        fn field_for(key: &str) -> &str {
            match key {
                "json_output" => "inject_json", // config/schema key vs struct field name
                other => other,
            }
        }

        let schema: Value = serde_json::from_str(SETTINGS_SCHEMA_JSON).unwrap();
        let default_json = serde_json::to_value(AppSettings::default()).unwrap();
        let mut all_probes = Map::new();
        let mut keys: Vec<String> = Vec::new();

        for entry in schema["settings"].as_array().unwrap() {
            let key = entry["key"].as_str().unwrap();
            let field = field_for(key);
            assert!(
                default_json.get(field).is_some(),
                "schema setting '{key}' has no matching AppSettings field '{field}'"
            );
            // A non-default probe value (always supplied as a JSON string, since
            // both string_value and bool_value read via as_str()).
            let probe = match &default_json[field] {
                Value::Bool(b) => Value::String(if *b { "0" } else { "1" }.to_owned()),
                Value::String(s) => Value::String(format!("{s}_wdprobe")),
                other => panic!("unexpected AppSettings field type for '{key}': {other}"),
            };
            // from_value must READ the key: the probe must change the field.
            let one = Value::Object([(key.to_owned(), probe.clone())].into_iter().collect());
            let parsed = serde_json::to_value(AppSettings::from_value(one).unwrap()).unwrap();
            assert_ne!(
                parsed[field], default_json[field],
                "AppSettings::from_value ignores schema setting '{key}'"
            );
            all_probes.insert(key.to_owned(), probe);
            keys.push(key.to_owned());
        }

        // apply_to_object must WRITE every schema key back.
        let settings = AppSettings::from_value(Value::Object(all_probes)).unwrap();
        let mut written = Map::new();
        settings.apply_to_object(&mut written);
        for key in &keys {
            assert!(
                written.contains_key(key),
                "AppSettings::apply_to_object does not persist schema setting '{key}'"
            );
        }
        // Re-reading the written object must reproduce the same settings, so
        // apply_to_object can't silently persist a wrong value (or a default)
        // under the right key.
        let reparsed = AppSettings::from_value(Value::Object(written)).unwrap();
        assert_eq!(
            reparsed, settings,
            "apply_to_object/from_value round-trip lost or corrupted a value"
        );
    }
}
