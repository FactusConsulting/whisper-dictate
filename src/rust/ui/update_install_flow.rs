//! The sidebar badge's one-click update flow on Windows: confirmation, the
//! local-only gate, the background installer download, and the hand-off to
//! the launchers in `update_install_launch`.
//!
//! PRIVACY: nothing here runs until the user clicks the badge AND confirms.
//! The installer download is two anonymous GETs of the release's assets on
//! github.com (`sha256sums.txt`, then the setup executable; GitHub redirects
//! them to its asset host); no data is sent. Chocolatey and winget fetch the
//! update themselves in their console. Local-only mode blocks every update
//! action before it starts.

use super::update_install::{
    expected_sha256, installer_asset_name, installer_download_dir, release_asset_url,
    ConsoleUpdate, UpdateRun, SHA256SUMS_ASSET,
};
use super::update_install_launch::{start_console_update, start_installer};
use super::*;
use crate::whisper::model_manager::{stream_download_to, DownloadProgress};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

const USER_AGENT: &str = concat!(
    "whisper-dictate-update/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/FactusConsulting/whisper-dictate)"
);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound for the whole installer transfer (~25 MB).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Repaint cadence while the download progress label is live.
const PROGRESS_REPAINT: Duration = Duration::from_millis(250);

/// Where the badge's update flow stands.
#[derive(Default)]
pub(in crate::ui) enum UpdateInstallState {
    #[default]
    Idle,
    /// The confirmation dialog is open for this update.
    Confirming(UpdateRun),
    /// The installer is downloading in the background.
    Downloading(InstallerDownload),
    /// The console or setup wizard is running; it closes this app.
    Started,
    /// The update could not start; the message is shown under the badge.
    Failed(String),
}

/// Byte counters the download worker updates and the badge reads.
#[derive(Debug, Default)]
struct DownloadCounters {
    downloaded: AtomicU64,
    /// `Content-Length`, or 0 when the server sent none.
    total: AtomicU64,
}

impl DownloadProgress for Arc<DownloadCounters> {
    fn on_progress(&self, downloaded: u64, total: Option<u64>) {
        self.downloaded.store(downloaded, Ordering::Relaxed);
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
    }
}

/// Handle to one background installer download.
pub(in crate::ui) struct InstallerDownload {
    counters: Arc<DownloadCounters>,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<PathBuf, String>>,
}

impl InstallerDownload {
    /// Download and verify the setup executable for `version` on a worker
    /// thread.
    pub(in crate::ui) fn spawn(version: String) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let download = Self::from_receiver(rx);
        let counters = download.counters.clone();
        let cancel = download.cancel.clone();
        std::thread::spawn(move || {
            let _ = tx.send(download_installer(&version, &counters, &cancel));
        });
        download
    }

    /// A handle whose result arrives on `result`.
    pub(in crate::ui) fn from_receiver(result: Receiver<Result<PathBuf, String>>) -> Self {
        Self {
            counters: Arc::default(),
            cancel: Arc::default(),
            result,
        }
    }

    /// `(downloaded bytes, total bytes when known)`.
    pub(in crate::ui) fn progress(&self) -> (u64, Option<u64>) {
        let total = self.counters.total.load(Ordering::Relaxed);
        (
            self.counters.downloaded.load(Ordering::Relaxed),
            (total > 0).then_some(total),
        )
    }

    /// The verified installer path once the worker finished.
    fn try_result(&self) -> Option<Result<PathBuf, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("the update download stopped unexpectedly".to_owned()))
            }
        }
    }

    /// Stop the transfer at the next read; the worker then removes its
    /// partial file and reports an error.
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// Short progress text for the badge: a percentage, or megabytes when the
/// size is unknown.
pub(in crate::ui) fn download_progress_text(downloaded: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => format!("{}%", downloaded.min(total) * 100 / total),
        _ => format!("{:.1} MB", downloaded as f64 / 1_000_000.0),
    }
}

/// Fails reads once `cancel` is set.
struct CancellableReader<R> {
    inner: R,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for CancellableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::other("update download cancelled"));
        }
        self.inner.read(buf)
    }
}

/// Fetch `sha256sums.txt` and the setup executable for `version`, verify the
/// executable's SHA-256 against its entry, and return its path in a
/// per-version temp directory. A missing or mismatching checksum leaves no
/// installer behind.
fn download_installer(
    version: &str,
    counters: &Arc<DownloadCounters>,
    cancel: &Arc<AtomicBool>,
) -> Result<PathBuf, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .into();
    let sums = agent
        .get(&release_asset_url(version, SHA256SUMS_ASSET))
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|err| format!("could not download {SHA256SUMS_ASSET}: {err}"))?
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("could not read {SHA256SUMS_ASSET}: {err}"))?;
    let asset = installer_asset_name(version);
    let expected = expected_sha256(&sums, &asset).map_err(|err| err.to_string())?;

    let dir = installer_download_dir(&std::env::temp_dir(), version);
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    let target = dir.join(&asset);
    let partial = dir.join(format!("{asset}.partial"));
    // A previous attempt's file is replaced by this verified download.
    let _ = std::fs::remove_file(&target);

    let response = agent
        .get(&release_asset_url(version, &asset))
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|err| format!("could not download {asset}: {err}"))?;
    let total = response
        .headers()
        .get("Content-Length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let (_, body) = response.into_parts();
    let mut reader = CancellableReader {
        inner: body.into_reader(),
        cancel: cancel.clone(),
    };
    stream_download_to(&mut reader, total, &partial, &target, &expected, counters)
        .map_err(|err| format!("{asset}: {err}"))?;
    Ok(target)
}

/// The side effects of an update run, injectable so the flow is testable
/// without launching processes or touching the network.
pub(in crate::ui) trait UpdateLauncher {
    fn start_console(&self, update: &ConsoleUpdate) -> Result<(), String>;
    fn start_download(&self, version: &str) -> InstallerDownload;
    fn start_installer(&self, installer: &Path) -> Result<(), String>;
}

/// The real launcher.
pub(in crate::ui) struct SystemUpdateLauncher;

impl UpdateLauncher for SystemUpdateLauncher {
    fn start_console(&self, update: &ConsoleUpdate) -> Result<(), String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("could not locate the running app: {err}"))?;
        start_console_update(update, &exe)
    }

    fn start_download(&self, version: &str) -> InstallerDownload {
        InstallerDownload::spawn(version.to_owned())
    }

    fn start_installer(&self, installer: &Path) -> Result<(), String> {
        start_installer(installer)
    }
}

impl WhisperDictateApp {
    /// Open the confirmation dialog for `run`.
    pub(in crate::ui) fn request_update_install(&mut self, run: UpdateRun) {
        if !matches!(self.update_install, UpdateInstallState::Downloading(_)) {
            self.update_install = UpdateInstallState::Confirming(run);
        }
    }

    /// Close the confirmation dialog without running anything.
    pub(in crate::ui) fn dismiss_update_install(&mut self) {
        if matches!(self.update_install, UpdateInstallState::Confirming(_)) {
            self.update_install = UpdateInstallState::Idle;
        }
    }

    /// Run the confirmed update, unless local-only mode forbids network
    /// access.
    pub(in crate::ui) fn confirm_update_install(&mut self, launcher: &dyn UpdateLauncher) {
        let UpdateInstallState::Confirming(run) = std::mem::take(&mut self.update_install) else {
            return;
        };
        if self.settings.local_only || self.local_only_downloads_blocked() {
            let message = ui_text(
                &self.settings.ui_language,
                UiTextKey::UpdateBlockedLocalOnly,
            );
            self.append_runtime_log("[ui] update: blocked by local-only mode");
            self.update_install = UpdateInstallState::Failed(message.to_owned());
            return;
        }
        self.update_install = match run {
            UpdateRun::Console(update) => {
                self.append_runtime_log(format!("[ui] update: starting {}", update.command_line()));
                match launcher.start_console(&update) {
                    Ok(()) => UpdateInstallState::Started,
                    Err(err) => self.update_install_failed(err),
                }
            }
            UpdateRun::Installer { version } => {
                self.append_runtime_log(format!(
                    "[ui] update: downloading {}",
                    installer_asset_name(&version)
                ));
                UpdateInstallState::Downloading(launcher.start_download(&version))
            }
        };
    }

    /// Adopt a finished installer download: start the verified installer, or
    /// show the error. Keeps frames coming while the progress label is live.
    pub(in crate::ui) fn poll_update_install(
        &mut self,
        ctx: &egui::Context,
        launcher: &dyn UpdateLauncher,
    ) {
        let UpdateInstallState::Downloading(download) = &self.update_install else {
            return;
        };
        let Some(result) = download.try_result() else {
            ctx.request_repaint_after(PROGRESS_REPAINT);
            return;
        };
        self.update_install = match result.and_then(|path| {
            self.append_runtime_log(format!(
                "[ui] update: checksum verified, starting {}",
                path.display()
            ));
            launcher.start_installer(&path)
        }) {
            Ok(()) => UpdateInstallState::Started,
            Err(err) => self.update_install_failed(err),
        };
    }

    /// Cancel a running installer download because local-only mode was just
    /// enabled. Returns whether one was running.
    pub(in crate::ui) fn cancel_update_download_for_local_only(&mut self) -> bool {
        let UpdateInstallState::Downloading(download) = &self.update_install else {
            return false;
        };
        download.cancel();
        let message = ui_text(
            &self.settings.ui_language,
            UiTextKey::UpdateBlockedLocalOnly,
        );
        self.update_install = UpdateInstallState::Failed(message.to_owned());
        true
    }

    fn update_install_failed(&mut self, err: String) -> UpdateInstallState {
        self.append_runtime_log(format!("[ui] update failed: {err}"));
        UpdateInstallState::Failed(err)
    }
}

#[cfg(test)]
#[path = "update_install_flow_tests.rs"]
mod tests;
