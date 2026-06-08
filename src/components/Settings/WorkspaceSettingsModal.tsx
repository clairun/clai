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
import type { ProviderConnection, ScheduleKind, WorkspaceSnapshot } from '../../generated/bindings';
import styles from './WorkspaceSettingsModal.module.css';

// ──────────────────────────────────────────────────────────────────────────
// Local shapes. The execution-config tree and agent/template payloads are
// ad-hoc (sourced from untyped api/client.js commands); modeled loosely
// here rather than dragging the full config module into the FE types.
// ──────────────────────────────────────────────────────────────────────────

type SectionKind = 'general' | 'schedule' | 'agent' | 'new-agent';
interface Selection {
  kind: SectionKind;
  agentId?: string | null;
}

interface SectionResult {
  ok: boolean;
  error?: string;
}
interface SectionHandle {
  validate: () => SectionResult;
  submit: () => Promise<SectionResult>;
}

interface GrantOrigin {
  kind: string;
  grantedAtUnixMs?: number;
  reason?: string;
}
interface PathGrant {
  path: string;
  access: string;
  origin?: GrantOrigin | null;
}
interface ExecutionConfig {
  sandbox: { network: string; sessionBus: string };
  filesystem: { extraPaths: PathGrant[] };
  shell: { mode: string; allowedCommandPrefixes: string[]; blockedCommandPrefixes: string[] };
  web: { enabled: boolean };
}

type ScheduleKindDraft =
  | { type: 'interval'; intervalMinutes: number }
  | { type: 'cron'; expression: string; timezone: string };

interface NamedRef {
  id: string;
  name: string;
  description?: string | null;
}
interface AgentTemplate {
  id: string;
  name: string;
  description?: string | null;
  defaultSkillIds?: string[];
  defaultExecution?: Partial<ExecutionConfig>;
}
// Agent detail loaded from workspaceGetAgent (untyped command).
interface AgentDetail {
  id: string;
  name?: string;
  description?: string;
  isDefault?: boolean;
  enabled?: boolean;
  selectedSkillIds?: string[];
  selectedMcpServerIds?: string[];
  providerConnectionIds?: string[];
  execution?: Partial<ExecutionConfig>;
}

interface ModalDeps {
  mcpServers: NamedRef[];
  skills: NamedRef[];
  providerConnections: ProviderConnection[];
  agentTemplates: AgentTemplate[];
  defaultExecution: Partial<ExecutionConfig> | null | undefined;
}

// ──────────────────────────────────────────────────────────────────────────
// Shared helpers (carried over from AgentFormModal — will be the only copy
// once that file is deleted)
// ──────────────────────────────────────────────────────────────────────────

const defaultExecution = (): ExecutionConfig => ({
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

const normalizeItems = (items: string[] = []): string[] =>
  items.map((item) => item.trim()).filter(Boolean);

const addUniqueItem = (items: string[], value: string): string[] => {
  const trimmed = value.trim();
  if (!trimmed || items.includes(trimmed)) return items;
  return [...items, trimmed];
};

const normalizePathGrants = (items: PathGrant[] = []): PathGrant[] =>
  items
    .map((item) => ({
      path: item.path?.trim() || '',
      access: item.access || 'read_only',
      origin: item.origin || null,
    }))
    .filter((item) => item.path);

const grantOriginLabel = (origin: GrantOrigin | null | undefined): string => {
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

const normalizeExecution = (execution: Partial<ExecutionConfig> = {}): ExecutionConfig => {
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
interface AgentPayloadInput {
  name?: string;
  description?: string;
  selectedSkillIds?: string[];
  selectedMcpServerIds?: string[];
  providerConnectionIds?: string[];
  sessionBusAllowed?: boolean;
  extraPathGrants?: PathGrant[];
  shellMode?: string;
  allowedCommands?: string[];
  blockedCommands?: string[];
  webEnabled?: boolean;
  enabled?: boolean;
}

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
}: AgentPayloadInput): string => JSON.stringify({
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
  const [deps, setDeps] = useState<ModalDeps>({
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
        agentTemplates: templates.status === 'fulfilled' ? ((templates.value || []) as AgentTemplate[]) : [],
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
      return (
        <AgentSection
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
    if (sel.kind === 'new-agent') {
      return (
        <AgentSection
          ref={setSectionRef('new-agent')}
          workspaceId={workspaceId}
          agentId={null}
          snapshot={snapshot}
          deps={deps}
          saving={saving}
          onDirtyChange={getDirtyCallback('new-agent')}
        />
      );
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
            </div>

            <div className={styles.sidebarGroup}>
              <h3 className={styles.sidebarGroupTitle}>Agents</h3>
              {sortedAgents.map((agent) => (
                <NavItem
                  key={agent.id}
                  active={selection.kind === 'agent' && selection.agentId === agent.id}
                  dirty={!!dirty[`agent:${agent.id}`]}
                  onClick={() => navigateTo({ kind: 'agent', agentId: agent.id })}
                >
                  {agent.isDefault ? 'Main' : (agent.displayName || agent.agentName || 'Untitled')}
                </NavItem>
              ))}
              <NavItem
                className={styles.navItemAddNew}
                active={selection.kind === 'new-agent'}
                dirty={!!dirty['new-agent']}
                onClick={() => navigateTo({ kind: 'new-agent' })}
              >
                + Add agent
              </NavItem>
            </div>
          </aside>

          <main className={styles.contentArea}>
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
    setEnabled(!!snapshot?.scheduleEnabled);
    setScheduleKind(initialScheduleKindFromSnapshot(snapshot?.scheduleKind));
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

const AgentSection = ({
  ref,
  workspaceId,
  agentId,             // string for edit; null for create
  snapshot: _snapshot, // unused; kept in signature for future use (e.g., showing peer agents)
  deps,
  saving,              // global save in flight — disables inputs
  onDirtyChange,
  onDeleted,
}: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  agentId: string | null;
  snapshot: WorkspaceSnapshot | null;
  deps: ModalDeps;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
  onDeleted?: () => void;
}) => {
  const isCreate = !agentId;
  // Both flows start in a loading state: edit waits on `workspaceGetAgent`,
  // create waits on `deps.defaultExecution` so the form opens with the
  // backend's `$HOME` RO grant pre-populated instead of an empty list.
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Local busy flag for the Delete imperative action. Save goes through
  // the parent's `saving` prop.
  const [deleting, setDeleting] = useState(false);
  const busy = saving || deleting;

  // Source-of-truth agent payload (loaded for edit, blank draft for create
  // — populated once `deps.defaultExecution` arrives).
  const [agent, setAgent] = useState<AgentDetail | null>(null);
  const isManager = agent?.isDefault === true;
  const canDelete = !isCreate && !isManager;

  // Form state
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [selectedMcpServerIds, setSelectedMcpServerIds] = useState<string[]>([]);
  const [selectedSkillIds, setSelectedSkillIds] = useState<string[]>([]);
  const [providerConnectionIds, setProviderConnectionIds] = useState<string[]>([]);
  const [providerConnectionDraft, setProviderConnectionDraft] = useState('');
  const [extraPathGrants, setExtraPathGrants] = useState<PathGrant[]>([]);
  const [extraPathDraft, setExtraPathDraft] = useState('');
  const [extraPathAccess, setExtraPathAccess] = useState('read_only');
  const [sessionBusAllowed, setSessionBusAllowed] = useState(true);
  const [shellMode, setShellMode] = useState('off');
  const [allowedCommands, setAllowedCommands] = useState<string[]>([]);
  const [blockedCommands, setBlockedCommands] = useState(defaultExecution().shell.blockedCommandPrefixes);
  const [allowedCommandDraft, setAllowedCommandDraft] = useState('');
  const [blockedCommandDraft, setBlockedCommandDraft] = useState('');
  const [webEnabled, setWebEnabled] = useState(false);
  const [enabled, setEnabled] = useState(true);
  const [selectedTemplateId, setSelectedTemplateId] = useState('');

  // Track which agentId we last fetched, so the effect doesn't refetch on
  // every re-render. setting key={agentId} on the parent already remounts
  // but this is a belt-and-braces for future callers.
  const lastFetchedId = useRef<string | null>(null);

  // Baseline payload captured at load time. The Save button compares
  // current form state against this to decide whether anything is pending.
  // Updated after a successful save so Save re-disables until the user
  // edits again.
  const baselinePayloadRef = useRef<string | null>(null);

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
        setAgent(detail as unknown as AgentDetail);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load agent.');
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

  // Validation rules surfaced both as an imperative `validate()` and from
  // inside `submit()`. Keeping them in one place avoids drift when we add
  // a new rule.
  const validateAgent = useCallback(() => {
    const trimmedName = name.trim();
    if (!isManager && !trimmedName) {
      return { ok: false, error: 'Agent name is required.' };
    }
    if (trimmedName.length > 100) {
      return { ok: false, error: 'Name must be 100 characters or less.' };
    }
    if (providerConnectionIds.length === 0) {
      return { ok: false, error: 'Select at least one provider connection.' };
    }
    return { ok: true };
  }, [name, isManager, providerConnectionIds]);

  // Report dirty changes up to the modal so the global Save button and
  // the sidebar dot indicators reflect current state.
  const onDirtyChangeRef = useRef(onDirtyChange);
  useEffect(() => { onDirtyChangeRef.current = onDirtyChange; });
  useEffect(() => { onDirtyChangeRef.current?.(isDirty); }, [isDirty]);

  useImperativeHandle(ref, () => ({
    validate: () => {
      const v = validateAgent();
      if (!v.ok) setError(v.error ?? null);
      else setError(null);
      return v;
    },
    submit: async () => {
      const v = validateAgent();
      if (!v.ok) {
        setError(v.error ?? null);
        return v;
      }
      const trimmedName = name.trim();
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
      try {
        if (isCreate) {
          await workspaceCreateAgent({
            workspaceId,
            name: trimmedName,
            description: description.trim(),
            selectedSkillIds,
            selectedMcpServerIds,
            providerConnectionIds,
            execution,
            enabled,
          });
        } else {
          await workspaceUpdateAgent({
            workspaceId,
            agentId: agent?.id,
            name: isManager ? (agent?.name || 'Manager') : trimmedName,
            description: description.trim(),
            selectedSkillIds,
            selectedMcpServerIds,
            providerConnectionIds,
            execution,
            enabled: isManager ? true : enabled,
          });
          // Mark form clean: the values we just persisted are now the
          // baseline. isDirty flips false and reports up so the sidebar
          // dot clears even though the modal will close on full success.
          baselinePayloadRef.current = currentPayload;
        }
        setError(null);
        return { ok: true };
      } catch (err) {
        const message = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to save agent.';
        setError(message);
        return { ok: false, error: message };
      }
    },
  }));

  const handleDelete = useCallback(async () => {
    if (!canDelete || !agent?.id) return;
    if (!window.confirm(`Delete agent "${agent?.name}"? This cannot be undone.`)) return;
    setDeleting(true);
    setError(null);
    try {
      await workspaceDeleteAgent(workspaceId, agent.id);
      onDeleted?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to delete agent.');
    } finally {
      setDeleting(false);
    }
  }, [canDelete, agent, workspaceId, onDeleted]);

  if (loading) {
    return <div className={styles.sectionRoot}>Loading…</div>;
  }

  return (
    <div className={styles.sectionRoot}>
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
              disabled={busy}
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
            disabled={busy}
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
          disabled={busy}
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
                    disabled={busy}
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
                    disabled={busy}
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
                    disabled={busy}
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
              disabled={busy}
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
                  disabled={busy}
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
            disabled={busy}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); handleAddPathGrant(); } }}
          />
          <select
            className={styles.select}
            value={extraPathAccess}
            onChange={(e) => setExtraPathAccess(e.target.value)}
            disabled={busy}
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
          disabled={busy}
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
                      disabled={busy}
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
                disabled={busy}
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
                      disabled={busy}
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
                disabled={busy}
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
              disabled={busy}
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
              disabled={busy}
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
                disabled={busy}
              />
              <span className={styles.toggleTrack}>
                <span className={styles.toggleThumb} />
              </span>
            </span>
          </label>
        </div>
      )}

      {error && <div className={styles.errorBanner}>{error}</div>}

      {canDelete && (
        <div className={styles.actions}>
          <button
            type="button"
            className={styles.dangerButton}
            onClick={handleDelete}
            disabled={busy}
          >
            {deleting ? 'Deleting…' : 'Delete agent'}
          </button>
        </div>
      )}
    </div>
  );
};

// Initial draft used by the "Add agent" form. The backend-provided
// `defaultExecution` (carries the `$HOME` RO grant) is preferred so the
// user sees the granted paths up front. Falls back to the local empty
// execution if the fetch failed — better to show an empty list than to
// block the whole create flow.
const blankAgentDraft = (defaultExecutionFromBackend?: Partial<ExecutionConfig>): AgentDetail => ({
  id: '',
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
