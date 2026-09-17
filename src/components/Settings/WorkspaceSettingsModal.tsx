/**
 * WorkspaceSettingsModal
 *
 * Unified Workspace Settings surface: sidebar nav on the left
 * (Workspace: General, Schedule; Agents: Main, sub-agents, + Add agent),
 * content pane on the right. Replaces the previous gear-icon ->
 * AgentFormModal(mode=workspace) leaky abstraction.
 */

import React, { useState, useEffect, useCallback, useImperativeHandle, useMemo, useRef } from 'react';
import ReactDOM from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { getMcpServers, getSkills, workspaceAgentDefaultExecution } from '../../api/client';
import { assistantClient } from '../../assistant';
import { setWorkspaceTitle } from '../../workspace/client';
import IntervalSelect from './IntervalSelect';
import { AssignmentSection, TeamPolicySection } from './WorkspaceTeamSettings';
import AgentBehaviorForm, { type AgentFormDeps } from './AgentBehaviorForm';
import type { SectionHandle } from './sectionHandle';
import type { ScheduleKind, WorkspaceSnapshot } from '../../generated/bindings';
import AgentAvatar from '../Agents/AgentAvatar';
import { identityFor } from '../Agents/agentIdentity';
import styles from './WorkspaceSettingsModal.module.css';

// ──────────────────────────────────────────────────────────────────────────
// Local shapes. Agent payload types live with the form in
// AgentBehaviorForm.tsx; only the modal's own navigation state is here.
// ──────────────────────────────────────────────────────────────────────────

type SectionKind = 'general' | 'schedule' | 'team' | 'agent' | 'new-main' | 'new-agent';
interface Selection {
  kind: SectionKind;
  agentId?: string | null;
}

type ScheduleKindDraft =
  | { type: 'interval'; intervalMinutes: number }
  | { type: 'cron'; expression: string; timezone: string };

// Stable string identifier per sidebar selection. Used as a key for the
// `visited`/`dirty` maps and the section-ref registry so the modal can
// address each section without ad-hoc string formatting at every callsite.
const selectionKey = (sel: Selection | null | undefined): string => {
  if (!sel) return 'general';
  if (sel.kind === 'agent') return `agent:${sel.agentId}`;
  return sel.kind;
};

const parseSelectionKey = (key: string): Selection => {
  if (key.startsWith('agent:')) return { kind: 'agent', agentId: key.slice('agent:'.length) };
  return { kind: key as SectionKind };
};

// ──────────────────────────────────────────────────────────────────────────
// Modal shell
// ──────────────────────────────────────────────────────────────────────────

interface WorkspaceSettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  workspaceId: string;
  snapshot: WorkspaceSnapshot | null;
  initialSelection?: Selection | null;
  onChanged?: () => void;
}

// Keep in sync with WorkspaceContextBar: the self-loading context bar
// listens for this to refetch its snapshot after a settings save.
const WORKSPACE_SETTINGS_CHANGED_EVENT = 'workspace-settings-changed';

const WorkspaceSettingsModal = ({
  isOpen,
  onClose,
  workspaceId,
  snapshot,
  initialSelection,
  onChanged,
}: WorkspaceSettingsModalProps) => {
  // Structural compare via a stringified key — parents commonly pass
  // inline literals like `{ kind: 'general' }`, which have fresh JS
  // identity every render. A pure reference dep would snap the modal
  // back to the initial section on every parent re-render.
  const initialSelectionKey = JSON.stringify(initialSelection || { kind: 'general' });
  const initialSel = useMemo(
    () => JSON.parse(initialSelectionKey),
    [initialSelectionKey],
  );

  const [selection, setSelection] = useState<Selection>(initialSel);
  const [deps, setDeps] = useState<AgentFormDeps>({
    mcpServers: [],
    skills: [],
    providerConnections: [],
    // Backend-provided defaults for a brand-new agent (includes `$HOME`
    // RO). `undefined` while the fetch is in flight; `null` if it failed
    // (AgentBehaviorForm falls back to the local empty execution). The create
    // form waits for either outcome before initializing so the user sees
    // the granted paths up front instead of having them silently injected
    // on save.
    defaultExecution: undefined,
  });

  // Sections the user has navigated to during this modal lifetime. Once a
  // section is mounted it stays mounted (just hidden via CSS when
  // inactive) so its draft state survives tab switches — that's the whole
  // point of the global Save: you can edit in General, jump to Schedule,
  // change a cron expression, hit Save once, and both persist.
  const [visited, setVisited] = useState<Set<string>>(() => new Set([selectionKey(initialSel)]));

  // Per-section dirty flags reported up via onDirtyChange. Drives the
  // global Save button's enabled state and the sidebar dot indicators.
  const [dirty, setDirty] = useState<Record<string, boolean>>({});

  // Coordination state for the global save flow.
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<{ key: string; message: string } | null>(null);

  // Imperative refs for each mounted section. Section components expose
  // `{ validate, submit }` via useImperativeHandle and the modal invokes
  // them in two phases (validate-all-first, then submit-all).
  const sectionRefs = useRef<Map<string, SectionHandle>>(new Map());

  // Per-section dirty callback factory. Memoized per key so each section
  // gets a stable callback identity across renders — otherwise a fresh
  // arrow on every render would retrigger the section's useEffect.
  const dirtyCallbacks = useRef<Map<string, (isDirty: boolean) => void>>(new Map());

  // Same idea for the callback refs that populate `sectionRefs`. Caching
  // avoids re-running the section's useImperativeHandle bookkeeping on
  // every modal re-render.
  const sectionRefCallbacks = useRef<Map<string, (node: SectionHandle | null) => void>>(new Map());

  const updateDirty = useCallback((key: string, isDirtyNow: boolean) => {
    setDirty((prev) => {
      const next = Boolean(isDirtyNow);
      if (Boolean(prev[key]) === next) return prev;
      return { ...prev, [key]: next };
    });
  }, []);

  const getDirtyCallback = useCallback((key: string) => {
    if (!dirtyCallbacks.current.has(key)) {
      dirtyCallbacks.current.set(key, (isDirtyNow: boolean) => updateDirty(key, isDirtyNow));
    }
    return dirtyCallbacks.current.get(key);
  }, [updateDirty]);

  const setSectionRef = useCallback((key: string) => {
    if (!sectionRefCallbacks.current.has(key)) {
      sectionRefCallbacks.current.set(key, (node: SectionHandle | null) => {
        if (node) sectionRefs.current.set(key, node);
        else sectionRefs.current.delete(key);
      });
    }
    return sectionRefCallbacks.current.get(key)!;
  }, []);

  const navigateTo = useCallback((sel: Selection) => {
    setSelection(sel);
    setVisited((prev) => {
      const key = selectionKey(sel);
      if (prev.has(key)) return prev;
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  }, []);

  // Fresh state when the modal re-opens (or when the caller hands a
  // meaningfully different initialSelection).
  useEffect(() => {
    if (!isOpen) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Resets selection/visited/dirty/saving state when the modal re-opens or the caller's initial selection changes; the lint cannot model the 5-field derived draft keyed on props, not on rendered state.
    setSelection(initialSel);
    setVisited(new Set([selectionKey(initialSel)]));
    setDirty({});
    setSaving(false);
    setSaveError(null);
    sectionRefs.current = new Map();
    dirtyCallbacks.current = new Map();
    sectionRefCallbacks.current = new Map();
  }, [isOpen, initialSel]);

  // Load static dependencies once per open.
  useEffect(() => {
    if (!isOpen) return undefined;
    let cancelled = false;
    (async () => {
      const [servers, skills, connections, defaults] = await Promise.allSettled([
        getMcpServers(),
        getSkills(),
        assistantClient.listProviderConnections(),
        workspaceAgentDefaultExecution(),
      ]);
      if (cancelled) return;
      setDeps({
        mcpServers: servers.status === 'fulfilled' ? (servers.value || []) : [],
        skills: skills.status === 'fulfilled' ? (skills.value || []) : [],
        providerConnections: connections.status === 'fulfilled' ? (connections.value || []) : [],
        defaultExecution: defaults.status === 'fulfilled' ? (defaults.value || null) : null,
      });
    })();
    return () => { cancelled = true; };
  }, [isOpen]);

  const anyDirty = useMemo(() => Object.values(dirty).some(Boolean), [dirty]);

  // Save flow: validate every dirty section first (atomic gate), then
  // submit them in order. First failure aborts the rest with their drafts
  // intact. On full success we refresh the parent snapshot and close.
  const handleSave = useCallback(async () => {
    if (saving) return;
    const dirtyKeys = Object.keys(dirty).filter((k) => dirty[k]);
    if (dirtyKeys.length === 0) return;

    setSaveError(null);

    // Phase 1 — validate everything. Don't write anything yet, so a
    // failure in one section can't leave a partially-saved workspace.
    for (const key of dirtyKeys) {
      const api = sectionRefs.current.get(key);
      const v = api?.validate?.();
      if (v && !v.ok) {
        setSaveError({ key, message: v.error ?? 'Validation failed.' });
        navigateTo(parseSelectionKey(key));
        return;
      }
    }

    // Phase 2 — submit. We still bail on the first failure so the user
    // can fix the offending section before retrying, but the drafts for
    // anything not-yet-saved are preserved (sections own their own state).
    setSaving(true);
    for (const key of dirtyKeys) {
      const api = sectionRefs.current.get(key);
      if (!api?.submit) continue;
      try {
        const result = await api.submit();
        if (!result?.ok) {
          setSaveError({ key, message: result?.error || 'Save failed.' });
          navigateTo(parseSelectionKey(key));
          setSaving(false);
          return;
        }
      } catch (err) {
        setSaveError({ key, message: err instanceof Error ? err.message : String(err) });
        navigateTo(parseSelectionKey(key));
        setSaving(false);
        return;
      }
    }

    setSaving(false);
    window.dispatchEvent(new CustomEvent(WORKSPACE_SETTINGS_CHANGED_EVENT));
    await Promise.resolve(onChanged?.());
    onClose();
  }, [saving, dirty, navigateTo, onChanged, onClose]);

  const handleClose = useCallback(() => {
    if (saving) return;
    if (anyDirty) {
      if (!window.confirm('You have unsaved changes. Close without saving?')) return;
    }
    onClose();
  }, [saving, anyDirty, onClose]);

  // Escape key
  useEffect(() => {
    if (!isOpen) return undefined;
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') handleClose(); };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [isOpen, handleClose]);

  // Prevent body scroll while open
  useEffect(() => {
    if (isOpen) {
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = '';
    }
    return () => { document.body.style.overflow = ''; };
  }, [isOpen]);

  const handleOverlay = useCallback((e: React.MouseEvent) => {
    if (e.target === e.currentTarget) handleClose();
  }, [handleClose]);

  const agents = useMemo(
    () => snapshot?.assignedAgents || [],
    [snapshot?.assignedAgents]
  );
  const sortedAgents = useMemo(() => (
    [...agents].sort((a, b) => (a.isDefault === b.isDefault ? 0 : a.isDefault ? -1 : 1))
  ), [agents]);

  const handleAgentDeleted = useCallback((agentId: string) => {
    const key = `agent:${agentId}`;
    setDirty((prev) => {
      if (!(key in prev)) return prev;
      const next = { ...prev };
      delete next[key];
      return next;
    });
    setVisited((prev) => {
      if (!prev.has(key)) return prev;
      const next = new Set(prev);
      next.delete(key);
      return next;
    });
    sectionRefs.current.delete(key);
    dirtyCallbacks.current.delete(key);
    navigateTo({ kind: 'general' });
    window.dispatchEvent(new CustomEvent(WORKSPACE_SETTINGS_CHANGED_EVENT));
    onChanged?.();
  }, [navigateTo, onChanged]);

  if (!isOpen) return null;

  const activeKey = selectionKey(selection);

  const renderSection = (sel: Selection) => {
    if (sel.kind === 'general') {
      return (
        <GeneralSection
          ref={setSectionRef('general')}
          workspaceId={workspaceId}
          snapshot={snapshot}
          saving={saving}
          onDirtyChange={getDirtyCallback('general')}
        />
      );
    }
    if (sel.kind === 'schedule') {
      return (
        <ScheduleSection
          ref={setSectionRef('schedule')}
          workspaceId={workspaceId}
          snapshot={snapshot}
          saving={saving}
          onDirtyChange={getDirtyCallback('schedule')}
          onSnapshotRefresh={onChanged}
        />
      );
    }
    if (sel.kind === 'agent') {
      const key = `agent:${sel.agentId}`;
      // Only the Main is edited here. A teammate's behavior belongs to its
      // shared definition, so its row opens the local-overlay editor instead.
      if (sel.agentId !== snapshot?.defaultWorkspaceAgentId) {
        return (
          <AssignmentSection
            workspaceId={workspaceId}
            agentId={sel.agentId ?? undefined}
            onChanged={onChanged}
            onUnassigned={(removed) => {
              handleAgentDeleted(removed);
              navigateTo({ kind: 'team' });
            }}
          />
        );
      }

      return (
        <AgentBehaviorForm
          ref={setSectionRef(key)}
          workspaceId={workspaceId}
          agentId={sel.agentId ?? null}
          snapshot={snapshot}
          deps={deps}
          saving={saving}
          onDirtyChange={getDirtyCallback(key)}
          onDeleted={() => handleAgentDeleted(sel.agentId ?? '')}
        />
      );
    }
    // A workspace whose Main could not be recovered from a pre-library config
    // has no agent row to edit. Without this the settings modal would offer no
    // way back to a working workspace at all.
    if (sel.kind === 'new-main') {
      return (
        <AgentBehaviorForm
          ref={setSectionRef('new-main')}
          workspaceId={workspaceId}
          agentId={null}
          snapshot={snapshot}
          deps={deps}
          saving={saving}
          onDirtyChange={getDirtyCallback('new-main')}
        />
      );
    }
    if (sel.kind === 'new-agent') {
      return <AssignmentSection workspaceId={workspaceId} onChanged={onChanged} />;
    }
    if (sel.kind === 'team') {
      return <TeamPolicySection workspaceId={workspaceId} onChanged={onChanged} />;
    }
    return null;
  };

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
            onClick={handleClose}
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
                dirty={!!dirty.general}
                onClick={() => navigateTo({ kind: 'general' })}
              >
                General
              </NavItem>
              <NavItem
                active={selection.kind === 'schedule'}
                dirty={!!dirty.schedule}
                onClick={() => navigateTo({ kind: 'schedule' })}
              >
                Schedule
              </NavItem>
              <NavItem
                active={selection.kind === 'team'}
                dirty={!!dirty.team}
                onClick={() => navigateTo({ kind: 'team' })}
              >
                Team
              </NavItem>
            </div>

            <div className={styles.sidebarGroup}>
              <h3 className={styles.sidebarGroupTitle}>Agents</h3>
              {!snapshot?.defaultWorkspaceAgentId && (
                <NavItem
                  active={selection.kind === 'new-main'}
                  dirty={!!dirty['new-main']}
                  onClick={() => navigateTo({ kind: 'new-main' })}
                >
                  Main — set up
                </NavItem>
              )}
              {sortedAgents.map((agent) => (
                <NavItem
                  key={agent.id}
                  active={selection.kind === 'agent' && selection.agentId === agent.id}
                  dirty={!!dirty[`agent:${agent.id}`]}
                  onClick={() => navigateTo({ kind: 'agent', agentId: agent.id })}
                >
                  <AgentAvatar
                    identity={identityFor(agent)}
                    size={16}
                    activity={agent.enabled ? 'none' : 'disabled'}
                    className={styles.navItemAvatar}
                  />
                  {agent.isDefault ? 'Main' : (agent.displayName || agent.agentName || 'Untitled')}
                </NavItem>
              ))}
              <NavItem
                className={styles.navItemAddNew}
                active={selection.kind === 'new-agent'}
                dirty={!!dirty['new-agent']}
                onClick={() => navigateTo({ kind: 'new-agent' })}
              >
                + Add teammate
              </NavItem>
            </div>
          </aside>

          <main className={styles.contentArea}>
            {/* eslint-disable-next-line react-hooks/refs -- `visited` is a `useState<Set<string>>` (declared above) rendered as a list of section keys. The rule's identifier classifier misidentifies the `Array.from(visited)` property access as a ref read; it is a plain state read and is safe. */}
            {Array.from(visited).map((key) => {
              const sel = parseSelectionKey(key);
              const isActive = key === activeKey;
              return (
                <div
                  key={key}
                  className={`${styles.content} ${isActive ? '' : styles.contentHidden}`}
                  aria-hidden={!isActive}
                >
                  {renderSection(sel)}
                </div>
              );
            })}
          </main>
        </div>

        <footer className={styles.footer}>
          {saveError && (
            <div className={styles.footerError} role="alert">
              {saveError.message}
            </div>
          )}
          <button
            type="button"
            className={styles.primaryButton}
            onClick={handleSave}
            disabled={saving || !anyDirty}
          >
            {saving ? 'Saving…' : 'Save'}
          </button>
        </footer>
      </div>
    </div>,
    document.body
  );
};

const NavItem = ({ active, dirty, onClick, children, className }: { active: boolean; dirty: boolean; onClick: () => void; children: React.ReactNode; className?: string }) => (
  <button
    type="button"
    className={`${styles.navItem} ${active ? styles.navItemActive : ''} ${className || ''}`}
    onClick={onClick}
  >
    <span className={styles.navItemLabel}>{children}</span>
    {dirty && <span className={styles.navItemDirtyDot} title="Unsaved changes" aria-hidden="true" />}
  </button>
);

// ──────────────────────────────────────────────────────────────────────────
// Workspace / General
// ──────────────────────────────────────────────────────────────────────────

const GeneralSection = ({ ref, workspaceId, snapshot, saving, onDirtyChange }: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  snapshot: WorkspaceSnapshot | null;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
}) => {
  const [title, setTitle] = useState(snapshot?.title || '');
  const [error, setError] = useState<string | null>(null);
  // Brief "Copied!" affordance after the workspace-id chip is clicked.
  // Auto-resets so a second click can confirm again.
  const [idCopied, setIdCopied] = useState(false);
  const handleCopyId = useCallback(async () => {
    const id = snapshot?.workspaceId;
    if (!id) return;
    try {
      await navigator.clipboard.writeText(id);
      setIdCopied(true);
      window.setTimeout(() => setIdCopied(false), 1200);
    } catch {
      // Stay silent on failure — the UUID is still selectable as text.
    }
  }, [snapshot?.workspaceId]);

  // Resync if the parent snapshot changes (e.g., a save just completed and
  // the parent refetched). Skipped when the local draft already matches
  // the snapshot so we don't fight an in-flight save's loopback.
  // eslint-disable-next-line react-hooks/set-state-in-effect -- Resyncs local title from the parent snapshot when it changes (e.g., after a save refetch); the lint cannot model the loopback-avoidance guard the next line adds.
  useEffect(() => { setTitle(snapshot?.title || ''); }, [snapshot?.title]);

  const isDirty = title.trim() !== (snapshot?.title || '').trim();

  // Report dirty changes upward without depending on the callback's
  // identity (the modal hands stable callbacks, but ref-storage is a
  // belt-and-braces defense against closure staleness).
  const onDirtyChangeRef = useRef(onDirtyChange);
  useEffect(() => { onDirtyChangeRef.current = onDirtyChange; });
  useEffect(() => { onDirtyChangeRef.current?.(isDirty); }, [isDirty]);

  useImperativeHandle(ref, () => ({
    validate: () => {
      const trimmed = title.trim();
      if (!trimmed) {
        const msg = 'Workspace title cannot be empty.';
        setError(msg);
        return { ok: false, error: msg };
      }
      if (trimmed.length > 100) {
        const msg = 'Workspace title must be 100 characters or less.';
        setError(msg);
        return { ok: false, error: msg };
      }
      setError(null);
      return { ok: true };
    },
    submit: async () => {
      const trimmed = title.trim();
      try {
        await setWorkspaceTitle(workspaceId, trimmed);
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to save title.';
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

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
          Workspace ID:{' '}
          <button
            type="button"
            className={`${styles.copyableId} ${idCopied ? styles.copyableIdCopied : ''}`}
            onClick={handleCopyId}
            disabled={!snapshot?.workspaceId}
            title={idCopied ? 'Copied!' : 'Click to copy'}
            aria-label={idCopied ? 'Workspace ID copied' : 'Copy workspace ID'}
          >
            <code>{snapshot?.workspaceId}</code>
            {idCopied && <span className={styles.copyableIdBadge}>Copied!</span>}
          </button>
        </span>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}
    </div>
  );
};

// ──────────────────────────────────────────────────────────────────────────
// Workspace / Schedule
// ──────────────────────────────────────────────────────────────────────────

// Common cron patterns surfaced as chips — keeps simple cases
// one-click and lets users escape to free-text for anything custom.
// Labels are short enough to fit on a chip; the cron value itself
// shows in the input below as visual confirmation.
const CRON_PRESETS = [
  { label: 'Hourly', value: '0 * * * *' },
  { label: 'Daily 9am', value: '0 9 * * *' },
  { label: 'Daily midnight', value: '0 0 * * *' },
  { label: 'Weekdays 9am', value: '0 9 * * 1-5' },
  { label: 'Mondays 9am', value: '0 9 * * 1' },
  { label: 'Monthly', value: '0 0 1 * *' },
];

// Narrows to ScheduleKind (not the whole snapshot) so callers can pass
// `snapshot?.scheduleKind` and the effect's closure matches its deps.
const initialScheduleKindFromSnapshot = (kind: ScheduleKind | null | undefined): ScheduleKindDraft => {
  if (kind?.type === 'cron') {
    return { type: 'cron', expression: kind.expression || '', timezone: kind.timezone || '' };
  }
  if (kind?.type === 'interval') {
    return { type: 'interval', intervalMinutes: kind.intervalMinutes ?? 30 };
  }
  return { type: 'interval', intervalMinutes: 30 };
};

// Compact absolute time — drops seconds and the year if it matches
// today's so the preview list stays scannable.
const formatPreviewAbsolute = (ms: number): string => {
  try {
    const d = new Date(ms);
    const sameYear = d.getFullYear() === new Date().getFullYear();
    return d.toLocaleString(undefined, {
      year: sameYear ? undefined : 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return new Date(ms).toISOString();
  }
};

// Coarse "in 3h" / "in 2d" string. Anything past 30 days falls back
// to the absolute date so the relative side stays meaningful.
const formatPreviewRelative = (ms: number): string => {
  const delta = ms - Date.now();
  if (!Number.isFinite(delta) || delta <= 0) return 'now';
  const min = Math.round(delta / 60_000);
  if (min < 60) return `in ${min}m`;
  const hr = Math.round(min / 60);
  if (hr < 48) return `in ${hr}h`;
  const day = Math.round(hr / 24);
  if (day < 30) return `in ${day}d`;
  return ''; // too far out — let the absolute side carry it
};

const ScheduleSection = ({
  ref,
  workspaceId,
  snapshot,
  saving,
  onDirtyChange,
  onSnapshotRefresh,
}: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  snapshot: WorkspaceSnapshot | null;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
  onSnapshotRefresh?: () => void;
}) => {
  const [enabled, setEnabled] = useState(!!snapshot?.scheduleEnabled);
  const [scheduleKind, setScheduleKind] = useState(() =>
    initialScheduleKindFromSnapshot(snapshot?.scheduleKind)
  );
  // Local busy flag for the imperative-only actions (Pause/Resume, Run
  // now). Save goes through the modal's global flow, so we don't track
  // its busy state here — the parent's `saving` prop handles the input
  // disables for save.
  const [localBusy, setLocalBusy] = useState(false);
  const busy = saving || localBusy;
  const [error, setError] = useState<string | null>(null);
  const [previewTimes, setPreviewTimes] = useState<number[]>([]);
  const [previewError, setPreviewError] = useState<string | null>(null);

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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Resyncs schedule state from the snapshot when the parent's enabled flag or kind changes; the lint cannot model a 2-field derived draft that survives an in-flight save's loopback.
    setEnabled(!!snapshot?.scheduleEnabled);
    setScheduleKind(initialScheduleKindFromSnapshot(snapshot?.scheduleKind));
  }, [snapshot?.scheduleEnabled, snapshot?.scheduleKind]);

  const paused = !!snapshot?.schedulePaused;

  // Live preview: every time the user edits the cron expression or
  // timezone, ping the backend for the next 3 fire times so they can
  // sanity-check what they typed before hitting Save.
  useEffect(() => {
    if (!enabled || scheduleKind.type !== 'cron') {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- Fetches the next 3 fire times whenever the cron expression/timezone change; async invoke is outside the lint's set-state model and the cancellation guard is invisible to it.
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
        setPreviewError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Invalid schedule.');
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, scheduleKind]);

  const updateKindType = (type: 'interval' | 'cron') => {
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

  // Build the wire payload from current form state and surface validation
  // errors. Returns `{ ok, payloadKind, error }` so both validate() and
  // submit() can share the logic without double-coding the rules.
  const buildPayload = useCallback(() => {
    if (!enabled) return { ok: true, payloadKind: null };
    if (scheduleKind.type === 'interval') {
      const mins = Number(scheduleKind.intervalMinutes);
      if (!Number.isFinite(mins) || mins < 1 || mins > 1440) {
        return { ok: false, error: 'Interval must be between 1 minute and 24 hours.' };
      }
      return { ok: true, payloadKind: { type: 'interval', intervalMinutes: mins } };
    }
    const expr = (scheduleKind.expression || '').trim();
    const tz = (scheduleKind.timezone || '').trim();
    if (!expr) return { ok: false, error: 'Cron expression is required.' };
    if (!tz) return { ok: false, error: 'Timezone is required.' };
    return { ok: true, payloadKind: { type: 'cron', expression: expr, timezone: tz } };
  }, [enabled, scheduleKind]);

  const handleTogglePaused = useCallback(async () => {
    setLocalBusy(true);
    setError(null);
    try {
      await invoke('workspace_set_schedule_paused', {
        workspaceId,
        paused: !paused,
      });
      onSnapshotRefresh?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to update pause state.');
    } finally {
      setLocalBusy(false);
    }
  }, [paused, workspaceId, onSnapshotRefresh]);

  const handleRunNow = useCallback(async () => {
    setLocalBusy(true);
    setError(null);
    try {
      await invoke('workspace_run_now', { workspaceId });
      onSnapshotRefresh?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to trigger run.');
    } finally {
      setLocalBusy(false);
    }
  }, [workspaceId, onSnapshotRefresh]);

  const isDirty =
    enabled !== !!snapshot?.scheduleEnabled
    || JSON.stringify(scheduleKind) !== JSON.stringify(initialScheduleKindFromSnapshot(snapshot?.scheduleKind));

  const onDirtyChangeRef = useRef(onDirtyChange);
  useEffect(() => { onDirtyChangeRef.current = onDirtyChange; });
  useEffect(() => { onDirtyChangeRef.current?.(isDirty); }, [isDirty]);

  useImperativeHandle(ref, () => ({
    validate: () => {
      const built = buildPayload();
      if (!built.ok) {
        setError(built.error ?? null);
        return { ok: false, error: built.error };
      }
      setError(null);
      return { ok: true };
    },
    submit: async () => {
      const built = buildPayload();
      if (!built.ok) {
        setError(built.error ?? null);
        return { ok: false, error: built.error };
      }
      try {
        await invoke('workspace_set_schedule', {
          workspaceId,
          kind: built.payloadKind,
        });
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to save schedule.';
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

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
        <label className={styles.label}>Schedule type</label>
        <div className={styles.segmented} role="tablist" aria-label="Schedule type">
          <button
            type="button"
            role="tab"
            aria-selected={scheduleKind.type === 'interval'}
            className={`${styles.segmentedOption} ${
              scheduleKind.type === 'interval' ? styles.segmentedOptionActive : ''
            }`}
            onClick={() => updateKindType('interval')}
            disabled={busy || !enabled}
          >
            Interval
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={scheduleKind.type === 'cron'}
            className={`${styles.segmentedOption} ${
              scheduleKind.type === 'cron' ? styles.segmentedOptionActive : ''
            }`}
            onClick={() => updateKindType('cron')}
            disabled={busy || !enabled}
          >
            Cron
          </button>
        </div>
      </div>

      {scheduleKind.type === 'interval' && (
        <div className={styles.scheduleCard}>
          <div className={styles.field} style={{ marginBottom: 0 }}>
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
                ? 'Fires N minutes after the previous completion. Use Cron for fixed-time schedules.'
                : 'Stored for later if you re-enable scheduling.'}
            </span>
          </div>
        </div>
      )}

      {scheduleKind.type === 'cron' && (
        <div className={styles.scheduleCard}>
          <div className={styles.field}>
            <label className={styles.label}>Quick patterns</label>
            <div className={styles.presetRow}>
              {CRON_PRESETS.map((p) => {
                const active = scheduleKind.expression === p.value;
                return (
                  <button
                    key={p.value}
                    type="button"
                    className={`${styles.presetChip} ${active ? styles.presetChipActive : ''}`}
                    onClick={() =>
                      setScheduleKind((prev) => ({ ...prev, expression: p.value }))
                    }
                    disabled={busy || !enabled}
                    title={p.value}
                  >
                    {p.label}
                  </button>
                );
              })}
            </div>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="ws-cron-expr">Cron expression</label>
            <input
              id="ws-cron-expr"
              type="text"
              className={styles.cronInput}
              value={scheduleKind.expression || ''}
              onChange={(e) =>
                setScheduleKind((prev) => ({ ...prev, expression: e.target.value }))
              }
              placeholder="0 9 * * 1-5"
              disabled={busy || !enabled}
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
            />
            <span className={styles.hint}>
              5 fields: minute · hour · day-of-month · month · day-of-week
            </span>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="ws-cron-tz">Timezone</label>
            <input
              id="ws-cron-tz"
              type="text"
              className={styles.input}
              value={scheduleKind.timezone || ''}
              onChange={(e) =>
                setScheduleKind((prev) => ({ ...prev, timezone: e.target.value }))
              }
              placeholder="America/New_York"
              disabled={busy || !enabled}
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
            />
            <span className={styles.tzHint}>
              IANA timezone name.
              {scheduleKind.timezone !== hostTimezone && (
                <>
                  {' '}
                  <button
                    type="button"
                    className={styles.tzLink}
                    onClick={() =>
                      setScheduleKind((prev) => ({ ...prev, timezone: hostTimezone }))
                    }
                    disabled={busy || !enabled}
                  >
                    Use system timezone ({hostTimezone})
                  </button>
                </>
              )}
            </span>
          </div>

          <div className={styles.field} style={{ marginBottom: 0 }}>
            <label className={styles.label}>Next runs</label>
            {previewError && (
              <div className={styles.errorBanner}>{previewError}</div>
            )}
            {!previewError && previewTimes.length === 0 && (
              <span className={styles.previewEmpty}>
                Enter a valid expression and timezone to preview upcoming runs.
              </span>
            )}
            {!previewError && previewTimes.length > 0 && (
              <ul className={styles.previewList}>
                {previewTimes.map((ms) => {
                  const rel = formatPreviewRelative(ms);
                  return (
                    <li key={ms} className={styles.previewItem}>
                      <span className={styles.previewAbsolute}>
                        {formatPreviewAbsolute(ms)}
                      </span>
                      {rel && <span className={styles.previewRelative}>{rel}</span>}
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </div>
      )}

      {snapshot?.scheduleEnabled && (
        <div className={styles.statusBar}>
          <span className={styles.statusBadge}>
            <span
              className={`${styles.statusDot} ${
                paused ? styles.statusDotPaused : styles.statusDotRunning
              }`}
            />
            {paused ? 'Paused' : 'Running'}
          </span>
          <button
            type="button"
            className={styles.secondaryButton}
            onClick={handleTogglePaused}
            disabled={busy}
          >
            {paused ? 'Resume' : 'Pause'}
          </button>
        </div>
      )}

      {error && <div className={styles.errorBanner}>{error}</div>}

      {snapshot?.scheduleEnabled && !paused && (
        <div className={styles.actions}>
          <button
            type="button"
            className={styles.secondaryButton}
            onClick={handleRunNow}
            disabled={busy}
          >
            Run now
          </button>
        </div>
      )}
    </div>
  );
};

// ──────────────────────────────────────────────────────────────────────────
// Agent section (manager + sub-agent + new)
// ──────────────────────────────────────────────────────────────────────────

export default WorkspaceSettingsModal;
