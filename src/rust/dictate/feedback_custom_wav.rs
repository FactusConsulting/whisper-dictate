//! Optional per-user WAV overrides for the existing start/stop/done cues.

use std::path::{Path, PathBuf};

use super::CueKind;

const MAX_WAV_BYTES: u64 = 1024 * 1024;
const MAX_SECONDS: u32 = 3;

fn file_name(kind: CueKind) -> &'static str {
    match kind {
        CueKind::Start => "start.wav",
        CueKind::Stop => "stop.wav",
        CueKind::Done => "done.wav",
    }
}

pub(super) fn cue_file(kind: CueKind) -> Option<PathBuf> {
    let result = cue_file_in(&crate::config::platform_config_dir().join("sounds"), kind);
    if crate::diag::debug_enabled() {
        crate::diag::log!("{}", decision_line(kind, &result));
    }
    result.ok()
}

fn decision_line(kind: CueKind, result: &Result<PathBuf, &'static str>) -> String {
    match result {
        Ok(_) => format!("[feedback] cue={} backend=custom_wav", file_name(kind)),
        Err(reason) => format!(
            "[feedback] cue={} backend=default reason={reason}",
            file_name(kind)
        ),
    }
}

fn cue_file_in(dir: &Path, kind: CueKind) -> Result<PathBuf, &'static str> {
    let path = dir.join(file_name(kind));
    let metadata = std::fs::metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "missing"
        } else {
            "metadata_unreadable"
        }
    })?;
    if !metadata.is_file() {
        return Err("not_a_file");
    }
    if metadata.len() > MAX_WAV_BYTES {
        return Err("oversized");
    }
    let mut reader = hound::WavReader::open(&path).map_err(|_| "invalid_wav")?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
        || !(1..=2).contains(&spec.channels)
        || !(8_000..=48_000).contains(&spec.sample_rate)
    {
        return Err("unsupported_format");
    }
    let duration = reader.duration();
    if duration == 0 || duration > spec.sample_rate * MAX_SECONDS {
        return Err("invalid_duration");
    }
    // PCM16 has fixed-width samples. If the declared last frame can be read,
    // the entire data payload is present; seeking keeps cue selection bounded
    // even for the largest accepted file.
    reader.seek(duration - 1).map_err(|_| "truncated_payload")?;
    let mut samples = reader.samples::<i16>();
    for _ in 0..spec.channels {
        match samples.next() {
            Some(Ok(_)) => {}
            _ => return Err("truncated_payload"),
        }
    }
    Ok(path)
}

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn PlaySoundW(sound: *const u16, module: *mut core::ffi::c_void, flags: u32) -> i32;
}

#[cfg(windows)]
pub(super) fn play_windows(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;

    const SND_NODEFAULT: u32 = 0x0002;
    const SND_FILENAME: u32 = 0x0002_0000;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and lives through synchronous playback.
    // SND_NODEFAULT prevents an unrelated system sound on a bad WAV file.
    let played = unsafe {
        PlaySoundW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_NODEFAULT,
        ) != 0
    };
    if !played && crate::diag::debug_enabled() {
        crate::diag::log!("[feedback] backend=default reason=custom_play_failed");
    }
    played
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, sample_rate: u32, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..samples {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn only_valid_short_event_wavs_override_the_default_cue() {
        let dir = tempfile::tempdir().unwrap();
        write_wav(&dir.path().join("start.wav"), 8_000, 800);
        write_wav(&dir.path().join("stop.wav"), 8_000, 24_001);
        std::fs::write(dir.path().join("done.wav"), b"not a WAV").unwrap();
        assert_eq!(
            cue_file_in(dir.path(), CueKind::Start),
            Ok(dir.path().join("start.wav"))
        );
        assert_eq!(
            cue_file_in(dir.path(), CueKind::Stop),
            Err("invalid_duration")
        );
        assert_eq!(cue_file_in(dir.path(), CueKind::Done), Err("invalid_wav"));
    }

    #[test]
    fn missing_and_oversized_event_wavs_fall_back_independently() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cue_file_in(dir.path(), CueKind::Start), Err("missing"));
        write_wav(&dir.path().join("done.wav"), 48_000, 48);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("done.wav"))
            .unwrap();
        file.set_len(MAX_WAV_BYTES + 1).unwrap();
        assert_eq!(cue_file_in(dir.path(), CueKind::Done), Err("oversized"));
    }

    #[test]
    fn truncated_pcm_payload_is_rejected_before_playback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("start.wav");
        write_wav(&path, 8_000, 800);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let original = file.metadata().unwrap().len();
        file.set_len(original - 100).unwrap();
        assert_eq!(
            cue_file_in(dir.path(), CueKind::Start),
            Err("truncated_payload")
        );
    }

    #[test]
    fn diagnostics_name_decision_without_disclosing_the_user_path() {
        let private = PathBuf::from("C:/Users/secret/sounds/start.wav");
        let selected = decision_line(CueKind::Start, &Ok(private));
        let rejected = decision_line(CueKind::Stop, &Err("invalid_wav"));
        assert_eq!(selected, "[feedback] cue=start.wav backend=custom_wav");
        assert_eq!(
            rejected,
            "[feedback] cue=stop.wav backend=default reason=invalid_wav"
        );
        assert!(selected.is_ascii() && rejected.is_ascii());
        assert!(!selected.contains("secret"));
    }
}
