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
    cue_file_in(&crate::config::platform_config_dir().join("sounds"), kind)
}

fn cue_file_in(dir: &Path, kind: CueKind) -> Option<PathBuf> {
    let path = dir.join(file_name(kind));
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_WAV_BYTES {
        return None;
    }
    let reader = hound::WavReader::open(&path).ok()?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
        || !(1..=2).contains(&spec.channels)
        || !(8_000..=48_000).contains(&spec.sample_rate)
        || reader.duration() == 0
        || reader.duration() > spec.sample_rate * MAX_SECONDS
    {
        return None;
    }
    Some(path)
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
    unsafe {
        PlaySoundW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_NODEFAULT,
        ) != 0
    }
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
            Some(dir.path().join("start.wav"))
        );
        assert_eq!(cue_file_in(dir.path(), CueKind::Stop), None);
        assert_eq!(cue_file_in(dir.path(), CueKind::Done), None);
    }

    #[test]
    fn missing_and_oversized_event_wavs_fall_back_independently() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cue_file_in(dir.path(), CueKind::Start), None);
        write_wav(&dir.path().join("done.wav"), 48_000, 48);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("done.wav"))
            .unwrap();
        file.set_len(MAX_WAV_BYTES + 1).unwrap();
        assert_eq!(cue_file_in(dir.path(), CueKind::Done), None);
    }
}
