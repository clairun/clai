//! App update commands and startup update checks.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};

use crate::AppState;

pub const APP_UPDATE_AVAILABLE_EVENT: &str = "app-updates://available";

const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(4);
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Release manifest used for notify-only version checks on builds that
/// cannot self-install (e.g. Flatpak). Keep in sync with the updater
/// endpoint in `tauri.conf.json`.
const LATEST_MANIFEST_URL: &str =
    "https://github.com/clairun/clai/releases/latest/download/latest.json";

/// Update package downloaded in the background, waiting for the user to
/// restart. Kept in memory: packages are tens of MB and the alternative
/// (a temp file) would need cleanup and re-verification on install.
///
/// `installer` is the updater handle that produced `bytes`, kept alongside
/// them. It is what makes "Restart to install" a restart rather than a
/// download: applying the package needs no network, so the click cannot be
/// derailed by an offline machine, by a slow manifest fetch, or by a newer
/// release appearing between the download and the click.
struct DownloadedPackage<I> {
    version: String,
    installer: I,
    bytes: Vec<u8>,
}

/// The one downloaded package we hold, if any. A newer download replaces an
/// older one: the badge only ever advertises a single version.
struct PackageCache<I> {
    package: Option<DownloadedPackage<I>>,
}

impl<I> Default for PackageCache<I> {
    fn default() -> Self {
        Self { package: None }
    }
}

/// In-memory update state for the running app.
///
/// Generic over the installer type purely as a test seam: production always
/// uses `Update`, which cannot be constructed outside the updater plugin, so
/// tests instantiate `AppUpdateRuntime<()>` to exercise the bookkeeping.
#[derive(Clone)]
pub struct AppUpdateRuntime<I = Update> {
    last_check: Arc<Mutex<Option<AppUpdateLastCheck>>>,
    downloaded: Arc<Mutex<PackageCache<I>>>,
    check_lock: Arc<tokio::sync::Mutex<()>>,
    install_lock: Arc<tokio::sync::Mutex<()>>,
}

impl<I> Default for AppUpdateRuntime<I> {
    fn default() -> Self {
        Self {
            last_check: Arc::default(),
            downloaded: Arc::default(),
            check_lock: Arc::default(),
            install_lock: Arc::default(),
        }
    }
}

impl<I> AppUpdateRuntime<I> {
    pub fn new() -> Self {
        Self::default()
    }

    fn last_check(&self) -> Option<AppUpdateLastCheck> {
        self.last_check
            .lock()
            .expect("app update state poisoned")
            .clone()
    }

    /// Records the latest check result. Re-derives the `downloaded` flag
    /// from the byte cache at record time so a background download finishing
    /// between a check's cache read and its recording cannot regress a
    /// `downloaded: true` state back to `false`.
    fn record_check(&self, mut check: AppUpdateLastCheck) -> AppUpdateLastCheck {
        let downloaded_version = self.downloaded_version();
        if let Some(update) = check.update.as_mut() {
            if downloaded_version.as_deref() == Some(update.version.as_str()) {
                update.downloaded = true;
            }
        }
        *self.last_check.lock().expect("app update state poisoned") = Some(check.clone());
        check
    }

    fn downloaded_version(&self) -> Option<String> {
        self.downloaded
            .lock()
            .expect("app update state poisoned")
            .package
            .as_ref()
            .map(|package| package.version.clone())
    }

    fn store_downloaded(&self, version: &str, installer: I, bytes: Vec<u8>) {
        self.downloaded
            .lock()
            .expect("app update state poisoned")
            .package = Some(DownloadedPackage {
            version: version.to_string(),
            installer,
            bytes,
        });
    }

    /// Takes the downloaded package, whatever version it is. Installing
    /// exactly what was downloaded is the point: it is the version the badge
    /// offered to install, and it needs no network to apply. A newer release
    /// is picked up by the next check, which replaces the cache.
    fn take_downloaded(&self) -> Option<DownloadedPackage<I>> {
        self.downloaded
            .lock()
            .expect("app update state poisoned")
            .package
            .take()
    }

    /// Flags the recorded last check's update as downloaded (if it is still
    /// the same version) and returns the refreshed info for re-emission.
    fn mark_downloaded(&self, version: &str) -> Option<AppUpdateInfo> {
        let mut guard = self.last_check.lock().expect("app update state poisoned");
        let update = guard.as_mut()?.update.as_mut()?;
        if update.version != version {
            return None;
        }
        update.downloaded = true;
        Some(update.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateSupportStatus {
    /// The build can download and install updates itself.
    pub supported: bool,
    /// The build can at least check for newer releases (a superset of
    /// `supported`: notify-only channels like Flatpak can check but not
    /// install).
    pub can_check: bool,
    pub platform: String,
    pub arch: String,
    pub channel: String,
    pub bundle_type: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateInfo {
    pub current_version: String,
    pub version: String,
    pub date: Option<String>,
    pub body: Option<String>,
    /// False for notify-only channels: the user must update through their
    /// package channel (e.g. download the new Flatpak bundle) instead of the
    /// in-app installer.
    pub installable: bool,
    /// The update package has already been downloaded in the background;
    /// installing it only needs a restart, not a download.
    pub downloaded: bool,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateLastCheck {
    pub checked_at: String,
    pub update: Option<AppUpdateInfo>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateStatus {
    pub support: AppUpdateSupportStatus,
    pub last_check: Option<AppUpdateLastCheck>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateCheckResult {
    pub support: AppUpdateSupportStatus,
    pub last_check: AppUpdateLastCheck,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateAvailableEvent {
    pub update: AppUpdateInfo,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum AppUpdateInstallEvent {
    Started,
    Progress { downloaded: u64, total: Option<u64> },
    DownloadFinished,
    Installing,
}

/// Subset of the updater `latest.json` manifest used for notify-only checks.
#[derive(Debug, Deserialize)]
struct UpdateManifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct SupportProbe<'a> {
    debug_assertions: bool,
    flatpak: bool,
    target_os: &'a str,
    arch: &'a str,
    bundle_type: Option<&'a str>,
    os_release: Option<&'a str>,
    has_dpkg: bool,
    has_rpm: bool,
}

/// Whether an available update should be downloaded in the background.
///
/// Mandatory by design — no user setting gates this — but only where the
/// build can install what it downloads: notify-only channels (Flatpak, Linux
/// deb/rpm) would burn bandwidth on a package they can never apply.
fn should_download_in_background(support: &AppUpdateSupportStatus, update: &AppUpdateInfo) -> bool {
    support.supported && update.installable && !update.downloaded
}

#[tauri::command]
pub fn get_app_update_status(state: State<'_, AppState>) -> AppUpdateStatus {
    AppUpdateStatus {
        support: detect_support_status(),
        last_check: state.app_updates.last_check(),
    }
}

#[tauri::command]
pub async fn check_for_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppUpdateCheckResult, String> {
    let result = check_for_update(&app, state.inner()).await;
    // Manual checks surface updates the same way startup checks do, so the
    // persistent top-bar badge appears no matter who found the update first.
    if let Some(update) = result.last_check.update.clone() {
        if let Err(error) = app.emit(
            APP_UPDATE_AVAILABLE_EVENT,
            AppUpdateAvailableEvent {
                update: update.clone(),
            },
        ) {
            tracing::warn!(%error, "Failed to emit app update notification");
        }
        if should_download_in_background(&result.support, &update) {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                download_update_in_background(&app).await;
            });
        }
    }
    Ok(result)
}

#[tauri::command]
pub async fn install_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<AppUpdateInstallEvent>,
) -> Result<(), String> {
    let _install_guard = state.app_updates.install_lock.lock().await;
    let support = detect_support_status();
    if !support.supported {
        return Err(support
            .reason
            .unwrap_or_else(|| "This CLAI build cannot update itself.".to_string()));
    }

    let _ = on_event.send(AppUpdateInstallEvent::Started);
    // Fast path: the background download already fetched this package and the
    // updater plugin verified its signature, and the handle it came from is
    // cached with it. So there is nothing left to fetch — no manifest check,
    // no network at all — which is what lets the badge promise a restart
    // rather than a wait, offline included.
    let package = match state.app_updates.take_downloaded() {
        Some(package) => {
            let _ = on_event.send(AppUpdateInstallEvent::DownloadFinished);
            package
        }
        // Nothing cached: the background download is still running, failed,
        // or never started (Settings > About can ask for an install the
        // moment a check reports one). Fetch it now, reporting progress.
        None => {
            let update = app
                .updater_builder()
                .timeout(INSTALL_TIMEOUT)
                .build()
                .map_err(format_updater_error)?
                .check()
                .await
                .map_err(format_updater_error)?
                .ok_or_else(|| "No update is available.".to_string())?;
            let mut downloaded: u64 = 0;
            let bytes = tokio::time::timeout(
                DOWNLOAD_TIMEOUT,
                update.download(
                    |chunk_len, total| {
                        downloaded = downloaded.saturating_add(chunk_len as u64);
                        let _ =
                            on_event.send(AppUpdateInstallEvent::Progress { downloaded, total });
                    },
                    || {
                        let _ = on_event.send(AppUpdateInstallEvent::DownloadFinished);
                    },
                ),
            )
            .await
            .map_err(|_| "Timed out downloading the update package.".to_string())?
            .map_err(format_updater_error)?;
            DownloadedPackage {
                version: update.version.clone(),
                installer: update,
                bytes,
            }
        }
    };

    let _ = on_event.send(AppUpdateInstallEvent::Installing);
    if let Err(error) = package.installer.install(&package.bytes) {
        // Keep the verified package: the retry is then a restart instead of
        // another download, and the badge's "downloaded" state stays true.
        cache_downloaded_package(&app, state.inner(), package);
        return Err(format_updater_error(error));
    }
    app.restart();
}

pub fn spawn_startup_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_CHECK_DELAY).await;
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };

        // Checking is always on: it is a cheap, anonymous manifest fetch and
        // the user must at least learn an update exists.
        let result = check_for_update(&app, state.inner()).await;
        let Some(update) = result.last_check.update else {
            return;
        };
        if let Err(error) = app.emit(
            APP_UPDATE_AVAILABLE_EVENT,
            AppUpdateAvailableEvent {
                update: update.clone(),
            },
        ) {
            tracing::warn!(%error, "Failed to emit app update notification");
        }
        if should_download_in_background(&result.support, &update) {
            download_update_in_background(&app).await;
        }
    });
}

/// Background download of an available update. Unconditional on builds that
/// can install updates themselves: having the package ready is what lets the
/// UI offer a one-click "Restart to install" instead of a download wait, and
/// it costs the user nothing to decide later (or never). Installing is still
/// entirely the user's call — nothing here restarts the app.
///
/// Re-checks the updater manifest to get a fresh signed package descriptor,
/// downloads it, caches the bytes, and re-emits the availability event with
/// `downloaded: true` so the badge grows its "Restart to install" action.
async fn download_update_in_background(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    // Serialize with manual installs; whichever runs first downloads.
    let _install_guard = state.app_updates.install_lock.lock().await;

    let update = match app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .map_err(format_updater_error)
    {
        Ok(updater) => match updater.check().await {
            Ok(Some(update)) => update,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(error = %format_updater_error(error), "Update auto-download check failed");
                return;
            }
        },
        Err(error) => {
            tracing::warn!(error, "Update auto-download check failed");
            return;
        }
    };
    if state.app_updates.downloaded_version().as_deref() == Some(update.version.as_str()) {
        return;
    }

    let bytes =
        match tokio::time::timeout(DOWNLOAD_TIMEOUT, update.download(|_, _| {}, || {})).await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(error)) => {
                tracing::warn!(error = %format_updater_error(error), "Update auto-download failed");
                return;
            }
            Err(_) => {
                tracing::warn!("Update auto-download timed out");
                return;
            }
        };

    tracing::info!(version = %update.version, "Update downloaded in the background");
    cache_downloaded_package(
        app,
        state.inner(),
        DownloadedPackage {
            version: update.version.clone(),
            installer: update,
            bytes,
        },
    );
}

/// Holds a verified package and tells the UI it can offer "Restart to
/// install". Shared by the background download and by a failed install
/// putting its package back, so both leave the same state behind.
fn cache_downloaded_package(app: &AppHandle, state: &AppState, package: DownloadedPackage<Update>) {
    let version = package.version.clone();
    state
        .app_updates
        .store_downloaded(&version, package.installer, package.bytes);
    // Only emits when the recorded check still names this version; a check
    // that has already moved on owns the badge's state instead.
    if let Some(update) = state.app_updates.mark_downloaded(&version) {
        if let Err(error) = app.emit(
            APP_UPDATE_AVAILABLE_EVENT,
            AppUpdateAvailableEvent { update },
        ) {
            tracing::warn!(%error, "Failed to emit app update notification");
        }
    }
}

async fn check_for_update(app: &AppHandle, state: &AppState) -> AppUpdateCheckResult {
    let _check_guard = state.app_updates.check_lock.lock().await;
    let support = detect_support_status();
    let checked_at = chrono::Utc::now().to_rfc3339();

    let last_check = if support.supported {
        match app
            .updater_builder()
            .timeout(CHECK_TIMEOUT)
            .build()
            .map_err(format_updater_error)
        {
            Ok(updater) => match updater.check().await.map_err(format_updater_error) {
                Ok(update) => AppUpdateLastCheck {
                    checked_at,
                    update: update
                        .as_ref()
                        .map(|update| update_info(update, state.app_updates.downloaded_version())),
                    error: None,
                },
                Err(error) => AppUpdateLastCheck {
                    checked_at,
                    update: None,
                    error: Some(error),
                },
            },
            Err(error) => AppUpdateLastCheck {
                checked_at,
                update: None,
                error: Some(error),
            },
        }
    } else if support.can_check {
        // Notify-only channels (Flatpak / linux_pkg): the build cannot
        // install updates itself, but we still tell the user a newer
        // release exists so they can fetch it through their package channel.
        let current_version = app.package_info().version.to_string();
        match fetch_update_manifest().await {
            Ok(manifest) => AppUpdateLastCheck {
                checked_at,
                update: notify_update_from_manifest(&current_version, &manifest),
                error: None,
            },
            Err(error) => AppUpdateLastCheck {
                checked_at,
                update: None,
                error: Some(error),
            },
        }
    } else {
        AppUpdateLastCheck {
            checked_at,
            update: None,
            error: support.reason.clone(),
        }
    };

    let last_check = state.app_updates.record_check(last_check);
    AppUpdateCheckResult {
        support,
        last_check,
    }
}

fn update_info(update: &Update, downloaded_version: Option<String>) -> AppUpdateInfo {
    AppUpdateInfo {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date: update.date.as_ref().map(ToString::to_string),
        body: update.body.clone(),
        installable: true,
        downloaded: downloaded_version.as_deref() == Some(update.version.as_str()),
    }
}

async fn fetch_update_manifest() -> Result<UpdateManifest, String> {
    let response = reqwest::Client::new()
        .get(LATEST_MANIFEST_URL)
        .timeout(CHECK_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch the release manifest: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Release manifest request failed with status {}.",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|e| format!("Failed to parse the release manifest: {e}"))
}

fn notify_update_from_manifest(
    current_version: &str,
    manifest: &UpdateManifest,
) -> Option<AppUpdateInfo> {
    if !version_is_newer(&manifest.version, current_version) {
        return None;
    }
    Some(AppUpdateInfo {
        current_version: current_version.to_string(),
        version: manifest.version.clone(),
        date: manifest.pub_date.clone(),
        body: manifest
            .notes
            .as_deref()
            .map(str::trim)
            .filter(|notes| !notes.is_empty())
            .map(str::to_string),
        installable: false,
        downloaded: false,
    })
}

/// Numeric dot-component comparison for CalVer strings like `26.7.12`,
/// tolerant of a leading `v` and of `-`/`+` suffixes (`26.7.12-38-gabc`
/// compares as `26.7.12`). Unparseable versions never report an update.
fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version_parts(candidate), parse_version_parts(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn parse_version_parts(value: &str) -> Option<Vec<u64>> {
    let core = value.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    if core.is_empty() {
        return None;
    }
    core.split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

fn detect_support_status() -> AppUpdateSupportStatus {
    let bundle = tauri::utils::platform::bundle_type().map(|bundle| bundle.to_string());
    let os_release = read_os_release();
    support_from_probe(SupportProbe {
        debug_assertions: cfg!(debug_assertions),
        flatpak: crate::providers::is_flatpak(),
        target_os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        bundle_type: bundle.as_deref(),
        os_release: os_release.as_deref(),
        has_dpkg: crate::providers::command_exists("dpkg"),
        has_rpm: crate::providers::command_exists("rpm"),
    })
}

fn support_from_probe(probe: SupportProbe<'_>) -> AppUpdateSupportStatus {
    if probe.debug_assertions {
        return unsupported(
            probe,
            "development",
            "Development builds do not use installer updates.",
        );
    }
    if probe.flatpak {
        return unsupported(
            probe,
            "flatpak",
            "This Flatpak build updates through \"flatpak update\", not through CLAI itself.",
        );
    }

    match probe.target_os {
        "windows" => match probe.bundle_type {
            Some("msi" | "nsis") => supported(probe, "native"),
            Some(_) => unsupported(
                probe,
                "native",
                "This Windows build was not installed with an updater-capable installer.",
            ),
            None => unsupported(
                probe,
                "development",
                "This Windows build does not expose its installer type.",
            ),
        },
        "macos" => match probe.bundle_type {
            Some("app" | "dmg") => supported(probe, "native"),
            Some(_) => unsupported(
                probe,
                "native",
                "This macOS build was not installed with an updater-capable bundle.",
            ),
            None => unsupported(
                probe,
                "development",
                "This macOS build does not expose its installer type.",
            ),
        },
        "linux" => linux_support_from_probe(probe),
        _ => unsupported(
            probe,
            "unsupported",
            "This operating system is not supported.",
        ),
    }
}

fn linux_support_from_probe(probe: SupportProbe<'_>) -> AppUpdateSupportStatus {
    // Linux deb/rpm bundles are always demoted to notify-only because
    // self-updating would race with the package manager. AppImage and any
    // future bundle type are not supported by CLAI self-updates today
    // (see tauri.conf.json bundle targets), so a non-deb/rpm bundle is
    // treated as silent — we have no installer to point the user at.
    match probe.bundle_type {
        Some(bundle @ "deb") | Some(bundle @ "rpm") => {
            let (has_tooling, on_family) = match bundle {
                "deb" => (
                    probe.has_dpkg,
                    os_release_matches(probe.os_release, DEB_FAMILIES),
                ),
                "rpm" => (
                    probe.has_rpm,
                    os_release_matches(probe.os_release, RPM_FAMILIES),
                ),
                _ => unreachable!("outer or-pattern restricts to deb|rpm; got {:?}", bundle),
            };
            if has_tooling && on_family {
                unsupported(
                    probe,
                    "linux_pkg",
                    "This Linux install should be updated by its package manager.",
                )
            } else if on_family {
                // Right distro, but the package-manager binary is missing.
                unsupported(
                    probe,
                    "package_manager",
                    "This Linux install is missing its package-manager tooling.",
                )
            } else {
                // Wrong distro family for this bundle type (e.g. deb on
                // Arch, rpm on Ubuntu) — no package manager CLAI can name.
                unsupported(
                    probe,
                    "package_manager",
                    "This Linux install does not match a known package-manager distro.",
                )
            }
        }
        Some(_) | None => unsupported(
            probe,
            "package_manager",
            "This Linux install is not managed by a CLAI self-updater bundle.",
        ),
    }
}

fn supported(probe: SupportProbe<'_>, channel: &str) -> AppUpdateSupportStatus {
    AppUpdateSupportStatus {
        supported: true,
        can_check: true,
        platform: probe.target_os.to_string(),
        arch: probe.arch.to_string(),
        channel: channel.to_string(),
        bundle_type: probe.bundle_type.map(str::to_string),
        reason: None,
    }
}

fn unsupported(probe: SupportProbe<'_>, channel: &str, reason: &str) -> AppUpdateSupportStatus {
    AppUpdateSupportStatus {
        supported: false,
        // Flatpak bundles are side-loaded (no origin remote) and Linux
        // deb/rpm installs are steward-managed by the package manager:
        // both can still check for newer versions and surface a "vX is
        // available" badge, but neither can install in place. Other
        // package-manager installs (e.g. AUR) stay silent because nothing
        // in CLAI can act on the update.
        can_check: matches!(channel, "flatpak" | "linux_pkg"),
        platform: probe.target_os.to_string(),
        arch: probe.arch.to_string(),
        channel: channel.to_string(),
        bundle_type: probe.bundle_type.map(str::to_string),
        reason: Some(reason.to_string()),
    }
}

const DEB_FAMILIES: &[&str] = &["debian", "ubuntu", "linuxmint", "pop", "elementary"];
const RPM_FAMILIES: &[&str] = &["fedora", "rhel", "centos", "suse", "opensuse"];

fn read_os_release() -> Option<String> {
    fs::read_to_string("/etc/os-release").ok()
}

fn os_release_matches(contents: Option<&str>, families: &[&str]) -> bool {
    let Some(contents) = contents else {
        return false;
    };
    os_release_ids(contents)
        .iter()
        .any(|id| families.iter().any(|family| id == family))
}

fn os_release_ids(contents: &str) -> Vec<String> {
    let mut values = Vec::new();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key != "ID" && key != "ID_LIKE" {
            continue;
        }
        let trimmed = value.trim().trim_matches('"').trim_matches('\'');
        values.extend(
            trimmed
                .split_whitespace()
                .map(|value| value.to_ascii_lowercase()),
        );
    }
    values
}

fn format_updater_error(error: UpdaterError) -> String {
    match error {
        UpdaterError::TargetNotFound(target) => {
            format!("No updater release is available for this build target ({target}).")
        }
        UpdaterError::TargetsNotFound(targets) => {
            format!(
                "No updater release is available for this build target (tried {}).",
                targets.join(", ")
            )
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe<'a>(target_os: &'a str, bundle_type: Option<&'a str>) -> SupportProbe<'a> {
        SupportProbe {
            debug_assertions: false,
            flatpak: false,
            target_os,
            arch: "x86_64",
            bundle_type,
            os_release: None,
            has_dpkg: false,
            has_rpm: false,
        }
    }

    #[test]
    fn flatpak_is_notify_only_even_with_native_bundle_type() {
        let mut probe = probe("linux", Some("deb"));
        probe.flatpak = true;

        let status = support_from_probe(probe);

        assert!(!status.supported);
        assert!(status.can_check);
        assert_eq!(status.channel, "flatpak");
    }

    #[test]
    fn deb_on_debian_family_is_notify_only_under_linux_pkg_channel() {
        // Native deb-on-debian-family installs cannot self-update — the
        // package manager owns the binary. We still surface "vX is available"
        // through the badge, so the build is notify-only with can_check: true.
        let mut probe = probe("linux", Some("deb"));
        probe.has_dpkg = true;
        probe.os_release = Some("ID=ubuntu\nID_LIKE=debian\n");

        let status = support_from_probe(probe);
        assert!(!status.supported);
        assert!(status.can_check);
        assert_eq!(status.channel, "linux_pkg");
        assert_eq!(status.bundle_type.as_deref(), Some("deb"));
        assert_eq!(status.platform, "linux");
    }

    #[test]
    fn deb_on_non_debian_family_stays_silent() {
        // A deb installed on, say, Arch has no package manager CLAI can
        // point the user at — staying silent avoids misleading guidance.
        let mut probe = probe("linux", Some("deb"));
        probe.has_dpkg = true;
        probe.os_release = Some("ID=arch\n");

        let status = support_from_probe(probe);
        assert!(!status.supported);
        assert!(!status.can_check);
        assert_eq!(status.channel, "package_manager");
        // The exact phrasing is what the toast would surface if it ever
        // showed on Linux; pin it so a string tweak here is a deliberate
        // copy change, not an accidental one.
        assert!(
            status
                .reason
                .as_deref()
                .unwrap()
                .contains("does not match a known package-manager distro"),
            "reason drifted: {:?}",
            status.reason,
        );
    }

    #[test]
    fn rpm_on_rpm_family_is_notify_only_under_linux_pkg_channel() {
        let mut probe = probe("linux", Some("rpm"));
        probe.has_rpm = true;
        probe.os_release = Some("ID=fedora\n");

        let status = support_from_probe(probe);
        assert!(!status.supported);
        assert!(status.can_check);
        assert_eq!(status.channel, "linux_pkg");
        assert_eq!(status.bundle_type.as_deref(), Some("rpm"));
    }

    #[test]
    fn rpm_without_rpm_binary_stays_silent() {
        // Same logic as deb-on-non-debian-family: if the package manager
        // tooling isn't there, we can't give the user actionable advice.
        let mut probe = probe("linux", Some("rpm"));
        probe.has_rpm = false;
        probe.os_release = Some("ID=fedora\n");

        let status = support_from_probe(probe);
        assert!(!status.supported);
        assert!(!status.can_check);
        assert_eq!(status.channel, "package_manager");
        assert!(
            status
                .reason
                .as_deref()
                .unwrap()
                .contains("missing its package-manager tooling"),
            "reason drifted: {:?}",
            status.reason,
        );
    }

    #[test]
    fn appimage_bundle_falls_through_to_silent_package_manager() {
        // Tripwire: AppImage is not a CLAI self-updater bundle. The probe
        // must classify it as can_check:false with channel "package_manager"
        // so the toast stays silent. If someone re-adds an AppImage target to
        // tauri.conf.json bundle targets later, this test fails first —
        // nudging them to think through whether notify-only is the desired
        // UX for AppImage users rather than letting it silently change.
        let status = support_from_probe(probe("linux", Some("appimage")));

        assert!(!status.supported);
        assert!(!status.can_check);
        assert_eq!(status.channel, "package_manager");
    }

    #[test]
    fn macos_app_bundle_is_supported_by_updater() {
        let status = support_from_probe(probe("macos", Some("app")));

        assert!(status.supported);
        assert!(status.can_check);
        assert_eq!(status.channel, "native");
    }

    #[test]
    fn parses_quoted_os_release_ids() {
        let ids = os_release_ids("NAME=Example\nID=\"ubuntu\"\nID_LIKE='debian rhel'\n");

        assert_eq!(ids, vec!["ubuntu", "debian", "rhel"]);
    }

    #[test]
    fn version_is_newer_compares_calver_numerically() {
        assert!(version_is_newer("26.7.13", "26.7.12"));
        assert!(version_is_newer("26.10.1", "26.9.30"));
        assert!(version_is_newer("v26.8.1", "26.7.12"));
        assert!(version_is_newer("26.7.12.1", "26.7.12"));
        assert!(version_is_newer("26.8.1", "26.7.12-38-g6148106"));

        assert!(!version_is_newer("26.7.12", "26.7.12"));
        assert!(!version_is_newer("26.7.11", "26.7.12"));
        assert!(!version_is_newer("not-a-version", "26.7.12"));
        assert!(!version_is_newer("26.8.1", "not-a-version"));
        assert!(!version_is_newer("", "26.7.12"));
    }

    #[test]
    fn notify_update_reports_newer_manifest_as_non_installable() {
        let manifest = UpdateManifest {
            version: "26.8.1".to_string(),
            notes: Some("  ".to_string()),
            pub_date: Some("2026-08-01T00:00:00Z".to_string()),
        };

        let update = notify_update_from_manifest("26.7.12", &manifest).expect("update expected");
        assert!(!update.installable);
        assert_eq!(update.version, "26.8.1");
        assert_eq!(update.current_version, "26.7.12");
        assert_eq!(update.body, None, "blank notes should be dropped");
        assert_eq!(update.date.as_deref(), Some("2026-08-01T00:00:00Z"));
    }

    #[test]
    fn notify_update_ignores_same_or_older_manifest() {
        let manifest = UpdateManifest {
            version: "26.7.12".to_string(),
            notes: None,
            pub_date: None,
        };

        assert!(notify_update_from_manifest("26.7.12", &manifest).is_none());
        assert!(notify_update_from_manifest("26.8.1", &manifest).is_none());
    }

    fn sample_info(version: &str) -> AppUpdateInfo {
        AppUpdateInfo {
            current_version: "26.7.12".to_string(),
            version: version.to_string(),
            date: None,
            body: None,
            installable: true,
            downloaded: false,
        }
    }

    /// A real `Update` cannot be built outside the updater plugin, so the
    /// bookkeeping is exercised with `()` standing in for the installer.
    fn runtime() -> AppUpdateRuntime<()> {
        AppUpdateRuntime::new()
    }

    fn supported() -> AppUpdateSupportStatus {
        super::supported(probe("macos", Some("app")), "native")
    }

    #[test]
    fn background_download_is_mandatory_on_self_updating_builds() {
        // The download used to be opt-out via `autoUpdate.autoDownload`.
        // Nothing gates it now except the build's own capability, and this
        // is the test that fails if a setting is ever wired back in.
        assert!(should_download_in_background(
            &supported(),
            &sample_info("26.8.1")
        ));
    }

    #[test]
    fn background_download_skips_builds_that_cannot_install() {
        // Notify-only channels (Flatpak, Linux deb/rpm) can see the new
        // version but never apply it, so downloading is pure waste.
        let mut probe = probe("linux", Some("deb"));
        probe.has_dpkg = true;
        probe.os_release = Some("ID=ubuntu\n");
        let notify_only = support_from_probe(probe);
        assert!(notify_only.can_check, "fixture should still be notify-only");

        assert!(!should_download_in_background(
            &notify_only,
            &sample_info("26.8.1")
        ));
        // Belt and braces: the per-update flag says the same thing.
        let not_installable = AppUpdateInfo {
            installable: false,
            ..sample_info("26.8.1")
        };
        assert!(!should_download_in_background(
            &supported(),
            &not_installable
        ));
    }

    #[test]
    fn background_download_does_not_repeat_a_finished_download() {
        let already_downloaded = AppUpdateInfo {
            downloaded: true,
            ..sample_info("26.8.1")
        };
        assert!(!should_download_in_background(
            &supported(),
            &already_downloaded
        ));
    }

    #[test]
    fn take_downloaded_returns_the_stored_package() {
        let runtime = runtime();
        runtime.store_downloaded("26.8.1", (), vec![1, 2, 3]);
        assert_eq!(runtime.downloaded_version().as_deref(), Some("26.8.1"));

        let package = runtime.take_downloaded().expect("package expected");
        assert_eq!(package.version, "26.8.1");
        assert_eq!(package.bytes, vec![1, 2, 3]);
        // Taking consumes the cache.
        assert_eq!(runtime.downloaded_version(), None);
        assert!(runtime.take_downloaded().is_none());
    }

    #[test]
    fn store_downloaded_replaces_a_superseded_package() {
        let runtime = runtime();
        runtime.store_downloaded("26.8.1", (), vec![1]);
        // A newer release was found and downloaded: only one package is ever
        // held, so the older one must not survive to be installed later.
        runtime.store_downloaded("26.8.2", (), vec![2]);

        assert_eq!(runtime.downloaded_version().as_deref(), Some("26.8.2"));
        let package = runtime.take_downloaded().expect("package expected");
        assert_eq!(package.version, "26.8.2");
        assert_eq!(package.bytes, vec![2]);
    }

    #[test]
    fn mark_downloaded_flags_recorded_check_and_returns_info() {
        let runtime = runtime();
        runtime.record_check(AppUpdateLastCheck {
            checked_at: "now".to_string(),
            update: Some(sample_info("26.8.1")),
            error: None,
        });
        let marked = runtime.mark_downloaded("26.8.1").expect("should mark");
        assert!(marked.downloaded);
        let recorded = runtime.last_check().unwrap().update.unwrap();
        assert!(recorded.downloaded);
    }

    #[test]
    fn record_check_rederives_downloaded_from_byte_cache() {
        let runtime = runtime();
        // A background download finished between the check's cache read and
        // its recording: recording must not regress `downloaded` to false.
        runtime.store_downloaded("26.8.1", (), vec![1]);
        let recorded = runtime.record_check(AppUpdateLastCheck {
            checked_at: "now".to_string(),
            update: Some(sample_info("26.8.1")),
            error: None,
        });
        assert!(recorded.update.unwrap().downloaded);
        assert!(runtime.last_check().unwrap().update.unwrap().downloaded);
    }

    #[test]
    fn mark_downloaded_ignores_version_mismatch() {
        let runtime = runtime();
        runtime.record_check(AppUpdateLastCheck {
            checked_at: "now".to_string(),
            update: Some(sample_info("26.8.2")),
            error: None,
        });
        assert!(runtime.mark_downloaded("26.8.1").is_none());
        assert!(!runtime.last_check().unwrap().update.unwrap().downloaded);
    }
}
