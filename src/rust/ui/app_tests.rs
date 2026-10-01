use super::app::injection_viewport_mouse_passthrough;
use super::tasks::{REINJECT_LAST_LABEL, RUN_BENCHMARK_LABEL};
use super::{
    test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK},
    AppSettings, HotkeyCaptureState, HotkeyVerificationSession, InstalledHotkeyStatus, WorkerEvent,
};
use crate::runtime::RuntimeEvent;
use eframe::egui;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

fn key_event(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

#[path = "app/view_tests.rs"]
mod view;

#[path = "app/model_policy_tests.rs"]
mod model_policy;

#[path = "app/runtime_lifecycle_tests.rs"]
mod runtime_lifecycle;

#[path = "app/polling_tests.rs"]
mod polling;

#[path = "app/worker_events_tests.rs"]
mod worker_events;
