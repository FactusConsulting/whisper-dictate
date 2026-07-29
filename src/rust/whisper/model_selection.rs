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
#[path = "model_selection_tests.rs"]
mod tests;
