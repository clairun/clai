import React, { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { SystemAppsConfig, SystemAppsStatus } from '../../generated/bindings';
import styles from './ApplicationsSettings.module.css';

/**
 * ApplicationsSettings — GNOME-style "default applications" rows for the
 * apps clai hands off to: the editor and the terminal used by the
 * "open in…" actions on artifacts.
 *
 * Dropdowns are populated by probing the host for a curated list of
 * known apps (each entry carries its own command-line incantation, so
 * users never write templates). "System default" goes through
 * xdg-open / xdg-terminal-exec — the OS owns those associations — and
 * "Custom…" exposes a free-text template as the escape hatch.
 */
const ApplicationsSettings = () => {
  const [status, setStatus] = useState<SystemAppsStatus | null>(null);
  const [config, setConfig] = useState<SystemAppsConfig | null>(null);
  const [error, setError] = useState('');

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      invoke<SystemAppsStatus>('system_apps_detect'),
      invoke<SystemAppsConfig>('get_system_apps_settings'),
    ])
      .then(([detected, settings]) => {
        if (cancelled) return;
        setStatus(detected);
        setConfig(settings);
      })
      .catch((err) => {
        if (!cancelled) setError(typeof err === 'string' ? err : 'Failed to load applications.');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const persist = useCallback((next: SystemAppsConfig) => {
    setConfig(next);
    invoke('set_system_apps_settings', { settings: next }).catch((err) => {
      setError(typeof err === 'string' ? err : 'Failed to save.');
    });
  }, []);

  if (error) return <div className={styles.section}>{error}</div>;
  if (!status || !config) return <div className={styles.section}>Detecting applications…</div>;

  const systemEditorLabel = status.systemEditorName
    ? `System default (${status.systemEditorName})`
    : 'System default';

  return (
    <div className={styles.section}>
      <h3 className={styles.heading}>Applications</h3>
      <p className={styles.subtle}>
        Used by the “open in…” actions on workspace artifacts. Detected from the apps installed on
        this system.
      </p>

      {status.editors.length === 0 && status.terminals.length === 0 && (
        <p className={styles.subtle}>
          No known editors or terminals were detected on this system. Install one (e.g. VS Code or
          Windows Terminal) and reopen Settings, or pick “Custom…” below to point at any command.
        </p>
      )}

      <div className={styles.row}>
        <div className={styles.rowLabel}>
          <span className={styles.rowTitle}>Editor</span>
          <span className={styles.rowDesc}>Opens artifact files and workspace folders.</span>
        </div>
        <select
          className={styles.select}
          value={config.editor ?? 'system'}
          onChange={(event) => {
            const value = event.target.value;
            persist({ ...config, editor: value === 'system' ? null : value });
          }}
        >
          <option value="system">{systemEditorLabel}</option>
          {status.editors.map((app) => (
            <option key={app.id} value={app.id}>
              {app.name}
            </option>
          ))}
          <option value="custom">Custom…</option>
        </select>
      </div>
      {config.editor === 'custom' && (
        <input
          type="text"
          className={styles.customInput}
          value={config.editorCustomCommand ?? ''}
          onChange={(event) => persist({ ...config, editorCustomCommand: event.target.value })}
          placeholder="e.g. code --goto {path}"
          spellCheck={false}
        />
      )}

      <div className={styles.row}>
        <div className={styles.rowLabel}>
          <span className={styles.rowTitle}>Terminal</span>
          <span className={styles.rowDesc}>Opens at the workspace folder.</span>
        </div>
        <select
          className={styles.select}
          value={config.terminal ?? 'auto'}
          onChange={(event) => {
            const value = event.target.value;
            persist({ ...config, terminal: value === 'auto' ? null : value });
          }}
        >
          <option value="auto">Automatic</option>
          {status.terminals.map((app) => (
            <option key={app.id} value={app.id}>
              {app.name}
            </option>
          ))}
          <option value="custom">Custom…</option>
        </select>
      </div>
      {config.terminal === 'custom' && (
        <input
          type="text"
          className={styles.customInput}
          value={config.terminalCustomCommand ?? ''}
          onChange={(event) => persist({ ...config, terminalCustomCommand: event.target.value })}
          placeholder="e.g. kitty --directory {dir}"
          spellCheck={false}
        />
      )}
    </div>
  );
};

export default ApplicationsSettings;
