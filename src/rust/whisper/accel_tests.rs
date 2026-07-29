//! Unit tests for [`super`] -- the whisper.cpp accelerator classifier.
//!
//! Every fixture line here is copied from the real whisper.cpp log
//! formats in `whisper.cpp/src/whisper.cpp` (`use gpu    = %d`,
//! `whisper_backend_init_gpu: no GPU found`,
//! `whisper_backend_init_gpu: using %s backend`) so a whisper.cpp bump
//! that reworded them fails here rather than silently degrading every
//! utterance record to `stt_accel=unknown`.

use super::*;

// -- wire labels ---------------------------------------------------------

#[test]
fn accel_labels_are_the_documented_lowercase_ascii_set() {
    // These strings land verbatim in history/metrics rows and in the
    // startup line; downstream tooling greps them.
    assert_eq!(Accel::Unknown.as_str(), "unknown");
    assert_eq!(Accel::Cpu.as_str(), "cpu");
    assert_eq!(Accel::Cuda.as_str(), "cuda");
    assert_eq!(Accel::Vulkan.as_str(), "vulkan");
    for accel in [Accel::Unknown, Accel::Cpu, Accel::Cuda, Accel::Vulkan] {
        assert!(
            accel.as_str().is_ascii(),
            "{accel:?} label must be ASCII (console guard)"
        );
    }
}

#[test]
fn default_accel_is_unknown() {
    assert_eq!(Accel::default(), Accel::Unknown);
}

// -- classifier: GPU verdicts -------------------------------------------

#[test]
fn using_vulkan_backend_line_classifies_as_vulkan() {
    assert_eq!(
        classify_log_line("whisper_backend_init_gpu: using Vulkan0 backend\n"),
        Some(Accel::Vulkan)
    );
}

#[test]
fn using_cuda_backend_line_classifies_as_cuda() {
    assert_eq!(
        classify_log_line("whisper_backend_init_gpu: using CUDA0 backend"),
        Some(Accel::Cuda)
    );
}

#[test]
fn no_gpu_found_line_classifies_as_cpu() {
    // THE regression this module exists for: a Vulkan-linked binary on a
    // box with no usable driver silently falls back to CPU. If this stops
    // being detected, `stt_accel` goes back to lying about GPU use.
    assert_eq!(
        classify_log_line("whisper_backend_init_gpu: no GPU found\n"),
        Some(Accel::Cpu)
    );
}

#[test]
fn use_gpu_zero_classifies_as_cpu() {
    assert_eq!(
        classify_log_line("whisper_init_from_file_with_params_no_state: use gpu    = 0\n"),
        Some(Accel::Cpu)
    );
}

#[test]
fn use_gpu_one_is_intent_not_outcome_and_yields_no_signal() {
    // `use gpu = 1` is what we ASKED for. Treating it as a verdict is the
    // exact class of bug (config mistaken for outcome) this module fixes.
    assert_eq!(
        classify_log_line("whisper_init_from_file_with_params_no_state: use gpu    = 1\n"),
        None
    );
}

#[test]
fn vulkan_device_enumeration_lines_are_not_a_gpu_verdict() {
    // whisper.cpp logs every candidate device BEFORE deciding. These name
    // "Vulkan0" but the very next line can be `no GPU found`.
    assert_eq!(
        classify_log_line("whisper_backend_init_gpu: device 0: Vulkan0 (type: 1)"),
        None
    );
    assert_eq!(
        classify_log_line(
            "whisper_backend_init_gpu: found GPU device 0: Vulkan0 (type: 1, cnt: 0)"
        ),
        None
    );
}

#[test]
fn zero_vulkan_devices_classifies_as_cpu() {
    // ICD loads but exposes nothing usable -- the "driver installed but
    // broken" case.
    assert_eq!(
        classify_log_line("ggml_vulkan: Found 0 Vulkan devices"),
        Some(Accel::Cpu)
    );
}

#[test]
fn unrelated_banner_lines_yield_no_signal() {
    for line in [
        "whisper_model_load: loading model",
        "whisper_model_load: n_vocab       = 51866",
        "whisper_backend_init: using BLAS backend",
        "system_info: n_threads = 4",
        "",
    ] {
        assert_eq!(
            classify_log_line(line),
            None,
            "unexpected signal for {line:?}"
        );
    }
}

#[test]
fn unknown_gpu_backend_name_stays_unclassified() {
    // Metal / SYCL / HIP are not backends we ship. Reporting `unknown` is
    // honest; reporting `vulkan` or `cpu` would not be.
    assert_eq!(
        classify_log_line("whisper_backend_init_gpu: using Metal backend"),
        None
    );
}

#[test]
fn classification_is_case_insensitive() {
    assert_eq!(
        classify_log_line("WHISPER_BACKEND_INIT_GPU: USING VULKAN0 BACKEND"),
        Some(Accel::Vulkan)
    );
}

// -- observer merge rules -----------------------------------------------

#[test]
fn observer_starts_with_nothing_observed() {
    let observer = AccelObserver::new();
    assert_eq!(observer.observed(), None);
    assert_eq!(observer.planned(), Accel::Unknown);
    assert_eq!(observer.resolved(), Accel::Unknown);
}

#[test]
fn gpu_verdict_wins_over_an_earlier_cpu_signal() {
    // Real load order on a working GPU box: `use gpu = 1` does nothing,
    // then Vulkan is chosen. Feed a CPU signal first to prove ordering
    // cannot produce a false `cpu`.
    let observer = AccelObserver::new();
    observer.note_log_line("ggml_vulkan: Found 0 Vulkan devices");
    assert_eq!(observer.observed(), Some(Accel::Cpu));
    observer.note_log_line("whisper_backend_init_gpu: using Vulkan0 backend");
    assert_eq!(observer.observed(), Some(Accel::Vulkan));
}

#[test]
fn cpu_signal_cannot_downgrade_an_established_gpu_verdict() {
    let observer = AccelObserver::new();
    observer.note_log_line("whisper_backend_init_gpu: using Vulkan0 backend");
    observer.note_log_line("whisper_init_from_file_with_params_no_state: use gpu    = 0");
    assert_eq!(observer.observed(), Some(Accel::Vulkan));
}

#[test]
fn unrecognised_lines_leave_the_observer_untouched() {
    let observer = AccelObserver::new();
    observer.note_log_line("whisper_model_load: loading model");
    assert_eq!(observer.observed(), None);
}

#[test]
fn resolved_prefers_observation_over_plan() {
    // The headline contract: a build that PLANNED vulkan but whose
    // whisper.cpp fell back to CPU must resolve to `cpu`.
    let observer = AccelObserver::new();
    observer.set_planned(Accel::Vulkan);
    assert_eq!(observer.resolved(), Accel::Vulkan, "plan is the fallback");
    observer.note_log_line("whisper_backend_init_gpu: no GPU found");
    assert_eq!(
        observer.resolved(),
        Accel::Cpu,
        "whisper.cpp's verdict must override the plan"
    );
}

#[test]
fn planned_from_policy_off_is_cpu() {
    assert_eq!(planned_from_policy(GpuPolicy::Off), Accel::Cpu);
}

#[test]
#[cfg(feature = "whisper-rs-vulkan")]
fn planned_from_policy_auto_is_vulkan_on_a_vulkan_build() {
    assert_eq!(planned_from_policy(GpuPolicy::Auto), Accel::Vulkan);
    assert_eq!(planned_from_policy(GpuPolicy::Vulkan), Accel::Vulkan);
}

#[test]
#[cfg(not(feature = "whisper-rs-vulkan"))]
fn planned_from_policy_auto_is_cpu_without_a_gpu_backend() {
    // Stock build: `VOICEPI_WHISPER_GPU=vulkan` cannot produce GPU, so the
    // plan must say `cpu` rather than advertising a backend that is not
    // linked in.
    assert_eq!(planned_from_policy(GpuPolicy::Auto), Accel::Cpu);
    assert_eq!(planned_from_policy(GpuPolicy::Vulkan), Accel::Cpu);
}

// -- global observer -----------------------------------------------------

#[test]
fn global_observer_is_a_single_shared_instance() {
    assert!(std::ptr::eq(global(), global()));
}

#[test]
fn resolved_label_reports_the_global_verdict() {
    // Serialised against every other test that reads or writes the global
    // observer (notably `whisper::protocol`'s response-envelope tests) via
    // the crate-wide lock; the reset afterwards hands the process back
    // unchanged.
    let _guard = crate::test_env_lock::ACCEL_OBSERVER_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    global().reset();
    assert_eq!(resolved_label(), "unknown");
    global().note_log_line("whisper_backend_init_gpu: using Vulkan0 backend");
    assert_eq!(resolved_label(), "vulkan");
    global().reset();
    assert_eq!(resolved_label(), "unknown");
}
