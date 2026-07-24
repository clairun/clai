/**
 * AboutSettings Component
 *
 * A compact version of the clai landing hero: logo, name, tagline, feature
 * pills, version, and a link to the repo. Version is read at runtime from the
 * Tauri app config.
 */

import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type {
  AppUpdateCheckResult,
  AppUpdateStatus,
  AutoUpdateConfig,
} from '../../generated/bindings';
import {
  LATEST_RELEASE_URL,
  installAppUpdate,
  installEventText,
  updateErrorText,
} from '../../utils/appUpdates';
import { openExternal } from '../../utils/openExternal';
import styles from './AboutSettings.module.css';

const REPO_URL = 'https://github.com/clairun/clai';

const PILLS = ['macOS · Windows · Linux', 'MCP-native', 'MIT licensed'];

const GitHubIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
    <path d="M12 .5C5.73.5.5 5.73.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.56 0-.27-.01-1.16-.02-2.1-3.2.7-3.88-1.37-3.88-1.37-.52-1.33-1.28-1.69-1.28-1.69-1.05-.72.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.77 2.7 1.26 3.36.96.1-.75.4-1.26.73-1.55-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.29 1.19-3.1-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.18 1.18a11.1 11.1 0 0 1 5.8 0c2.2-1.49 3.17-1.18 3.17-1.18.63 1.59.23 2.76.11 3.05.74.81 1.19 1.84 1.19 3.1 0 4.42-2.69 5.39-5.25 5.68.41.36.78 1.07.78 2.16 0 1.56-.01 2.82-.01 3.2 0 .31.21.68.8.56A11.51 11.51 0 0 0 23.5 12C23.5 5.73 18.27.5 12 .5z" />
  </svg>
);

const AboutSettings = () => {
  const [version, setVersion] = useState('');
  const [updateStatus, setUpdateStatus] = useState<AppUpdateStatus | null>(null);
  const [updateError, setUpdateError] = useState('');
  const [checking, setChecking] = useState(false);
  const [savingAutoUpdate, setSavingAutoUpdate] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [installProgress, setInstallProgress] = useState('');

  useEffect(() => {
    let cancelled = false;
    // Dev-aware version: the crate version in a release build, or the
    // `git describe` string (e.g. `26.6.7-38-g6148106`) in a dev build past
    // the last release tag. See commands::app_info::app_version_detail.
    invoke<string>('app_version_detail')
      .then((value) => {
        if (!cancelled) setVersion(value);
      })
      .catch((err) => {
        // Version is non-essential; leave it blank rather than failing the page.
        console.error('[AboutSettings] Failed to read app version:', err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    invoke<AppUpdateStatus>('get_app_update_status')
      .then((status) => {
        if (!cancelled) setUpdateStatus(status);
      })
      .catch((err) => {
        if (!cancelled) setUpdateError(updateErrorText(err, 'Failed to read update status.'));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const persistAutoUpdate = (autoDownload: boolean) => {
    if (!updateStatus || savingAutoUpdate) return;
    const previousSettings = updateStatus.settings;
    const settings: AutoUpdateConfig = { autoDownload };
    setSavingAutoUpdate(true);
    setUpdateError('');
    setUpdateStatus((current) => (current ? { ...current, settings } : current));
    invoke<AutoUpdateConfig>('set_auto_update_settings', { settings })
      .then((saved) => {
        setUpdateStatus((current) => (current ? { ...current, settings: saved } : current));
      })
      .catch((err) => {
        setUpdateError(updateErrorText(err, 'Failed to save update settings.'));
        setUpdateStatus((current) =>
          current ? { ...current, settings: previousSettings } : current
        );
      })
      .finally(() => {
        setSavingAutoUpdate(false);
      });
  };

  const checkForUpdates = async () => {
    setChecking(true);
    setUpdateError('');
    try {
      const result = await invoke<AppUpdateCheckResult>('check_for_app_update');
      setUpdateStatus({
        settings: result.settings,
        support: result.support,
        lastCheck: result.lastCheck,
      });
    } catch (err) {
      setUpdateError(updateErrorText(err, 'Failed to check for updates.'));
    } finally {
      setChecking(false);
    }
  };

  const installUpdate = async () => {
    setInstalling(true);
    setInstallProgress('Starting download...');
    setUpdateError('');
    try {
      await installAppUpdate((event) => {
        setInstallProgress(installEventText(event));
      });
    } catch (err) {
      setUpdateError(updateErrorText(err, 'Failed to install update.'));
      setInstalling(false);
    }
  };

  const support = updateStatus?.support;
  const lastCheck = updateStatus?.lastCheck;
  const availableUpdate = lastCheck?.update ?? null;
  const supportsUpdates = support?.supported ?? false;
  const canCheck = support?.canCheck ?? false;
  const supportDescription = updateStatus
    ? support?.reason || `${support?.platform ?? 'Desktop'} ${support?.bundleType ?? 'native'}`
    : 'Checking update support...';
  const supportBadge = !updateStatus
    ? 'Checking'
    : supportsUpdates
      ? 'Available'
      : canCheck
        ? 'Notify only'
        : 'Unavailable';
  // Fully-unavailable builds (dev, package-manager installs): the header
  // reason already says everything. Rendering the status line too would
  // repeat the same sentence (the backend mirrors the reason into
  // lastCheck.error), and the check button would be permanently disabled —
  // so both are hidden.
  const updatesUnavailable = updateStatus !== null && !supportsUpdates && !canCheck;
  const updateSummary = availableUpdate
    ? availableUpdate.downloaded
      ? `CLAI v${availableUpdate.version} has been downloaded. Restart to apply it.`
      : `CLAI v${availableUpdate.version} is available.`
    : lastCheck?.error || (lastCheck ? 'CLAI is up to date.' : 'Not checked yet.');

  return (
    <div className={styles.hero}>
      <img src="/icon.svg" alt="CLAI logo" className={styles.logo} />
      <h2 className={styles.title}>CLAI</h2>
      {version && <span className={styles.version}>v{version}</span>}

      <p className={styles.tagline}>
        Build, run, and supervise teams of AI agents on your desktop.
      </p>
      <p className={styles.subtitle}>
        Multi-agent orchestration, with MCP-native tools and a local execution sandbox.
      </p>

      <div className={styles.pills}>
        {PILLS.map((pill) => (
          <span key={pill} className={styles.pill}>
            {pill}
          </span>
        ))}
      </div>

      <button
        type="button"
        className={styles.githubButton}
        onClick={() => {
          openExternal(REPO_URL).catch((err) =>
            console.error('[AboutSettings] Failed to open link:', err)
          );
        }}
      >
        <GitHubIcon />
        <span>View on GitHub</span>
      </button>

      <div className={styles.updatePanel}>
        <div className={styles.updateHeader}>
          <div className={styles.updateTitleGroup}>
            <span className={styles.updateTitle}>Updates</span>
            <span className={styles.updateDesc}>{supportDescription}</span>
          </div>
          <span className={`${styles.updateBadge} ${supportsUpdates ? styles.updateBadgeOk : ''}`}>
            {supportBadge}
          </span>
        </div>

        {/* Checking for updates is always on; only the background download is
            configurable, and only where this build can actually install
            updates itself. Notify-only builds (e.g. Flatpak) get no toggle. */}
        {supportsUpdates && (
          <label className={styles.toggleRow}>
            <span className={styles.toggleCopy}>
              <span className={styles.toggleTitle}>Automatically download updates</span>
              <span className={styles.toggleDesc}>
                New versions download in the background. You choose when to restart.
              </span>
            </span>
            <span
              className={`${styles.toggle} ${
                updateStatus?.settings.autoDownload ? styles.toggleOn : ''
              }`}
            >
              <input
                type="checkbox"
                className={styles.toggleInput}
                checked={updateStatus?.settings.autoDownload ?? true}
                onChange={(event) => persistAutoUpdate(event.target.checked)}
                disabled={savingAutoUpdate}
              />
              <span className={styles.toggleTrack}>
                <span className={styles.toggleThumb} />
              </span>
            </span>
          </label>
        )}

        {!updatesUnavailable && (
          <div className={styles.updateStatus}>
            <span>{installing ? installProgress : updateSummary}</span>
            {updateError && <span className={styles.updateError}>{updateError}</span>}
          </div>
        )}
        {updatesUnavailable && updateError && (
          <div className={styles.updateStatus}>
            <span className={styles.updateError}>{updateError}</span>
          </div>
        )}

        {!updatesUnavailable && (
          <div className={styles.updateActions}>
            <button
              type="button"
              className={styles.secondaryButton}
              onClick={checkForUpdates}
              disabled={checking || !canCheck}
            >
              {checking ? 'Checking...' : 'Check for updates'}
            </button>
            {availableUpdate &&
              (availableUpdate.installable ? (
                <button
                  type="button"
                  className={styles.primaryButton}
                  onClick={installUpdate}
                  disabled={installing}
                >
                  {installing
                    ? 'Installing...'
                    : availableUpdate.downloaded
                      ? 'Restart and update'
                      : 'Install and restart'}
                </button>
              ) : (
                <button
                  type="button"
                  className={styles.primaryButton}
                  onClick={() => {
                    openExternal(LATEST_RELEASE_URL).catch((err) =>
                      console.error('[AboutSettings] Failed to open release page:', err)
                    );
                  }}
                >
                  View release
                </button>
              ))}
          </div>
        )}
      </div>
    </div>
  );
};

export default AboutSettings;
