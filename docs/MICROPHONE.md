# Microphone & audio diagnostics

In **Live dictation**, hold the push-to-talk shortcut to see the microphone's
input level. If you use toggle mode, start recording to see the
level. The bars stay dark in **Ready** because the app has closed the microphone.

Linux can list software-routed audio inputs as well as hardware microphones.
The `null` source generates silence, so it is not offered as a microphone
or used as the default. A source named `Monitor of …` carries playback from
an output device, not speech from a microphone.

The **Runtime** log shows microphone quality on each measured dictation, next to
the transcript. If the recording was measured but produced no text, its card
shows the microphone assessment separately from the failure reason. The
assessment describes the captured signal, not how accurately the model
transcribed it. Recordings with too little audio to measure have no assessment.

The card shows signal-to-noise ratio (SNR) and input level. Switch the log to
**Diagnostic** for peak and estimated noise floor. To take a separate
five-second measurement without transcribing, run `wd calibrate-mic 5`.

`dBFS` = decibels relative to digital full scale: `0` is the maximum a sample
can hold, more negative is quieter. There is no positive dBFS.

## What each number means

### Input level — loudness before normalization (dBFS)

RMS level of the captured audio. Soft, close speech typically lands around
**−35 to −45 dBFS**. The default speech gate rejects input below **−55 dBFS**.
If the app reports low input, move closer, speak louder, or raise the OS input
level. Normalization raises both speech and background noise; it does not
improve their ratio.

### `peak` — loudest single sample of the raw input (0…1, linear) — *clipping guard*

Normalization caps the boost at `0.99 / peak` so it does not introduce
clipping. A peak near **1.0** means the original input may already be clipping.
Lower the input gain or move farther from the microphone.

### Gain — normalization multiplier (×)

The target is −20 dBFS by default. On a rejected recording no boost was applied.

### `noise` — the noise floor (dBFS) — **lower (more negative) = better**

The quiet frames *between* words (10th-percentile per-30 ms-frame RMS). This is
a real property of your **mic + room** (fans, AC, electrical hiss). Clean setups
sit around **−75 to −90 dBFS**; **above −60 dBFS** is a noisy mic/room.

### `snr` — signal-to-noise ratio (dB) — **higher = better; the number that matters most**

This is an estimate from the recording, not a room-noise measurement or a
recognition score. A uniform gain change does not improve it.

| SNR | How to read it |
|---|---|
| 25 dB or higher | Strong speech contrast |
| 15–25 dB | Enough contrast for typical dictation |
| 6–15 dB | Usable if the input level is healthy, but background noise may interfere |
| Below 6 dB | Rejected by the default speech gate |

## Is my microphone good? (quick rubric)

- **SNR ≥ 25 dB and noise ≤ −75 dBFS** → strong mic/room result. If dictation
  is still inaccurate, check the language, dictionary and model settings.
- **SNR 15–25 dB** → fine for dictation; move closer / reduce room noise to gain
  headroom.
- **SNR < 15 dB or noise > −60 dBFS** → fix the mic/room **first**. No model
  setting recovers SNR that was never captured.

## Comparing two microphones

Compare in this order: **SNR first** (gain-invariant), then **noise floor**
(lower = cleaner), then how little **gain** is needed. Ignore `raw`/`peak` for
the comparison — both inputs are normalised to −20 dBFS anyway.

Worked example:

| | Mic #1 | Mic #2 |
|---|---|---|
| snr | 45 dB | 44 dB |
| noise | −84 dBFS | −78 dBFS |
| gain | 15× | 9.3× |

Both are excellent (SNR ~44–45 dB). The 1 dB SNR difference is within noise — a
tie. Mic #1 wins on a **6 dB lower noise floor** (cleaner background); Mic #2
only leads on `raw`/`gain`, which is normalised away. **→ pick Mic #1.**

## Related tuning

Capture thresholds (env vars): `VOICEPI_TARGET_DBFS` (−20 dBFS; a higher value,
such as −16, boosts quiet speech harder), `VOICEPI_MIN_INPUT_DBFS` (−55; reject-too-quiet
gate), `VOICEPI_MIN_SNR_DB` (6; reject-no-contrast gate). These shape capture,
not recognition. Recognition-accuracy levers are separate and are documented in
CONFIGURATION.md — they do **not** fix a low-SNR microphone.
