//! Tests for the badge's one-click update flow: the local-only gate, the
//! dispatch to the launchers, adopting a finished download, and the verified
//! write path the installer download uses. A recording fake launcher stands
//! in for process launches and the network.

use super::super::test_support::{test_app, EnvVarGuard};
use super::*;
use std::cell::RefCell;
use std::io::Cursor;
use std::sync::mpsc::Sender;

/// Records every launcher call; `fail` makes the launches return an error.
#[derive(Default)]
struct FakeLauncher {
    consoles: RefCell<Vec<ConsoleUpdate>>,
    downloads: RefCell<Vec<String>>,
    installers: RefCell<Vec<PathBuf>>,
    fail: Option<String>,
}

impl FakeLauncher {
    fn failing(message: &str) -> Self {
        Self {
            fail: Some(message.to_owned()),
            ..Self::default()
        }
    }

    fn launched_nothing(&self) -> bool {
        self.consoles.borrow().is_empty()
            && self.downloads.borrow().is_empty()
            && self.installers.borrow().is_empty()
    }

    fn result(&self) -> Result<(), String> {
        self.fail.clone().map_or(Ok(()), Err)
    }
}

impl UpdateLauncher for FakeLauncher {
    fn start_console(&self, update: &ConsoleUpdate) -> Result<(), String> {
        self.consoles.borrow_mut().push(update.clone());
        self.result()
    }

    fn start_download(&self, version: &str) -> InstallerDownload {
        self.downloads.borrow_mut().push(version.to_owned());
        // The sender is dropped, so a later poll reports a stopped worker.
        InstallerDownload::from_receiver(std::sync::mpsc::channel().1)
    }

    fn start_installer(&self, installer: &Path) -> Result<(), String> {
        self.installers.borrow_mut().push(installer.to_path_buf());
        self.result()
    }
}

fn choco_run() -> UpdateRun {
    UpdateRun::Console(ConsoleUpdate::Choco {
        version: "3.6.0".to_owned(),
        prerelease: false,
    })
}

fn installer_run() -> UpdateRun {
    UpdateRun::Installer {
        version: "3.6.0".to_owned(),
    }
}

/// An app with local-only off (setting AND environment) and `run` awaiting
/// confirmation.
fn confirming_app(run: UpdateRun) -> WhisperDictateApp {
    let mut app = test_app(AppSettings::default());
    app.update_install = UpdateInstallState::Confirming(run);
    app
}

fn downloading_app() -> (WhisperDictateApp, Sender<Result<PathBuf, String>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = test_app(AppSettings::default());
    app.update_install = UpdateInstallState::Downloading(InstallerDownload::from_receiver(rx));
    (app, tx)
}

fn failure_message(app: &WhisperDictateApp) -> Option<&str> {
    match &app.update_install {
        UpdateInstallState::Failed(message) => Some(message),
        _ => None,
    }
}

// ── local-only gate ─────────────────────────────────────────────────────────

#[test]
fn local_only_setting_blocks_every_update_run_before_it_starts() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    for run in [
        choco_run(),
        UpdateRun::Console(ConsoleUpdate::Winget),
        installer_run(),
    ] {
        let mut app = confirming_app(run.clone());
        app.settings.local_only = true;
        let launcher = FakeLauncher::default();

        app.confirm_update_install(&launcher);

        assert!(
            launcher.launched_nothing(),
            "{run:?} started under local-only"
        );
        assert_eq!(
            failure_message(&app),
            Some(ui_text("en", UiTextKey::UpdateBlockedLocalOnly))
        );
    }
}

#[test]
fn local_only_environment_override_blocks_the_installer_download() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::set("VOICEPI_LOCAL_ONLY", "1");
    let mut app = confirming_app(installer_run());
    assert!(!app.settings.local_only);
    let launcher = FakeLauncher::default();

    app.confirm_update_install(&launcher);

    assert!(launcher.launched_nothing());
    assert!(failure_message(&app).is_some());
}

#[test]
fn enabling_local_only_cancels_a_running_download() {
    let (mut app, _tx) = downloading_app();
    let UpdateInstallState::Downloading(download) = &app.update_install else {
        unreachable!()
    };
    let cancel = download.cancel.clone();

    assert!(app.cancel_update_download_for_local_only());

    assert!(cancel.load(Ordering::Acquire));
    assert!(failure_message(&app).is_some());
    // Nothing left to cancel.
    assert!(!app.cancel_update_download_for_local_only());
}

// ── dispatch ────────────────────────────────────────────────────────────────

#[test]
fn confirmed_console_update_starts_the_console() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let mut app = confirming_app(choco_run());
    let launcher = FakeLauncher::default();

    app.confirm_update_install(&launcher);

    let UpdateRun::Console(expected) = choco_run() else {
        unreachable!()
    };
    assert_eq!(*launcher.consoles.borrow(), [expected]);
    assert!(matches!(app.update_install, UpdateInstallState::Started));
}

#[test]
fn console_launch_error_is_shown() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let mut app = confirming_app(choco_run());

    app.confirm_update_install(&FakeLauncher::failing(
        "the administrator prompt was cancelled",
    ));

    assert_eq!(
        failure_message(&app),
        Some("the administrator prompt was cancelled")
    );
}

#[test]
fn confirmed_installer_update_starts_the_download_only() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let mut app = confirming_app(installer_run());
    let launcher = FakeLauncher::default();

    app.confirm_update_install(&launcher);

    assert_eq!(*launcher.downloads.borrow(), ["3.6.0"]);
    assert!(launcher.installers.borrow().is_empty());
    assert!(matches!(
        app.update_install,
        UpdateInstallState::Downloading(_)
    ));
}

#[test]
fn confirm_without_an_open_dialog_does_nothing() {
    let mut app = test_app(AppSettings::default());
    let launcher = FakeLauncher::default();

    app.confirm_update_install(&launcher);

    assert!(launcher.launched_nothing());
    assert!(matches!(app.update_install, UpdateInstallState::Idle));
}

#[test]
fn dismissing_the_dialog_runs_nothing() {
    let mut app = confirming_app(choco_run());
    app.dismiss_update_install();
    assert!(matches!(app.update_install, UpdateInstallState::Idle));
}

#[test]
fn a_click_during_a_download_does_not_reopen_the_dialog() {
    let (mut app, _tx) = downloading_app();
    let launcher = FakeLauncher::default();
    app.request_update_install(choco_run());
    app.confirm_update_install(&launcher);
    assert!(launcher.launched_nothing());
    assert!(matches!(
        app.update_install,
        UpdateInstallState::Downloading(_)
    ));
}

#[test]
fn a_click_after_an_updater_started_launches_nothing_more() {
    let _lock = crate::test_env_lock::lock_env();
    let _env = EnvVarGuard::remove("VOICEPI_LOCAL_ONLY");
    let mut app = confirming_app(choco_run());
    let launcher = FakeLauncher::default();
    app.confirm_update_install(&launcher);
    assert!(matches!(app.update_install, UpdateInstallState::Started));

    // A second click, then a confirm (as the dialog's button would do), must
    // not reopen the dialog or start a competing console.
    app.request_update_install(choco_run());
    assert!(matches!(app.update_install, UpdateInstallState::Started));
    app.confirm_update_install(&launcher);

    assert_eq!(launcher.consoles.borrow().len(), 1);
    assert!(matches!(app.update_install, UpdateInstallState::Started));
}

#[test]
fn a_click_after_a_failure_reopens_the_dialog() {
    let mut app = test_app(AppSettings::default());
    app.update_install = UpdateInstallState::Failed("cancelled".to_owned());
    app.request_update_install(installer_run());
    assert!(matches!(
        app.update_install,
        UpdateInstallState::Confirming(_)
    ));
}

// ── adopting the download ───────────────────────────────────────────────────

#[test]
fn verified_download_starts_the_installer() {
    let (mut app, tx) = downloading_app();
    let installer = PathBuf::from("whisper-dictate-windows-setup-3.6.0.exe");
    tx.send(Ok(installer.clone())).unwrap();
    let launcher = FakeLauncher::default();

    app.poll_update_install(&egui::Context::default(), &launcher);

    assert_eq!(*launcher.installers.borrow(), [installer]);
    assert!(matches!(app.update_install, UpdateInstallState::Started));
}

#[test]
fn failed_download_never_starts_an_installer() {
    let (mut app, tx) = downloading_app();
    tx.send(Err("sha256sums.txt has no entry for x".to_owned()))
        .unwrap();
    let launcher = FakeLauncher::default();

    app.poll_update_install(&egui::Context::default(), &launcher);

    assert!(launcher.launched_nothing());
    assert_eq!(
        failure_message(&app),
        Some("sha256sums.txt has no entry for x")
    );
}

#[test]
fn pending_download_stays_downloading() {
    let (mut app, _tx) = downloading_app();
    let launcher = FakeLauncher::default();

    app.poll_update_install(&egui::Context::default(), &launcher);

    assert!(launcher.launched_nothing());
    assert!(matches!(
        app.update_install,
        UpdateInstallState::Downloading(_)
    ));
}

#[test]
fn vanished_download_worker_is_reported() {
    let (mut app, tx) = downloading_app();
    drop(tx);

    app.poll_update_install(&egui::Context::default(), &FakeLauncher::default());

    assert_eq!(
        failure_message(&app),
        Some("the update download stopped unexpectedly")
    );
}

// ── verified write path ─────────────────────────────────────────────────────

fn write_verified(
    body: &[u8],
    expected_sha256: &str,
    cancel: bool,
) -> (Result<(), String>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut reader = CancellableReader {
        inner: Cursor::new(body.to_vec()),
        cancel: Arc::new(AtomicBool::new(cancel)),
    };
    let counters = Arc::new(DownloadCounters::default());
    let result = stream_download_to(
        &mut reader,
        Some(body.len() as u64),
        &dir.path().join("setup.exe.partial"),
        &dir.path().join("setup.exe"),
        expected_sha256,
        &counters,
    )
    .map_err(|err| err.to_string());
    (result, dir)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn checksum_mismatch_leaves_no_installer_behind() {
    let (result, dir) = write_verified(b"tampered installer", &sha256_hex(b"installer"), false);

    let err = result.unwrap_err();
    assert!(err.contains("SHA-256 mismatch"), "{err}");
    assert!(!dir.path().join("setup.exe").exists());
    assert!(!dir.path().join("setup.exe.partial").exists());
}

#[test]
fn matching_checksum_produces_the_installer() {
    let (result, dir) = write_verified(b"installer", &sha256_hex(b"installer"), false);

    assert_eq!(result, Ok(()));
    assert_eq!(
        std::fs::read(dir.path().join("setup.exe")).unwrap(),
        b"installer"
    );
}

#[test]
fn cancelled_download_leaves_no_installer_behind() {
    let (result, dir) = write_verified(b"installer", &sha256_hex(b"installer"), true);

    assert!(result.unwrap_err().contains("cancelled"));
    assert!(!dir.path().join("setup.exe").exists());
}

#[test]
fn progress_text_shows_percent_or_megabytes() {
    assert_eq!(download_progress_text(0, Some(200)), "0%");
    assert_eq!(download_progress_text(50, Some(200)), "25%");
    assert_eq!(download_progress_text(500, Some(200)), "100%");
    assert_eq!(download_progress_text(2_500_000, None), "2.5 MB");
    assert_eq!(download_progress_text(7, Some(0)), "0.0 MB");
}
