//! App update commands and startup update checks.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};

use crate::config::AutoUpdateConfig;
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

#[derive(Clone, Default)]
pub struct AppUpdateRuntime {
    last_check: Arc<Mutex<Option<AppUpdateLastCheck>>>,
    check_lock: Arc<tokio::sync::Mutex<()>>,
    install_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AppUpdateRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn last_check(&self) -> Option<AppUpdateLastCheck> {
        self.last_check
            .lock()
            .expect("app update state poisoned")
            .clone()
    }

    fn record_check(&self, check: AppUpdateLastCheck) {
        *self.last_check.lock().expect("app update state poisoned") = Some(check);
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
    pub settings: AutoUpdateConfig,
    pub support: AppUpdateSupportStatus,
    pub last_check: Option<AppUpdateLastCheck>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AppUpdateCheckResult {
    pub settings: AutoUpdateConfig,
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
    appimage_env: bool,
}

#[tauri::command]
pub fn get_auto_update_settings(state: State<'_, AppState>) -> Result<AutoUpdateConfig, String> {
    auto_update_settings(state.inner())
}

#[tauri::command]
pub fn set_auto_update_settings(
    settings: AutoUpdateConfig,
    state: State<'_, AppState>,
) -> Result<AutoUpdateConfig, String> {
    state
        .config_manager
        .lock()
        .map_err(|e| format!("Config lock error: {e}"))?
        .update(|config| {
            config.auto_update = settings.clone();
        })
        .map_err(|e| format!("Failed to save update settings: {e}"))?;
    Ok(settings)
}

#[tauri::command]
pub fn get_app_update_status(state: State<'_, AppState>) -> Result<AppUpdateStatus, String> {
    Ok(AppUpdateStatus {
        settings: auto_update_settings(state.inner())?,
        support: detect_support_status(),
        last_check: state.app_updates.last_check(),
    })
}

#[tauri::command]
pub async fn check_for_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppUpdateCheckResult, String> {
    Ok(check_for_update(&app, state.inner()).await)
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

    let update = app
        .updater_builder()
        .timeout(INSTALL_TIMEOUT)
        .build()
        .map_err(format_updater_error)?
        .check()
        .await
        .map_err(format_updater_error)?
        .ok_or_else(|| "No update is available.".to_string())?;

    let _ = on_event.send(AppUpdateInstallEvent::Started);
    let mut downloaded: u64 = 0;
    let bytes = tokio::time::timeout(
        DOWNLOAD_TIMEOUT,
        update.download(
            |chunk_len, total| {
                downloaded = downloaded.saturating_add(chunk_len as u64);
                let _ = on_event.send(AppUpdateInstallEvent::Progress { downloaded, total });
            },
            || {
                let _ = on_event.send(AppUpdateInstallEvent::DownloadFinished);
            },
        ),
    )
    .await
    .map_err(|_| "Timed out downloading the update package.".to_string())?
    .map_err(format_updater_error)?;

    let _ = on_event.send(AppUpdateInstallEvent::Installing);
    update.install(bytes).map_err(format_updater_error)?;
    app.restart();
}

pub fn spawn_startup_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_CHECK_DELAY).await;
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        match auto_update_settings(state.inner()) {
            Ok(settings) if settings.enabled => {}
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(error, "Skipping startup update check");
                return;
            }
        }

        let result = check_for_update(&app, state.inner()).await;
        if let Some(update) = result.last_check.update {
            if let Err(error) = app.emit(
                APP_UPDATE_AVAILABLE_EVENT,
                AppUpdateAvailableEvent { update },
            ) {
                tracing::warn!(%error, "Failed to emit app update notification");
            }
        }
    });
}

fn auto_update_settings(state: &AppState) -> Result<AutoUpdateConfig, String> {
    Ok(state
        .config_manager
        .lock()
        .map_err(|e| format!("Config lock error: {e}"))?
        .get()
        .auto_update)
}

async fn check_for_update(app: &AppHandle, state: &AppState) -> AppUpdateCheckResult {
    let _check_guard = state.app_updates.check_lock.lock().await;
    let settings = auto_update_settings(state).unwrap_or_default();
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
                    update: update.as_ref().map(update_info),
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
        // Notify-only channels (Flatpak): the build cannot install updates
        // itself, but we still tell the user a newer release exists so they
        // can fetch it through their package channel.
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

    state.app_updates.record_check(last_check.clone());
    AppUpdateCheckResult {
        settings,
        support,
        last_check,
    }
}

fn update_info(update: &Update) -> AppUpdateInfo {
    AppUpdateInfo {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date: update.date.as_ref().map(ToString::to_string),
        body: update.body.clone(),
        installable: true,
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
        appimage_env: std::env::var_os("APPIMAGE").is_some(),
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
            "This Flatpak build cannot update itself; download new releases from GitHub.",
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
    match probe.bundle_type {
        Some("deb") => {
            if probe.has_dpkg && os_release_matches(probe.os_release, DEB_FAMILIES) {
                supported(probe, "deb")
            } else {
                unsupported(
                    probe,
                    "package_manager",
                    "This Linux install should be updated by its package manager.",
                )
            }
        }
        Some("rpm") => {
            if probe.has_rpm && os_release_matches(probe.os_release, RPM_FAMILIES) {
                supported(probe, "rpm")
            } else {
                unsupported(
                    probe,
                    "package_manager",
                    "This Linux install should be updated by its package manager.",
                )
            }
        }
        Some("appimage") => {
            if probe.appimage_env {
                supported(probe, "appimage")
            } else {
                unsupported(
                    probe,
                    "appimage",
                    "This AppImage build was not launched from an AppImage runtime.",
                )
            }
        }
        Some(_) => unsupported(
            probe,
            "unsupported",
            "This Linux bundle type is not supported by CLAI self-updates.",
        ),
        None => unsupported(
            probe,
            "package_manager",
            "This Linux install does not expose an updater-capable bundle type.",
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
        // Flatpak bundles are side-loaded (no origin remote), so Flatpak
        // itself never updates them — notify-only is the only signal those
        // users get. Package-manager installs (e.g. AUR) do receive updates
        // through their repo, so they stay silent.
        can_check: channel == "flatpak",
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
            appimage_env: false,
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
    fn deb_support_requires_debian_family_and_dpkg() {
        let mut probe = probe("linux", Some("deb"));
        probe.has_dpkg = true;
        probe.os_release = Some("ID=ubuntu\nID_LIKE=debian\n");

        assert!(support_from_probe(probe).supported);

        probe.os_release = Some("ID=arch\n");
        let status = support_from_probe(probe);
        assert!(!status.supported);
        assert!(!status.can_check);
        assert_eq!(status.channel, "package_manager");
    }

    #[test]
    fn rpm_support_requires_rpm_family_and_rpm_binary() {
        let mut probe = probe("linux", Some("rpm"));
        probe.has_rpm = true;
        probe.os_release = Some("ID=fedora\n");

        assert!(support_from_probe(probe).supported);

        probe.has_rpm = false;
        assert!(!support_from_probe(probe).supported);
    }

    #[test]
    fn appimage_support_requires_appimage_runtime() {
        let mut probe = probe("linux", Some("appimage"));

        assert!(!support_from_probe(probe).supported);

        probe.appimage_env = true;
        assert!(support_from_probe(probe).supported);
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
}
