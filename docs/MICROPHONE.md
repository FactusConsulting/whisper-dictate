# Microphone & audio diagnostics

whisper-dictate prints per-utterance diagnostics so you can tell whether your
**microphone/room** — not the Whisper model — is the bottleneck. Three lines
per utterance:

```text
[cap] raw=-44dBFS peak=0.066 gain=15.0x noise=-84dBFS snr=45dB
[gate] raw=-44dBFS noise=-84dBFS snr=45dB
[stt] dur=5.1s post-boost=-20dBFS compute=0.6s text='…'
```

`dBFS` = decibels relative to digital full scale: `0` is the maximum a sample
can hold, more negative is quieter. There is no positive dBFS.

## What each number means

### `raw` — input loudness before any processing (dBFS) — *context, not quality*

RMS level of the captured audio. Soft, close speech typically lands around
**−35 to −45 dBFS**. Below **−55 dBFS** the utterance is rejected as *"input too
quiet"* — speak up, move closer, or raise the OS input level. It is normalised
away before Whisper sees it, so a low `raw` is not bad *per se*; but very low
`raw` forces a high `gain`, which also amplifies noise.

### `peak` — loudest single sample of the raw input (0…1, linear) — *clipping guard*

The boost is hard-capped at `0.99 / peak`, so the signal can **never clip**.
`peak` near **1.0** means you're hitting the ceiling — back off or lower the
input gain. A healthy `peak` is roughly **0.05–0.5**.

### `gain` — how hard quiet input was boosted (×) — *lower is better*

`gain = 10^((target − raw)/20)`, target = −20 dBFS, clamped by the no-clip cap.
A hot, clean mic needs little boost (**≈1–5×**). **15–50×** means very quiet
input: usable, but every dB of room hiss is boosted along with your voice.

### `noise` — the noise floor (dBFS) — **lower (more negative) = better**

The quiet frames *between* words (10th-percentile per-30 ms-frame RMS). This is
a real property of your **mic + room** (fans, AC, electrical hiss). Clean setups
sit around **−75 to −90 dBFS**; **above −60 dBFS** is a noisy mic/room.

### `snr` — signal-to-noise ratio (dB) — **higher = better; the number that matters most**

How far speech sits above the noise floor. It is **gain-invariant** — a louder
mic cannot flatter it — so it is the honest quality metric:

| SNR | Verdict |
|---|---|
| ≳ 25 dB | excellent |
| 15–25 dB | workable |
| < 15 dB | the mic or room is the limiting factor |
| < 6 dB | rejected: *"no speech contrast"* |

### `post-boost` — loudness after normalisation (dBFS) — *should be ≈ −20*

Confirms the boost reached the −20 dBFS target. Far from −20 means the gain was
clip-limited (peak too high) — reduce the input level.

### `compute` — transcription time (seconds) — **speed, not quality**

Wall-clock for the Whisper model. GPU/CUDA ≈ 0.3–1 s; CPU is several seconds.
Affects latency only, never accuracy. `dur` is how long you spoke.

## Is my microphone good? (quick rubric)

- **SNR ≥ 25 dB and noise ≤ −75 dBFS** → great mic/room; Whisper is now the only
  limit. Improve accuracy with `--lang`, `VOICEPI_INITIAL_PROMPT`, a bigger
  `--model`, or `VOICEPI_BEAM_SIZE` (see CONFIGURATION.md).
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

Capture thresholds (env vars): `VOICEPI_TARGET_DBFS` (−20; *lower*, e.g. −16,
boosts quiet speech harder), `VOICEPI_MIN_INPUT_DBFS` (−55; reject-too-quiet
gate), `VOICEPI_MIN_SNR_DB` (6; reject-no-contrast gate). These shape capture,
not recognition. Recognition-accuracy levers are separate and are documented in
CONFIGURATION.md — they do **not** fix a low-SNR microphone.
