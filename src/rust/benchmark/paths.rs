//! Per-user audio-directory helpers for the golden-benchmark corpus.
//!
//! Rust port of the two `vp_benchmark_paths` helpers that the corpus resolver
//! in `crate::corpus` did not need but the native benchmark runner does:
//!
//! * [`appdata_audio_dir`] — the per-user, gitignored recordings directory
//!   `<appdata>/benchmark/audio/`. The runner surfaces it in the audio-hint
//!   line the summary appends when every corpus item was skipped for missing
//!   audio.
//! * [`resolve_item_audio`] — walk the same fallback chain as
//!   `vp_benchmark_paths.resolve_item_audio`: the manifest-relative recording
//!   first, then `<appdata>/benchmark/audio/<file-name>` if the former is
//!   missing, so a user who keeps recordings in the survives-reinstall dir
//!   never sees "audio file missing" for an item they in fact recorded.
//!
//! Kept in a separate module so the corpus loader in `crate::corpus` can stay
//! IO-free (only `Path::exists` checks) and unit-testable without dragging in
//! the audio-resolution filesystem fallbacks. Pure — the only side effect is
//! `Path::exists` checks — so the fallback rule is unit-testable in a
//! sandboxed tempdir without needing a real corpus recording.

use std::path::{Path, PathBuf};

/// Per-user benchmark audio directory (`<appdata>/benchmark/audio/`).
///
/// Matches `vp_benchmark_paths.appdata_audio_dir` byte-for-byte: two path
/// components appended to the caller-supplied appdata base so both the Python
/// and Rust code paths land on the same on-disk directory. Callers use it as
/// the "record corpus audio to <path>" hint in the summary line when every
/// item was skipped for missing audio.
pub fn appdata_audio_dir(appdata: &Path) -> PathBuf {
    appdata.join("benchmark").join("audio")
}

/// Resolve a corpus item's audio recording, falling back to the per-user dir.
///
/// Precedence (matches `vp_benchmark_paths.resolve_item_audio`):
///
/// 1. `audio` verbatim if it exists — the manifest-relative recording shipped
///    with the corpus or recorded next to it.
/// 2. `<appdata>/benchmark/audio/<file-name>` if that exists — the per-user
///    dir that survives reinstalls.
/// 3. `audio` verbatim otherwise — the caller records the item as a skip with
///    reason `MISSING_AUDIO_REASON`.
///
/// The `appdata` fallback is only consulted when supplied; a `None` argument
/// disables it (used by tests that want to pin the "no fallback" branch).
pub fn resolve_item_audio(audio: &Path, appdata: Option<&Path>) -> PathBuf {
    if audio.exists() {
        return audio.to_path_buf();
    }
    let Some(appdata) = appdata else {
        return audio.to_path_buf();
    };
    let Some(name) = audio.file_name() else {
        return audio.to_path_buf();
    };
    let fallback = appdata_audio_dir(appdata).join(name);
    if fallback.exists() {
        fallback
    } else {
        audio.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn appdata_audio_dir_appends_benchmark_slash_audio() {
        let base = Path::new("/tmp/whisper-dictate");
        let expected = base.join("benchmark").join("audio");
        assert_eq!(appdata_audio_dir(base), expected);
    }

    #[test]
    fn resolve_returns_original_when_it_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = tmp.path().join("recording.wav");
        fs::write(&audio, b"riff").unwrap();
        // Even with an appdata base whose fallback does not exist, an
        // existing manifest-relative path is returned verbatim.
        let appdata = tmp.path().join("appdata");
        let resolved = resolve_item_audio(&audio, Some(&appdata));
        assert_eq!(resolved, audio);
    }

    #[test]
    fn resolve_falls_back_to_appdata_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = tmp.path().join("missing").join("item.wav"); // doesn't exist
        let appdata = tmp.path().join("appdata");
        let per_user_dir = appdata_audio_dir(&appdata);
        fs::create_dir_all(&per_user_dir).unwrap();
        let per_user = per_user_dir.join("item.wav");
        fs::write(&per_user, b"riff").unwrap();
        let resolved = resolve_item_audio(&audio, Some(&appdata));
        assert_eq!(resolved, per_user);
    }

    #[test]
    fn resolve_returns_original_when_both_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = tmp.path().join("nowhere.wav");
        let appdata = tmp.path().join("appdata"); // no fallback dir
        let resolved = resolve_item_audio(&audio, Some(&appdata));
        assert_eq!(resolved, audio);
    }

    #[test]
    fn resolve_without_appdata_returns_original() {
        let tmp = tempfile::tempdir().unwrap();
        let audio = tmp.path().join("nowhere.wav");
        // No appdata argument → no fallback consulted at all.
        let resolved = resolve_item_audio(&audio, None);
        assert_eq!(resolved, audio);
    }
}
