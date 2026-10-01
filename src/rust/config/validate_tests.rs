use crate::config::AppSettings;

#[test]
fn persistence_validation_rejects_nonfinite_and_out_of_schema_numeric_values() {
    for (key, value) in [
        ("release_tail_ms", "60000"),
        ("release_tail_ms", "NaN"),
        ("min_record_seconds", "-1"),
        ("target_dbfs", "inf"),
        ("command_hook_timeout_ms", "600001"),
        ("update_check_interval_minutes", "1"),
    ] {
        let mut fields = serde_json::to_value(AppSettings::default()).unwrap();
        fields[key] = serde_json::json!(value);
        let settings: AppSettings = serde_json::from_value(fields).unwrap();
        let error = settings.validate().unwrap_err().to_string();
        assert!(error.contains(key), "{error}");
    }
}
