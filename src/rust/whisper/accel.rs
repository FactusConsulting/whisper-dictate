//! Which compute path whisper.cpp **actually** used, as opposed to which
//! one the build/config asked for.
//!
//! Startup reports a plan before the lazy model is loaded. The inference
//! path records an observed verdict only after verifying its Whisper state
//! on the selected GPU. This observer is diagnostic; it does not decide
//! whether inference may run.

use std::sync::atomic::{AtomicU8, Ordering};

use super::gpu::{should_use_gpu, GpuPolicy};

/// The compute path a transcription pass actually ran on.
///
/// Deliberately a small closed set matching the `stt_accel` field's
/// documented vocabulary (`vulkan` / `cuda` / `cpu` / `unknown`) so
/// history + metrics rows stay greppable. A GPU backend we do not have a
/// variant for (Metal, SYCL, HIP -- none of which we ship today) stays
/// [`Accel::Unknown`] rather than being mislabelled; add a variant here
/// when such a backend is actually shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accel {
    /// Nothing has been observed yet, or the signal was not recognised.
    #[default]
    Unknown,
    /// CPU backend signal (not used for local Whisper transcription).
    Cpu,
    /// whisper.cpp initialised a CUDA GPU backend.
    Cuda,
    /// whisper.cpp initialised a Vulkan GPU backend.
    Vulkan,
}

impl Accel {
    /// Wire label used for the `stt_accel` field and the startup line.
    /// ASCII, lowercase, stable -- downstream tooling greps these.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
        }
    }

    /// Encoding for the [`AccelObserver`] atomics.
    const fn as_u8(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Cpu => 1,
            Self::Cuda => 2,
            Self::Vulkan => 3,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Cpu,
            2 => Self::Cuda,
            3 => Self::Vulkan,
            _ => Self::Unknown,
        }
    }

    /// Confidence ranking used by [`AccelObserver::record`] so a later,
    /// weaker signal cannot downgrade a stronger one.
    ///
    /// whisper.cpp emits `use gpu = 1` (intent) BEFORE
    /// `whisper_backend_init_gpu: using Vulkan0 backend` (outcome), and on
    /// a fallback it emits `use gpu = 1` then `no GPU found`. Ranking a
    /// concrete GPU verdict above the CPU verdict means the arrival order
    /// of the two lines does not change the answer. `Unknown` ranks lowest
    /// so it never overwrites anything.
    const fn rank(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Cpu => 1,
            Self::Cuda | Self::Vulkan => 2,
        }
    }
}

/// Classify one whisper.cpp / ggml log line into an [`Accel`] signal.
///
/// Pure function -- the whole reason this file is compiled on stock
/// builds. Returns `None` for every line that says nothing about the
/// compute path (the overwhelming majority of whisper.cpp's banner).
///
/// Recognised diagnostic lines:
///
/// * `whisper_init_from_file_with_params_no_state: use gpu    = 0`
///   -> [`Accel::Cpu`]. `= 1` yields `None`: it is the REQUEST, not the
///   outcome, and treating it as a GPU verdict would reintroduce exactly
///   the lie this module exists to kill.
/// * `whisper_backend_init_gpu: no GPU found` -> [`Accel::Cpu`]. This is
///   the silent-fallback line: GPU was asked for and refused.
/// * `whisper_backend_init_gpu: using Vulkan0 backend` -> [`Accel::Vulkan`]
///   (`CUDA0` -> [`Accel::Cuda`]). Only the `using ... backend` form
///   counts as a GPU verdict; the sibling `device N: Vulkan0 (type: 1)`
///   `found GPU device 0: Vulkan0` enumeration lines name devices that
///   whisper.cpp may still decline to use, so they are ignored.
/// * `ggml_vulkan: Found 0 Vulkan devices` -> [`Accel::Cpu`]. Emitted by
///   the Vulkan backend's own init when the ICD loads but exposes no
///   usable device -- the "driver installed but broken" case.
pub fn classify_log_line(line: &str) -> Option<Accel> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("whisper_backend_init_gpu") {
        if lower.contains("no gpu found") {
            return Some(Accel::Cpu);
        }
        return classify_backend_name(&lower);
    }
    if lower.contains("found 0 vulkan device") {
        return Some(Accel::Cpu);
    }
    if lower.contains("use gpu") && use_gpu_is_disabled(&lower) {
        return Some(Accel::Cpu);
    }
    None
}

/// Pull the backend name out of a `... using <name> backend` line and map
/// it to a GPU variant. `lower` must already be lowercased.
fn classify_backend_name(lower: &str) -> Option<Accel> {
    let after_using = lower.split("using ").nth(1)?;
    let name = after_using.split(" backend").next()?;
    if name.contains("vulkan") {
        return Some(Accel::Vulkan);
    }
    if name.contains("cuda") {
        return Some(Accel::Cuda);
    }
    // A GPU backend we ship no variant for (metal / sycl / hip). Say
    // nothing rather than guess -- `unknown` is honest, `cpu` would be a
    // lie and `vulkan` would be a worse one.
    None
}

/// True for `use gpu    = 0` (and the `false` spelling some ggml builds
/// use). `lower` must already be lowercased.
fn use_gpu_is_disabled(lower: &str) -> bool {
    let Some(after) = lower.split("use gpu").nth(1) else {
        return false;
    };
    let Some(value) = after.split('=').nth(1) else {
        return false;
    };
    matches!(value.trim(), "0" | "false")
}

/// The accelerator we EXPECT from the resolved [`GpuPolicy`] plus the
/// compiled-in backend features.
///
/// Used only as the fallback for [`AccelObserver::resolved`] before the
/// first model load has produced an observation. `Off` (or a build with
/// no GPU backend) plans CPU; otherwise the single GPU backend we ship
/// is Vulkan.
pub fn planned_from_policy(policy: GpuPolicy) -> Accel {
    if !should_use_gpu(policy) {
        return Accel::Cpu;
    }
    if cfg!(feature = "whisper-rs-vulkan") {
        Accel::Vulkan
    } else {
        // Unreachable today (`should_use_gpu` is only true on a Vulkan
        // build) but kept explicit so adding a second GPU feature does
        // not silently label it `vulkan`.
        Accel::Unknown
    }
}

/// Process-wide record of the planned + observed accelerator.
///
/// Two independent slots rather than one so `resolved()` can distinguish
/// "whisper.cpp told us" from "we are guessing from config". Atomics (not
/// a `Mutex`) because the log callback runs on whichever thread whisper.cpp
/// is loading on and must never block or unwind.
///
/// Instantiable so unit tests exercise the merge rules without touching
/// (or racing on) [`global`].
#[derive(Debug, Default)]
pub struct AccelObserver {
    observed: AtomicU8,
    planned: AtomicU8,
}

impl AccelObserver {
    /// Fresh observer with nothing planned and nothing observed.
    pub const fn new() -> Self {
        Self {
            observed: AtomicU8::new(Accel::Unknown.as_u8()),
            planned: AtomicU8::new(Accel::Unknown.as_u8()),
        }
    }

    /// Feed one whisper.cpp / ggml log line. No-op for unrecognised lines.
    pub fn note_log_line(&self, line: &str) {
        if let Some(signal) = classify_log_line(line) {
            self.record(signal);
        }
    }

    /// Start a new load or inference pass with no previous observation.
    pub fn begin_model_load(&self, planned: Accel) {
        self.reset_observed();
        self.set_planned(planned);
    }

    /// Clear the OBSERVED slot only, leaving the plan intact. See
    /// [`Self::begin_model_load`] for why this is not test-only.
    pub fn reset_observed(&self) {
        self.observed
            .store(Accel::Unknown.as_u8(), Ordering::Relaxed);
    }

    /// Record an observed signal, keeping the highest-[`Accel::rank`] one
    /// seen so far. Lock-free CAS loop: the callback can fire from any
    /// thread whisper.cpp loads on.
    pub fn record(&self, signal: Accel) {
        let mut current = self.observed.load(Ordering::Relaxed);
        loop {
            if Accel::from_u8(current).rank() >= signal.rank() {
                return;
            }
            match self.observed.compare_exchange_weak(
                current,
                signal.as_u8(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Observed accelerator, or `None` if it has not been confirmed.
    pub fn observed(&self) -> Option<Accel> {
        match Accel::from_u8(self.observed.load(Ordering::Relaxed)) {
            Accel::Unknown => None,
            other => Some(other),
        }
    }

    /// Stamp the config-derived expectation (see [`planned_from_policy`]).
    pub fn set_planned(&self, accel: Accel) {
        self.planned.store(accel.as_u8(), Ordering::Relaxed);
    }

    /// The config-derived expectation, [`Accel::Unknown`] until stamped.
    pub fn planned(&self) -> Accel {
        Accel::from_u8(self.planned.load(Ordering::Relaxed))
    }

    /// Observation if we have one, else the plan. This is what the
    /// `stt_accel` field and the startup line report.
    pub fn resolved(&self) -> Accel {
        self.observed().unwrap_or_else(|| self.planned())
    }

    /// Clear both slots. Test-only seam: the [`global`] observer is
    /// process-wide state and a test that stamps it has to be able to
    /// hand the process back the way it found it.
    #[cfg(test)]
    pub(crate) fn reset(&self) {
        self.reset_observed();
        self.planned
            .store(Accel::Unknown.as_u8(), Ordering::Relaxed);
    }
}

/// The process-wide observer shared by Whisper provenance consumers.
pub fn global() -> &'static AccelObserver {
    static GLOBAL: AccelObserver = AccelObserver::new();
    &GLOBAL
}

/// Begin a session against [`global`]: clear the previous session's
/// observation and stamp the plan from the env GPU policy. Returns the
/// plan.
///
/// Called at session construction so the startup provenance line can name
/// an accelerator before whisper.cpp has loaded anything (the model loads
/// lazily on the first utterance). A malformed `VOICEPI_WHISPER_GPU` falls
/// back to the default policy rather than failing: the env parser's own
/// hard error is surfaced by the model loader, and refusing to print a
/// startup line because of a typo would remove the diagnostic exactly when
/// it is most wanted.
///
/// Clearing the observation matters because the new session's model is
/// NOT loaded yet: a second session built in the same process (a retried
/// in-process install, a policy flip from Vulkan to CPU) would otherwise
/// print the previous session's verdict as its own banner and keep it
/// until the first transcription triggered the lazy load.
pub fn begin_session_from_env() -> Accel {
    let policy = super::gpu::parse_gpu_policy_from_env().unwrap_or_default();
    begin_session(policy)
}

/// Start a session using a policy already resolved at the process boundary.
pub(crate) fn begin_session(policy: GpuPolicy) -> Accel {
    let planned = planned_from_policy(policy);
    global().begin_model_load(planned);
    planned
}

/// Wire label for the accelerator that served the current process's
/// whisper.cpp work -- `"unknown"` on a build/run where whisper.cpp never
/// loaded (stock builds, cloud STT).
pub fn resolved_label() -> &'static str {
    global().resolved().as_str()
}

#[cfg(test)]
#[path = "accel_tests.rs"]
mod tests;
