//! Platform glue for the desktop UI: the STT backend mode enum, Wayland/XKB
//! keyboard-layout detection, and the OS "open URL" shell-out.

use super::*;
use anyhow::Result;
use std::process::Command;

pub(in crate::ui) const XKB_LAYOUT_ENV: &str = "VOICEPI_XKB_LAYOUT";
const SUPPORTED_XKB_LAYOUTS: &[&str] = &[
    "dk", "no", "se", "de", "fi", "es", "pt", "br", "pl", "ua", "us",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum SttBackendMode {
    Whisper,
    Parakeet,
    Cloud,
}

impl SttBackendMode {
    pub(in crate::ui) fn from_raw(raw: &str) -> Self {
        match raw {
            "parakeet" => Self::Parakeet,
            "openai" => Self::Cloud,
            _ => Self::Whisper,
        }
    }
}

pub(in crate::ui) fn effective_xkb_layout(settings: &AppSettings) -> Option<String> {
    if let Some(configured) = normalize_xkb_layout(&settings.xkb_layout) {
        return Some(configured);
    }
    detect_gnome_xkb_layout()
}

pub(in crate::ui) fn normalize_xkb_layout(raw: &str) -> Option<String> {
    let layout = match raw.trim() {
        "da" => "dk",
        "sv" => "se",
        "nb" | "nn" => "no",
        "uk" => "ua",
        value => value,
    };
    if SUPPORTED_XKB_LAYOUTS.contains(&layout) {
        Some(layout.to_owned())
    } else {
        None
    }
}

fn detect_gnome_xkb_layout() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "sources"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    parse_gnome_xkb_sources(&raw)
}

pub(in crate::ui) fn parse_gnome_xkb_sources(raw: &str) -> Option<String> {
    for entry in raw.split('(').skip(1) {
        let Some(entry) = entry.split(')').next() else {
            continue;
        };
        let mut values = entry
            .split(',')
            .map(|part| part.trim().trim_matches('\'').trim_matches('"'));
        let kind = values.next().unwrap_or_default();
        let layout = values.next().unwrap_or_default();
        let layout = normalize_xkb_layout(layout);
        if kind == "xkb" && layout.as_deref().is_some_and(|value| value != "us") {
            return layout;
        }
    }
    None
}

pub(in crate::ui) fn open_url(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        command
            .args(["/C", "start", "", url])
            .creation_flags(0x08000000);
        command.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
}
