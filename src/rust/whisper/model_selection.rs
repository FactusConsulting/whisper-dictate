//! Catalog model selection shared by feature-gated inference paths.

use anyhow::{anyhow, Result};

use super::model_manager::{self, ModelEntry, CATALOG};

pub(crate) fn select_downloaded_model<F>(
    requested: Option<&str>,
    is_downloaded: F,
) -> Result<Option<&'static ModelEntry>>
where
    F: Fn(&ModelEntry) -> bool,
{
    let requested = requested.map(str::trim).filter(|value| !value.is_empty());
    if let Some(name) = requested {
        let entry = model_manager::find(name).ok_or_else(|| {
            let available = model_manager::visible_catalog()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!("unknown Whisper model `{name}`; available: {available}")
        })?;
        if !is_downloaded(entry) {
            return Err(anyhow!(
                "selected Whisper model `{name}` is not downloaded; run \
                 `whisper-dictate models download {name}`"
            ));
        }
        return Ok(Some(entry));
    }

    Ok(CATALOG.iter().find(|entry| is_downloaded(entry)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_model_wins_when_multiple_catalog_models_are_downloaded() {
        let selected = select_downloaded_model(Some("large-v3"), |entry| {
            matches!(entry.name, "large-v3-turbo" | "large-v3")
        })
        .unwrap()
        .expect("selected model");
        assert_eq!(selected.name, "large-v3");
    }

    #[test]
    fn selected_model_must_exist_and_be_downloaded() {
        let missing = select_downloaded_model(Some("large-v3"), |_| false).unwrap_err();
        assert!(missing.to_string().contains("models download large-v3"));

        let unknown = select_downloaded_model(Some("not-a-model"), |_| true).unwrap_err();
        assert!(unknown.to_string().contains("unknown Whisper model"));
    }
}
