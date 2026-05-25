/**
 * WorkspaceSettingsModal
 *
 * Unified Workspace Settings surface: sidebar nav on the left
 * (Workspace: General, Schedule; Agents: Main, sub-agents, + Add agent),
 * content pane on the right. Replaces the previous gear-icon ->
 * AgentFormModal(mode=workspace) leaky abstraction.
 */

import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import ReactDOM from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import {
  getAgentTemplates,
  getMcpServers,
  getSkills,
  workspaceAgentDefaultExecution,
  workspaceCreateAgent,
  workspaceDeleteAgent,
  workspaceGetAgent,
  workspaceUpdateAgent,
} from '../../api/client';
import { assistantClient } from '../../assistant';
import { setWorkspaceTitle } from '../../workspace/client';
import IntervalSelect from './IntervalSelect';
import styles from './WorkspaceSettingsModal.module.css';

// ──────────────────────────────────────────────────────────────────────────
// Shared helpers (carried over from AgentFormModal — will be the only copy
// once that file is deleted)
// ──────────────────────────────────────────────────────────────────────────

const defaultExecution = () => ({
  sandbox: { network: 'enabled', sessionBus: 'allow' },
  filesystem: { extraPaths: [] },
  shell: {
    mode: 'off',
    allowedCommandPrefixes: [],
    blockedCommandPrefixes: [
      'rm', 'sudo', 'chmod', 'chown', 'dd', 'mkfs', 'mount', 'umount', 'shutdown', 'reboot',
    ],
  },
  web: { enabled: false },
});

const normalizeItems = (items = []) => items.map((item) => item.trim()).filter(Boolean);

const addUniqueItem = (items, value) => {
  const trimmed = value.trim();
  if (!trimmed || items.includes(trimmed)) return items;
  return [...items, trimmed];
};

const normalizePathGrants = (items = []) =>
  items
    .map((item) => ({
      path: item.path?.trim() || '',
      access: item.access || 'read_only',
      origin: item.origin || null,
    }))
    .filter((item) => item.path);

const grantOriginLabel = (origin) => {
  if (!origin || origin.kind === 'manual') return 'Manual';
  if (origin.kind === 'credentialsPreset') return 'Preset';
  if (origin.kind === 'approval') {
    const when = origin.grantedAtUnixMs
      ? new Date(origin.grantedAtUnixMs).toLocaleDateString(undefined, {
          year: 'numeric',
          month: 'short',
          day: 'numeric',
        })
      : null;
    return when ? `Approved ${when}` : 'Approved';
  }
  return 'Manual';
};

const normalizeExecution = (execution = {}) => {
  const d = defaultExecution();
  return {
    sandbox: {
      network: execution.sandbox?.network || d.sandbox.network,
      sessionBus: execution.sandbox?.sessionBus || d.sandbox.sessionBus,
    },
    filesystem: {
      extraPaths: normalizePathGrants(execution.filesystem?.extraPaths || d.filesystem.extraPaths),
    },
    shell: {
      mode: execution.shell?.mode || d.shell.mode,
      allowedCommandPrefixes: normalizeItems(execution.shell?.allowedCommandPrefixes || d.shell.allowedCommandPrefixes),
      blockedCommandPrefixes: normalizeItems(execution.shell?.blockedCommandPrefixes || d.shell.blockedCommandPrefixes),
    },
    web: { enabled: execution.web?.enabled || false },
  };
};

// Canonical JSON of the editable agent fields. Used to compare current
// form state against the loaded baseline so the Save button can disable
// itself when there are no pending changes. Pure function — pass already
// normalized values (e.g., execution coming from normalizeExecution).
const serializeAgentPayload = ({
  name,
  description,
  selectedSkillIds,
  selectedMcpServerIds,
  providerConnectionIds,
  sessionBusAllowed,
  extraPathGrants,
  shellMode,
  allowedCommands,
  blockedCommands,
  webEnabled,
  enabled,
}) => JSON.stringify({
  name: (name || '').trim(),
  description: (description || '').trim(),
  selectedSkillIds: [...(selectedSkillIds || [])],
  selectedMcpServerIds: [...(selectedMcpServerIds || [])],
  providerConnectionIds: [...(providerConnectionIds || [])],
  execution: {
    sandbox: { sessionBus: sessionBusAllowed ? 'allow' : 'deny' },
    filesystem: { extraPaths: extraPathGrants || [] },
    shell: {
      mode: shellMode,
      allowedCommandPrefixes: allowedCommands || [],
      blockedCommandPrefixes: blockedCommands || [],
    },
    web: { enabled: !!webEnabled },
  },
  enabled: !!enabled,
});

// ──────────────────────────────────────────────────────────────────────────
// Modal shell
// ──────────────────────────────────────────────────────────────────────────

const WorkspaceSettingsModal = ({
  isOpen,
  onClose,
  workspaceId,
  snapshot,
  initialSelection,
  onChanged,
}) => {
  const [selection, setSelection] = useState(initialSelection || { kind: 'general' });
  const [deps, setDeps] = useState({
    mcpServers: [],
    skills: [],
    providerConnections: [],
    agentTemplates: [],
    // Backend-provided defaults for a brand-new agent (includes `$HOME`
    // RO). `undefined` while the fetch is in flight; `null` if it failed
    // (AgentSection falls back to the local empty execution). The create
    // form waits for either outcome before initializing so the user sees
    // the granted paths up front instead of having them silently injected
    // on save.
    defaultExecution: undefined,
  });

  // Reset selection when the modal is (re)opened or the caller hands a
  // *meaningfully different* initialSelection (e.g., gear icon -> general,
  // drawer edit -> agent). Structural compare via a stringified key —
  // parents commonly pass inline literals like `{ kind: 'general' }`,
  // which have a fresh JS identity every render. A pure reference
  // dependency would snap the modal back to the initial section every
  // time the parent re-renders (e.g., Fleet's 5-second poll refresh).
  const initialSelectionKey = JSON.stringify(initialSelection || { kind: 'general' });
  useEffect(() => {
    if (isOpen) setSelection(JSON.parse(initialSelectionKey));
  }, [isOpen, initialSelectionKey]);

  // Load static dependencies once per open.
  useEffect(() => {
    if (!isOpen) return undefined;
    let cancelled = false;
    (async () => {
      const [servers, skills, connections, templates, defaults] = await Promise.allSettled([
        getMcpServers(),
        getSkills(),
        assistantClient.listProviderConnections(),
        getAgentTemplates(),
        workspaceAgentDefaultExecution(),
      ]);
      if (cancelled) return;
      setDeps({
        mcpServers: servers.status === 'fulfilled' ? (servers.value || []) : [],
        skills: skills.status === 'fulfilled' ? (skills.value || []) : [],
        providerConnections: connections.status === 'fulfilled' ? (connections.value || []) : [],
        agentTemplates: templates.status === 'fulfilled' ? (templates.value || []) : [],
        defaultExecution: defaults.status === 'fulfilled' ? (defaults.value || null) : null,
      });
    })();
    return () => { cancelled = true; };
  }, [isOpen]);

  // Escape key
  useEffect(() => {
    if (!isOpen) return undefined;
    const onKey = (e) => { if (e.key === 'Escape') onClose(); };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  // Prevent body scroll while open
  useEffect(() => {
    if (isOpen) {
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = '';
    }
    return () => { document.body.style.overflow = ''; };
  }, [isOpen]);

  const handleOverlay = useCallback((e) => {
    if (e.target === e.currentTarget) onClose();
  }, [onClose]);

  const agents = snapshot?.assignedAgents || [];
  const sortedAgents = useMemo(() => (
    [...agents].sort((a, b) => (a.isDefault === b.isDefault ? 0 : a.isDefault ? -1 : 1))
  ), [agents]);

  if (!isOpen) return null;

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={handleOverlay}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        <header className={styles.header}>
          <h2 className={styles.title}>
            {snapshot?.title || 'Workspace'} — Settings
          </h2>
          <button
            type="button"
            className={styles.closeButton}
            onClick={onClose}
            aria-label="Close settings"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </header>

        <div className={styles.body}>
          <aside className={styles.sidebar} aria-label="Settings sections">
            <div className={styles.sidebarGroup}>
              <h3 className={styles.sidebarGroupTitle}>Workspace</h3>
              <NavItem
                active={selection.kind === 'general'}
                onClick={() => setSelection({ kind: 'general' })}
              >
                General
              </NavItem>
              <NavItem
                active={selection.kind === 'schedule'}
                onClick={() => setSelection({ kind: 'schedule' })}
              >
                Schedule
              </NavItem>
            </div>

            <div className={styles.sidebarGroup}>
              <h3 className={styles.sidebarGroupTitle}>Agents</h3>
              {sortedAgents.map((agent) => (
                <NavItem
                  key={agent.id}
                  active={selection.kind === 'agent' && selection.agentId === agent.id}
                  onClick={() => setSelection({ kind: 'agent', agentId: agent.id })}
                >
                  {agent.isDefault ? 'Main' : (agent.displayName || agent.agentName || 'Untitled')}
                </NavItem>
              ))}
              <NavItem
                className={styles.navItemAddNew}
                active={selection.kind === 'new-agent'}
                onClick={() => setSelection({ kind: 'new-agent' })}
              >
                + Add agent
              </NavItem>
            </div>
          </aside>

          <main className={styles.content}>
            {selection.kind === 'general' && (
              <GeneralSection
                workspaceId={workspaceId}
                snapshot={snapshot}
                onSaved={onChanged}
              />
            )}
            {selection.kind === 'schedule' && (
              <ScheduleSection
                workspaceId={workspaceId}
                snapshot={snapshot}
                onSaved={onChanged}
              />
            )}
            {selection.kind === 'agent' && (
              <AgentSection
                key={selection.agentId}
                workspaceId={workspaceId}
                agentId={selection.agentId}
                snapshot={snapshot}
                deps={deps}
                onSaved={onChanged}
                onDeleted={() => {
                  onChanged?.();
                  setSelection({ kind: 'general' });
                }}
              />
            )}
            {selection.kind === 'new-agent' && (
              <AgentSection
                key="new-agent"
                workspaceId={workspaceId}
                agentId={null}
                snapshot={snapshot}
                deps={deps}
                onSaved={(created) => {
                  onChanged?.();
                  if (created?.id) {
                    setSelection({ kind: 'agent', agentId: created.id });
                  } else {
                    setSelection({ kind: 'general' });
                  }
                }}
              />
            )}
          </main>
        </div>
      </div>
    </div>,
    document.body
  );
};

const NavItem = ({ active, onClick, children, className }) => (
  <button
    type="button"
    className={`${styles.navItem} ${active ? styles.navItemActive : ''} ${className || ''}`}
    onClick={onClick}
  >
    {children}
  </button>
);

// ──────────────────────────────────────────────────────────────────────────
// Workspace / General
// ──────────────────────────────────────────────────────────────────────────

const GeneralSection = ({ workspaceId, snapshot, onSaved }) => {
  const [title, setTitle] = useState(snapshot?.title || '');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState(null);

  // Resync if the parent snapshot changes (e.g., another section saved).
  useEffect(() => { setTitle(snapshot?.title || ''); }, [snapshot?.title]);

  const handleSave = useCallback(async () => {
    const trimmed = title.trim();
    if (!trimmed) {
      setError('Workspace title cannot be empty.');
      return;
    }
    if (trimmed.length > 100) {
      setError('Workspace title must be 100 characters or less.');
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await setWorkspaceTitle(workspaceId, trimmed);
      onSaved?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to save title.'));
    } finally {
      setSaving(false);
    }
  }, [title, workspaceId, onSaved]);

  const isDirty = title.trim() !== (snapshot?.title || '').trim();

  return (
    <div className={styles.sectionRoot}>
      <h3 className={styles.sectionTitle}>General</h3>
      <p className={styles.sectionDescription}>
        Workspace identity. The title is what shows up in the Fleet view and on this workspace&apos;s page.
      </p>

      <div className={styles.field}>
        <label className={styles.label} htmlFor="ws-title">
          Title <span className={styles.required}>*</span>
        </label>
        <input
          id="ws-title"
          type="text"
          className={styles.input}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          disabled={saving}
          maxLength={100}
        />
        <span className={styles.hint}>
          Workspace ID: <code>{snapshot?.workspaceId}</code>
        </span>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      <div className={styles.actions}>
        <button
          type="button"
          className={styles.primaryButton}
          onClick={handleSave}
          disabled={saving || !title.trim() || !isDirty}
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  );
};

// ──────────────────────────────────────────────────────────────────────────
// Workspace / Schedule
// ──────────────────────────────────────────────────────────────────────────

// Common cron patterns surfaced as a dropdown — keeps simple cases
// one-click and lets users escape to free-text for anything custom.
const CRON_PRESETS = [
  { label: 'Every hour on the hour', value: '0 * * * *' },
  { label: 'Every day at 9:00 AM', value: '0 9 * * *' },
  { label: 'Every day at midnight', value: '0 0 * * *' },
  { label: 'Every weekday at 9:00 AM', value: '0 9 * * 1-5' },
  { label: 'Every Monday at 9:00 AM', value: '0 9 * * 1' },
  { label: 'First of the month at midnight', value: '0 0 1 * *' },
];

const initialScheduleKindFromSnapshot = (snapshot) => {
  const kind = snapshot?.scheduleKind;
  if (kind?.type === 'cron') {
    return { type: 'cron', expression: kind.expression || '', timezone: kind.timezone || '' };
  }
  if (kind?.type === 'interval') {
    return { type: 'interval', intervalMinutes: kind.intervalMinutes ?? 30 };
  }
  return { type: 'interval', intervalMinutes: 30 };
};

const formatUnixMsForUserLocale = (ms) => {
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return new Date(ms).toISOString();
  }
};

const ScheduleSection = ({ workspaceId, snapshot, onSaved }) => {
  const [enabled, setEnabled] = useState(!!snapshot?.scheduleEnabled);
  const [scheduleKind, setScheduleKind] = useState(() =>
    initialScheduleKindFromSnapshot(snapshot)
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const [previewTimes, setPreviewTimes] = useState([]);
  const [previewError, setPreviewError] = useState(null);

  // Resolve the host timezone once and use it as the default when the
  // user first switches to cron mode. Falls back to the browser's own
  // resolved zone if the Tauri command isn't available (e.g., dev).
  const [hostTimezone, setHostTimezone] = useState('UTC');
  useEffect(() => {
    let cancelled = false;
    invoke('workspace_host_timezone')
      .then((tz) => {
        if (!cancelled && typeof tz === 'string' && tz.length > 0) {
          setHostTimezone(tz);
        }
      })
      .catch(() => {
        const fallback = Intl.DateTimeFormat().resolvedOptions().timeZone;
        if (!cancelled && fallback) setHostTimezone(fallback);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    setEnabled(!!snapshot?.scheduleEnabled);
    setScheduleKind(initialScheduleKindFromSnapshot(snapshot));
  }, [snapshot?.scheduleEnabled, snapshot?.scheduleKind]);

  const paused = !!snapshot?.schedulePaused;

  // Live preview: every time the user edits the cron expression or
  // timezone, ping the backend for the next 3 fire times so they can
  // sanity-check what they typed before hitting Save.
  useEffect(() => {
    if (!enabled || scheduleKind.type !== 'cron') {
      setPreviewTimes([]);
      setPreviewError(null);
      return undefined;
    }
    const expr = (scheduleKind.expression || '').trim();
    const tz = (scheduleKind.timezone || '').trim();
    if (!expr || !tz) {
      setPreviewTimes([]);
      setPreviewError(null);
      return undefined;
    }
    let cancelled = false;
    invoke('workspace_preview_schedule', {
      kind: { type: 'cron', expression: expr, timezone: tz },
      count: 3,
    })
      .then((times) => {
        if (cancelled) return;
        setPreviewTimes(Array.isArray(times) ? times : []);
        setPreviewError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        setPreviewTimes([]);
        setPreviewError(typeof err === 'string' ? err : err?.message || 'Invalid schedule.');
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, scheduleKind]);

  const updateKindType = (type) => {
    if (type === 'cron') {
      setScheduleKind((prev) =>
        prev.type === 'cron'
          ? prev
          : { type: 'cron', expression: '0 * * * *', timezone: hostTimezone || 'UTC' }
      );
    } else {
      setScheduleKind((prev) =>
        prev.type === 'interval' ? prev : { type: 'interval', intervalMinutes: 30 }
      );
    }
  };

  const handleSave = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      let payloadKind = null;
      if (enabled) {
        if (scheduleKind.type === 'interval') {
          const mins = Number(scheduleKind.intervalMinutes);
          if (!Number.isFinite(mins) || mins < 1 || mins > 1440) {
            throw new Error('Interval must be between 1 minute and 24 hours.');
          }
          payloadKind = { type: 'interval', intervalMinutes: mins };
        } else {
          const expr = (scheduleKind.expression || '').trim();
          const tz = (scheduleKind.timezone || '').trim();
          if (!expr) throw new Error('Cron expression is required.');
          if (!tz) throw new Error('Timezone is required.');
          payloadKind = { type: 'cron', expression: expr, timezone: tz };
        }
      }
      await invoke('workspace_set_schedule', {
        workspaceId,
        kind: payloadKind,
      });
      onSaved?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to save schedule.');
    } finally {
      setBusy(false);
    }
  }, [enabled, scheduleKind, workspaceId, onSaved]);

  const handleTogglePaused = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke('workspace_set_schedule_paused', {
        workspaceId,
        paused: !paused,
      });
      onSaved?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to update pause state.'));
    } finally {
      setBusy(false);
    }
  }, [paused, workspaceId, onSaved]);

  const handleRunNow = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await invoke('workspace_run_now', { workspaceId });
      onSaved?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to trigger run.'));
    } finally {
      setBusy(false);
    }
  }, [workspaceId, onSaved]);

  const dirty =
    enabled !== !!snapshot?.scheduleEnabled
    || JSON.stringify(scheduleKind) !== JSON.stringify(initialScheduleKindFromSnapshot(snapshot));

  return (
    <div className={styles.sectionRoot}>
      <h3 className={styles.sectionTitle}>Schedule</h3>
      <p className={styles.sectionDescription}>
        When enabled, the main agent runs on the chosen schedule. Sub-agents are invoked on demand by the main agent — they don&apos;t have their own schedules.
      </p>

      <div className={styles.field}>
        <label className={styles.toggleRow}>
          <span className={styles.toggleLabel}>Run on a recurring schedule</span>
          <span className={`${styles.toggle} ${enabled ? styles.toggleOn : ''}`}>
            <input
              type="checkbox"
              className={styles.toggleInput}
              checked={enabled}
              onChange={(e) => setEnabled(e.target.checked)}
              disabled={busy}
            />
            <span className={styles.toggleTrack}>
              <span className={styles.toggleThumb} />
            </span>
          </span>
        </label>
      </div>

      <div className={styles.field}>
        <label className={styles.label} htmlFor="ws-schedule-type">Schedule type</label>
        <select
          id="ws-schedule-type"
          className={styles.input}
          value={scheduleKind.type}
          onChange={(e) => updateKindType(e.target.value)}
          disabled={busy || !enabled}
        >
          <option value="interval">Interval (every N minutes, anchored to last completion)</option>
          <option value="cron">Cron (fire at a specific time / day)</option>
        </select>
      </div>

      {scheduleKind.type === 'interval' && (
        <div className={styles.field}>
          <label className={styles.label} htmlFor="ws-interval">Interval</label>
          <IntervalSelect
            id="ws-interval"
            value={scheduleKind.intervalMinutes}
            onChange={(v) =>
              setScheduleKind((prev) => ({ ...prev, intervalMinutes: Number(v) }))
            }
            disabled={busy || !enabled}
          />
          <span className={styles.hint}>
            {enabled
              ? 'How often the main agent runs while not paused. Anchored to the previous completion.'
              : 'Stored for later if you re-enable scheduling.'}
          </span>
        </div>
      )}

      {scheduleKind.type === 'cron' && (
        <>
          <div className={styles.field}>
            <label className={styles.label} htmlFor="ws-cron-preset">Common patterns</label>
            <select
              id="ws-cron-preset"
              className={styles.input}
              value=""
              onChange={(e) => {
                if (!e.target.value) return;
                setScheduleKind((prev) => ({ ...prev, expression: e.target.value }));
              }}
              disabled={busy || !enabled}
            >
              <option value="">Pick a preset or write your own below…</option>
              {CRON_PRESETS.map((p) => (
                <option key={p.value} value={p.value}>{p.label} ({p.value})</option>
              ))}
            </select>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="ws-cron-expr">
              Cron expression (5 fields: minute hour day-of-month month day-of-week)
            </label>
            <input
              id="ws-cron-expr"
              type="text"
              className={styles.input}
              value={scheduleKind.expression || ''}
              onChange={(e) =>
                setScheduleKind((prev) => ({ ...prev, expression: e.target.value }))
              }
              placeholder="e.g. 0 9 * * 1-5"
              disabled={busy || !enabled}
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
            />
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="ws-cron-tz">Timezone (IANA)</label>
            <input
              id="ws-cron-tz"
              type="text"
              className={styles.input}
              value={scheduleKind.timezone || ''}
              onChange={(e) =>
                setScheduleKind((prev) => ({ ...prev, timezone: e.target.value }))
              }
              placeholder="e.g. America/New_York"
              disabled={busy || !enabled}
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
            />
            <span className={styles.hint}>
              Defaults to your system timezone ({hostTimezone}). Use any IANA name like
              {' '}<code>UTC</code>, <code>Europe/Madrid</code>, etc.
            </span>
          </div>

          <div className={styles.field}>
            <label className={styles.label}>Next 3 fire times</label>
            {previewError && (
              <div className={styles.errorBanner}>{previewError}</div>
            )}
            {!previewError && previewTimes.length === 0 && (
              <span className={styles.hint}>
                Enter a valid expression + timezone to see the next runs.
              </span>
            )}
            {!previewError && previewTimes.length > 0 && (
              <ul className={styles.cronPreviewList}>
                {previewTimes.map((ms) => (
                  <li key={ms}>{formatUnixMsForUserLocale(ms)}</li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}

      {snapshot?.scheduleEnabled && (
        <div className={styles.field}>
          <label className={styles.toggleRow}>
            <span className={styles.toggleLabel}>
              {paused ? 'Schedule is paused' : 'Schedule is running'}
            </span>
            <button
              type="button"
              className={styles.secondaryButton}
              onClick={handleTogglePaused}
              disabled={busy}
            >
              {paused ? 'Resume' : 'Pause'}
            </button>
          </label>
          <span className={styles.scheduleStatus}>
            {paused
              ? 'The scheduler will skip this workspace until resumed.'
              : 'Next tick fires on the configured interval.'}
          </span>
        </div>
      )}

      {error && <div className={styles.errorBanner}>{error}</div>}

      <div className={styles.actions}>
        {snapshot?.scheduleEnabled && !paused && (
          <button
            type="button"
            className={styles.secondaryButton}
            onClick={handleRunNow}
            disabled={busy}
          >
            Run now
          </button>
        )}
        <button
          type="button"
          className={styles.primaryButton}
          onClick={handleSave}
          disabled={busy || !dirty}
        >
          {busy ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  );
};

// ──────────────────────────────────────────────────────────────────────────
// Agent section (manager + sub-agent + new)
// ──────────────────────────────────────────────────────────────────────────

const AgentSection = ({
  workspaceId,
  agentId,             // string for edit; null for create
  snapshot: _snapshot, // unused; kept in signature for future use (e.g., showing peer agents)
  deps,
  onSaved,
  onDeleted,
}) => {
  const isCreate = !agentId;
  // Both flows start in a loading state: edit waits on `workspaceGetAgent`,
  // create waits on `deps.defaultExecution` so the form opens with the
  // backend's `$HOME` RO grant pre-populated instead of an empty list.
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [saving, setSaving] = useState(false);

  // Source-of-truth agent payload (loaded for edit, blank draft for create
  // — populated once `deps.defaultExecution` arrives).
  const [agent, setAgent] = useState(null);
  const isManager = agent?.isDefault === true;
  const canDelete = !isCreate && !isManager;

  // Form state
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [selectedMcpServerIds, setSelectedMcpServerIds] = useState([]);
  const [selectedSkillIds, setSelectedSkillIds] = useState([]);
  const [providerConnectionIds, setProviderConnectionIds] = useState([]);
  const [providerConnectionDraft, setProviderConnectionDraft] = useState('');
  const [extraPathGrants, setExtraPathGrants] = useState([]);
  const [extraPathDraft, setExtraPathDraft] = useState('');
  const [extraPathAccess, setExtraPathAccess] = useState('read_only');
  const [sessionBusAllowed, setSessionBusAllowed] = useState(true);
  const [shellMode, setShellMode] = useState('off');
  const [allowedCommands, setAllowedCommands] = useState([]);
  const [blockedCommands, setBlockedCommands] = useState(defaultExecution().shell.blockedCommandPrefixes);
  const [allowedCommandDraft, setAllowedCommandDraft] = useState('');
  const [blockedCommandDraft, setBlockedCommandDraft] = useState('');
  const [webEnabled, setWebEnabled] = useState(false);
  const [enabled, setEnabled] = useState(true);
  const [selectedTemplateId, setSelectedTemplateId] = useState('');

  // Track which agentId we last fetched, so the effect doesn't refetch on
  // every re-render. setting key={agentId} on the parent already remounts
  // but this is a belt-and-braces for future callers.
  const lastFetchedId = useRef(null);

  // Baseline payload captured at load time. The Save button compares
  // current form state against this to decide whether anything is pending.
  // Updated after a successful save so Save re-disables until the user
  // edits again.
  const baselinePayloadRef = useRef(null);

  // Load the agent for the edit flow.
  useEffect(() => {
    if (isCreate) return undefined;
    if (lastFetchedId.current === agentId) return undefined;
    let cancelled = false;
    setLoading(true);
    setError(null);
    workspaceGetAgent(workspaceId, agentId)
      .then((detail) => {
        if (cancelled || !detail) return;
        lastFetchedId.current = agentId;
        setAgent(detail);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(typeof err === 'string' ? err : (err?.message || 'Failed to load agent.'));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => { cancelled = true; };
  }, [workspaceId, agentId, isCreate]);

  // Initialize the blank draft for create flow once the backend's
  // default-execution fetch has resolved (success or failure). Doing this
  // here — rather than in the useState initializer — means the form opens
  // with `$HOME` RO already showing in the path-grants list, which the
  // user can ×-remove before clicking Create. A failed fetch falls back
  // to the local empty execution so the create flow still works.
  useEffect(() => {
    if (!isCreate) return;
    if (agent) return;                              // already initialized
    if (deps?.defaultExecution === undefined) return; // fetch still pending
    setAgent(blankAgentDraft(deps.defaultExecution || undefined));
    setLoading(false);
  }, [isCreate, agent, deps?.defaultExecution]);

  // Reset form fields whenever the source agent changes
  useEffect(() => {
    if (!agent) return;
    setName(agent.name || '');
    setDescription(agent.description || '');
    setSelectedMcpServerIds(agent.selectedMcpServerIds || []);
    setSelectedSkillIds(agent.selectedSkillIds || []);
    setProviderConnectionIds(agent.providerConnectionIds || []);
    setProviderConnectionDraft('');
    const execution = normalizeExecution(agent.execution);
    setExtraPathGrants(execution.filesystem.extraPaths);
    setExtraPathDraft('');
    setExtraPathAccess('read_only');
    setSessionBusAllowed(execution.sandbox.sessionBus === 'allow');
    setShellMode(execution.shell.mode);
    setAllowedCommands(execution.shell.allowedCommandPrefixes);
    setBlockedCommands(execution.shell.blockedCommandPrefixes);
    setAllowedCommandDraft('');
    setBlockedCommandDraft('');
    setWebEnabled(execution.web.enabled);
    setEnabled(agent.enabled !== false);
    setSelectedTemplateId('');

    // Capture the baseline that matches the values we just loaded into
    // form state. Built from the same normalized `execution` so the
    // representation matches what `currentPayload` will produce.
    baselinePayloadRef.current = serializeAgentPayload({
      name: agent.name,
      description: agent.description,
      selectedSkillIds: agent.selectedSkillIds,
      selectedMcpServerIds: agent.selectedMcpServerIds,
      providerConnectionIds: agent.providerConnectionIds,
      sessionBusAllowed: execution.sandbox.sessionBus === 'allow',
      extraPathGrants: execution.filesystem.extraPaths,
      shellMode: execution.shell.mode,
      allowedCommands: execution.shell.allowedCommandPrefixes,
      blockedCommands: execution.shell.blockedCommandPrefixes,
      webEnabled: execution.web.enabled,
      enabled: agent.enabled !== false,
    });
  }, [agent]);

  const enabledProviderConnections = useMemo(
    () => (deps?.providerConnections || []).filter((c) => c.enabled),
    [deps?.providerConnections]
  );

  const availableProviderConnections = useMemo(
    () => enabledProviderConnections.filter((c) => !providerConnectionIds.includes(c.id)),
    [enabledProviderConnections, providerConnectionIds]
  );

  useEffect(() => {
    if (providerConnectionDraft && availableProviderConnections.some((c) => c.id === providerConnectionDraft)) return;
    setProviderConnectionDraft(availableProviderConnections[0]?.id || '');
  }, [availableProviderConnections, providerConnectionDraft]);

  const selectedTemplate = useMemo(
    () => (deps?.agentTemplates || []).find((t) => t.id === selectedTemplateId) || null,
    [deps?.agentTemplates, selectedTemplateId]
  );

  // Canonical form payload, used to detect pending changes.
  const currentPayload = useMemo(
    () => serializeAgentPayload({
      name,
      description,
      selectedSkillIds,
      selectedMcpServerIds,
      providerConnectionIds,
      sessionBusAllowed,
      extraPathGrants,
      shellMode,
      allowedCommands,
      blockedCommands,
      webEnabled,
      enabled,
    }),
    [
      name, description, selectedSkillIds, selectedMcpServerIds, providerConnectionIds,
      sessionBusAllowed, extraPathGrants, shellMode, allowedCommands, blockedCommands,
      webEnabled, enabled,
    ]
  );

  // True when the form has changed from the loaded (or freshly created)
  // baseline. Drives the Save button's enabled state.
  const isDirty = baselinePayloadRef.current !== null
    && baselinePayloadRef.current !== currentPayload;

  const handleApplyTemplate = useCallback(() => {
    if (!selectedTemplate) return;
    setName(selectedTemplate.name || '');
    setDescription(selectedTemplate.description || '');
    setSelectedSkillIds(selectedTemplate.defaultSkillIds || []);
    const execution = normalizeExecution(selectedTemplate.defaultExecution || defaultExecution());
    setExtraPathGrants(execution.filesystem.extraPaths);
    setSessionBusAllowed(execution.sandbox.sessionBus === 'allow');
    setShellMode(execution.shell.mode);
    setAllowedCommands(execution.shell.allowedCommandPrefixes);
    setBlockedCommands(execution.shell.blockedCommandPrefixes);
    setWebEnabled(execution.web.enabled);
  }, [selectedTemplate]);

  const handleAddPathGrant = () => {
    const path = extraPathDraft.trim();
    if (!path) return;
    if (extraPathGrants.some((g) => g.path === path)) {
      setExtraPathDraft('');
      return;
    }
    setExtraPathGrants((current) => [
      ...current,
      { path, access: extraPathAccess, origin: null },
    ]);
    setExtraPathDraft('');
  };

  const handleSubmit = useCallback(async (e) => {
    e.preventDefault();
    setError(null);

    const trimmedName = name.trim();
    if (!isManager && !trimmedName) {
      setError('Agent name is required.');
      return;
    }
    if (trimmedName.length > 100) {
      setError('Name must be 100 characters or less.');
      return;
    }
    if (providerConnectionIds.length === 0) {
      setError('Select at least one provider connection.');
      return;
    }

    const execution = {
      sandbox: {
        network: 'enabled',
        sessionBus: sessionBusAllowed ? 'allow' : 'deny',
      },
      filesystem: { extraPaths: extraPathGrants },
      shell: {
        mode: shellMode,
        allowedCommandPrefixes: allowedCommands,
        blockedCommandPrefixes: blockedCommands,
      },
      web: { enabled: webEnabled },
    };

    setSaving(true);
    try {
      if (isCreate) {
        const created = await workspaceCreateAgent({
          workspaceId,
          name: trimmedName,
          description: description.trim(),
          selectedSkillIds,
          selectedMcpServerIds,
          providerConnectionIds,
          execution,
          enabled,
        });
        onSaved?.(created);
      } else {
        await workspaceUpdateAgent({
          workspaceId,
          agentId: agent.id,
          name: isManager ? (agent.name || 'Manager') : trimmedName,
          description: description.trim(),
          selectedSkillIds,
          selectedMcpServerIds,
          providerConnectionIds,
          execution,
          enabled: isManager ? true : enabled,
        });
        // Mark form clean: the values we just persisted are now the
        // baseline, so Save disables until the user edits something.
        baselinePayloadRef.current = currentPayload;
        onSaved?.();
      }
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to save agent.'));
    } finally {
      setSaving(false);
    }
  }, [
    name, description, selectedSkillIds, selectedMcpServerIds, providerConnectionIds,
    extraPathGrants, sessionBusAllowed, shellMode, allowedCommands, blockedCommands,
    webEnabled, enabled, isCreate, isManager, workspaceId, agent, onSaved, currentPayload,
  ]);

  const handleDelete = useCallback(async () => {
    if (!canDelete) return;
    if (!window.confirm(`Delete agent "${agent.name}"? This cannot be undone.`)) return;
    setSaving(true);
    setError(null);
    try {
      await workspaceDeleteAgent(workspaceId, agent.id);
      onDeleted?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to delete agent.'));
    } finally {
      setSaving(false);
    }
  }, [canDelete, agent, workspaceId, onDeleted]);

  if (loading) {
    return <div className={styles.sectionRoot}>Loading…</div>;
  }

  return (
    <form className={styles.sectionRoot} onSubmit={handleSubmit}>
      <h3 className={styles.sectionTitle}>
        {isCreate ? 'Add agent' : (isManager ? 'Main agent' : (agent?.name || 'Agent'))}
      </h3>
      <p className={styles.sectionDescription}>
        {isManager
          ? "This workspace's main agent. It's always present and runs whenever you send a message or the schedule fires."
          : isCreate
            ? 'Sub-agents are invoked by the main agent via delegation.'
            : 'Sub-agent — invoked by the main agent via delegation.'}
      </p>

      {/* Template picker (create only) */}
      {isCreate && (deps?.agentTemplates || []).length > 0 && (
        <div className={styles.field}>
          <label className={styles.label}>Start from template</label>
          <div className={styles.listInputRow}>
            <select
              className={styles.select}
              value={selectedTemplateId}
              onChange={(e) => setSelectedTemplateId(e.target.value)}
              disabled={saving}
            >
              <option value="">(no template)</option>
              {(deps.agentTemplates || []).map((tpl) => (
                <option key={tpl.id} value={tpl.id}>{tpl.name}</option>
              ))}
            </select>
            <button
              type="button"
              className={styles.addButton}
              onClick={handleApplyTemplate}
              disabled={!selectedTemplate || saving}
            >
              Apply
            </button>
          </div>
        </div>
      )}

      {/* Name (hidden for manager — its name is "Main" by convention) */}
      {!isManager && (
        <div className={styles.field}>
          <label className={styles.label} htmlFor="agent-name">
            Name <span className={styles.required}>*</span>
          </label>
          <input
            id="agent-name"
            type="text"
            className={styles.input}
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={saving}
            maxLength={100}
          />
        </div>
      )}

      <div className={styles.field}>
        <label className={styles.label} htmlFor="agent-description">Description</label>
        <textarea
          id="agent-description"
          className={styles.textarea}
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          disabled={saving}
          rows={5}
          placeholder={isManager
            ? 'Instructions for how the main agent behaves in this workspace…'
            : 'What this sub-agent does and when the main agent should delegate to it…'}
        />
        <span className={styles.hint}>
          Markdown supported. Appended to the agent&apos;s system prompt at runtime.
        </span>
      </div>

      {/* Skills */}
      <div className={styles.field}>
        <label className={styles.label}>Skills</label>
        {(deps?.skills || []).length === 0 ? (
          <span className={styles.hint}>No skills configured. Add skill sources in app settings.</span>
        ) : (
          <div className={styles.checkboxGroup}>
            {(deps?.skills || []).map((skill) => {
              const checked = selectedSkillIds.includes(skill.id);
              return (
                <label key={skill.id} className={styles.checkboxOption}>
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) => {
                      if (e.target.checked) {
                        setSelectedSkillIds((s) => [...s, skill.id]);
                      } else {
                        setSelectedSkillIds((s) => s.filter((id) => id !== skill.id));
                      }
                    }}
                    disabled={saving}
                  />
                  <span>
                    <strong>{skill.name}</strong>
                    {skill.description && <span className={styles.checkboxDescription}>{skill.description}</span>}
                  </span>
                </label>
              );
            })}
          </div>
        )}
      </div>

      {/* MCP Servers */}
      <div className={styles.field}>
        <label className={styles.label}>MCP servers</label>
        {(deps?.mcpServers || []).length === 0 ? (
          <span className={styles.hint}>No MCP servers configured.</span>
        ) : (
          <div className={styles.checkboxGroup}>
            {(deps?.mcpServers || []).map((server) => {
              const checked = selectedMcpServerIds.includes(server.id);
              return (
                <label key={server.id} className={styles.checkboxOption}>
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) => {
                      if (e.target.checked) {
                        setSelectedMcpServerIds((s) => [...s, server.id]);
                      } else {
                        setSelectedMcpServerIds((s) => s.filter((id) => id !== server.id));
                      }
                    }}
                    disabled={saving}
                  />
                  <span>{server.name}</span>
                </label>
              );
            })}
          </div>
        )}
      </div>

      {/* Provider connections */}
      <div className={styles.field}>
        <label className={styles.label}>
          Provider connections <span className={styles.required}>*</span>
        </label>
        {providerConnectionIds.length > 0 && (
          <div className={styles.chipList}>
            {providerConnectionIds.map((id) => {
              const conn = enabledProviderConnections.find((c) => c.id === id);
              return (
                <span key={id} className={styles.chip}>
                  {conn?.name || id}
                  <button
                    type="button"
                    className={styles.chipRemove}
                    onClick={() => setProviderConnectionIds((s) => s.filter((x) => x !== id))}
                    disabled={saving}
                    aria-label={`Remove ${conn?.name || id}`}
                  >
                    ×
                  </button>
                </span>
              );
            })}
          </div>
        )}
        {availableProviderConnections.length > 0 && (
          <div className={styles.listInputRow}>
            <select
              className={styles.select}
              value={providerConnectionDraft}
              onChange={(e) => setProviderConnectionDraft(e.target.value)}
              disabled={saving}
            >
              {availableProviderConnections.map((c) => (
                <option key={c.id} value={c.id}>{c.name}</option>
              ))}
            </select>
            <button
              type="button"
              className={styles.addButton}
              onClick={() => {
                if (!providerConnectionDraft) return;
                setProviderConnectionIds((s) => addUniqueItem(s, providerConnectionDraft));
              }}
              disabled={!providerConnectionDraft || saving}
            >
              Add
            </button>
          </div>
        )}
      </div>

      {/* Local capabilities */}
      <h4 className={styles.sectionTitle} style={{ marginTop: 24 }}>Local capabilities</h4>
      <p className={styles.sectionDescription}>
        What this agent can do on your machine. Every new agent ships with <code>$HOME</code> read-only — remove it below if you want this agent fully isolated.
      </p>

      <div className={styles.field}>
        <label className={styles.label}>Additional path grants</label>
        {extraPathGrants.length > 0 && (
          <div className={styles.grantList}>
            {extraPathGrants.map((grant) => (
              <div key={grant.path} className={styles.grantItem}>
                <span className={styles.grantPath}>{grant.path}</span>
                <span className={styles.grantAccess}>{grant.access === 'read_write' ? 'RW' : 'RO'}</span>
                <span className={styles.grantOrigin} title={grant.origin?.reason || undefined}>
                  {grantOriginLabel(grant.origin)}
                </span>
                <button
                  type="button"
                  className={styles.chipRemove}
                  onClick={() => setExtraPathGrants((s) => s.filter((g) => g.path !== grant.path))}
                  disabled={saving}
                  aria-label={`Remove grant for ${grant.path}`}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
        )}
        <div className={styles.listInputRow}>
          <input
            type="text"
            className={styles.input}
            value={extraPathDraft}
            onChange={(e) => setExtraPathDraft(e.target.value)}
            placeholder="/home/user/project"
            disabled={saving}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); handleAddPathGrant(); } }}
          />
          <select
            className={styles.select}
            value={extraPathAccess}
            onChange={(e) => setExtraPathAccess(e.target.value)}
            disabled={saving}
          >
            <option value="read_only">Read only</option>
            <option value="read_write">Read &amp; write</option>
          </select>
          <button
            type="button"
            className={styles.addButton}
            onClick={handleAddPathGrant}
            disabled={!extraPathDraft.trim() || saving}
          >
            Add
          </button>
        </div>
      </div>

      <div className={styles.field}>
        <label className={styles.label} htmlFor="shell-mode">Shell access</label>
        <select
          id="shell-mode"
          className={styles.select}
          value={shellMode}
          onChange={(e) => setShellMode(e.target.value)}
          disabled={saving}
        >
          <option value="off">Off</option>
          <option value="restricted">Restricted (allow/block lists)</option>
          <option value="full">Full</option>
        </select>
      </div>

      {shellMode === 'restricted' && (
        <>
          <div className={styles.field}>
            <label className={styles.label}>Allowed command prefixes</label>
            {allowedCommands.length > 0 && (
              <div className={styles.chipList}>
                {allowedCommands.map((cmd) => (
                  <span key={cmd} className={styles.chip}>
                    {cmd}
                    <button
                      type="button"
                      className={styles.chipRemove}
                      onClick={() => setAllowedCommands((s) => s.filter((c) => c !== cmd))}
                      disabled={saving}
                      aria-label={`Remove ${cmd}`}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
            <div className={styles.listInputRow}>
              <input
                type="text"
                className={styles.input}
                value={allowedCommandDraft}
                onChange={(e) => setAllowedCommandDraft(e.target.value)}
                placeholder="git status"
                disabled={saving}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    setAllowedCommands((s) => addUniqueItem(s, allowedCommandDraft));
                    setAllowedCommandDraft('');
                  }
                }}
              />
              <button
                type="button"
                className={styles.addButton}
                onClick={() => {
                  setAllowedCommands((s) => addUniqueItem(s, allowedCommandDraft));
                  setAllowedCommandDraft('');
                }}
                disabled={!allowedCommandDraft.trim() || saving}
              >
                Add
              </button>
            </div>
          </div>

          <div className={styles.field}>
            <label className={styles.label}>Blocked command prefixes</label>
            {blockedCommands.length > 0 && (
              <div className={styles.chipList}>
                {blockedCommands.map((cmd) => (
                  <span key={cmd} className={styles.chip}>
                    {cmd}
                    <button
                      type="button"
                      className={styles.chipRemove}
                      onClick={() => setBlockedCommands((s) => s.filter((c) => c !== cmd))}
                      disabled={saving}
                      aria-label={`Remove ${cmd}`}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
            <div className={styles.listInputRow}>
              <input
                type="text"
                className={styles.input}
                value={blockedCommandDraft}
                onChange={(e) => setBlockedCommandDraft(e.target.value)}
                placeholder="rm -rf"
                disabled={saving}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    setBlockedCommands((s) => addUniqueItem(s, blockedCommandDraft));
                    setBlockedCommandDraft('');
                  }
                }}
              />
              <button
                type="button"
                className={styles.addButton}
                onClick={() => {
                  setBlockedCommands((s) => addUniqueItem(s, blockedCommandDraft));
                  setBlockedCommandDraft('');
                }}
                disabled={!blockedCommandDraft.trim() || saving}
              >
                Add
              </button>
            </div>
          </div>
        </>
      )}

      <div className={styles.field}>
        <label className={styles.toggleRow}>
          <span className={styles.toggleLabel}>Web access (fetch, search)</span>
          <span className={`${styles.toggle} ${webEnabled ? styles.toggleOn : ''}`}>
            <input
              type="checkbox"
              className={styles.toggleInput}
              checked={webEnabled}
              onChange={(e) => setWebEnabled(e.target.checked)}
              disabled={saving}
            />
            <span className={styles.toggleTrack}>
              <span className={styles.toggleThumb} />
            </span>
          </span>
        </label>
      </div>

      <div className={styles.field}>
        <label className={styles.toggleRow}>
          <span className={styles.toggleLabel}>Allow session D-Bus (libsecret / keyring)</span>
          <span className={`${styles.toggle} ${sessionBusAllowed ? styles.toggleOn : ''}`}>
            <input
              type="checkbox"
              className={styles.toggleInput}
              checked={sessionBusAllowed}
              onChange={(e) => setSessionBusAllowed(e.target.checked)}
              disabled={saving}
            />
            <span className={styles.toggleTrack}>
              <span className={styles.toggleThumb} />
            </span>
          </span>
        </label>
      </div>

      {/* Enabled toggle (sub-agents only — manager is always enabled) */}
      {!isManager && !isCreate && (
        <div className={styles.field}>
          <label className={styles.toggleRow}>
            <span className={styles.toggleLabel}>Enabled</span>
            <span className={`${styles.toggle} ${enabled ? styles.toggleOn : ''}`}>
              <input
                type="checkbox"
                className={styles.toggleInput}
                checked={enabled}
                onChange={(e) => setEnabled(e.target.checked)}
                disabled={saving}
              />
              <span className={styles.toggleTrack}>
                <span className={styles.toggleThumb} />
              </span>
            </span>
          </label>
        </div>
      )}

      {error && <div className={styles.errorBanner}>{error}</div>}

      <div className={styles.actions}>
        {canDelete && (
          <button
            type="button"
            className={`${styles.dangerButton} ${styles.deleteSpacer}`}
            onClick={handleDelete}
            disabled={saving}
          >
            Delete agent
          </button>
        )}
        <button
          type="submit"
          className={styles.primaryButton}
          disabled={saving || !isDirty}
        >
          {saving ? 'Saving…' : (isCreate ? 'Create' : 'Save')}
        </button>
      </div>
    </form>
  );
};

// Initial draft used by the "Add agent" form. The backend-provided
// `defaultExecution` (carries the `$HOME` RO grant) is preferred so the
// user sees the granted paths up front. Falls back to the local empty
// execution if the fetch failed — better to show an empty list than to
// block the whole create flow.
const blankAgentDraft = (defaultExecutionFromBackend) => ({
  isDefault: false,
  name: '',
  description: '',
  selectedSkillIds: [],
  selectedMcpServerIds: [],
  providerConnectionIds: [],
  execution: defaultExecutionFromBackend || defaultExecution(),
  enabled: true,
});

export default WorkspaceSettingsModal;
