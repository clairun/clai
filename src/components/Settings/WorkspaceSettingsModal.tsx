/**
 * WorkspaceSettingsModal
 *
 * Near-full-window overlay with a drill-down: the root lists the sections
 * (Workspace: General, Schedule, Team; Agents: one row per agent, + Add to
 * crew) and entering one gives it the whole pane. Each page saves on its
 * own through the sticky bar; leaving a page with unsaved changes asks
 * first. Escape goes back one level and closes only at the root.
 */

import React, { useState, useEffect, useCallback, useImperativeHandle, useMemo, useRef } from 'react';
import ReactDOM from 'react-dom';
import { invoke } from '@tauri-apps/api/core';
import { getMcpServers, getSkills, workspaceAgentDefaultExecution } from '../../api/client';
import { assistantClient } from '../../assistant';
import { setWorkspaceTitle } from '../../workspace/client';
import IntervalSelect from './IntervalSelect';
import { AssignmentSection, TeamPolicySection } from './WorkspaceTeamSettings';
import { AgentBehaviorForm, type AgentFormDeps } from './AgentBehaviorForm';
import type { SectionHandle } from './sectionHandle';
import type { ScheduleKind, WorkspaceAgentResponse, WorkspaceDetails } from '../../generated/bindings';
import { useOverlayLayer } from '../../hooks/useOverlayLayer';
import AgentAvatar from '../Agents/AgentAvatar';
import { identityFor } from '../Agents/agentIdentity';
import styles from './WorkspaceSettingsModal.module.css';

// ──────────────────────────────────────────────────────────────────────────
// Local shapes. Agent payload types live with the form in
// AgentBehaviorForm.tsx; only the modal's own navigation state is here.
// ──────────────────────────────────────────────────────────────────────────

type PageKind = 'general' | 'schedule' | 'team' | 'agent' | 'new-main' | 'new-agent';
interface Selection {
  kind: PageKind;
  agentId?: string | null;
}

type ScheduleKindDraft =
  | { type: 'interval'; intervalMinutes: number }
  | { type: 'cron'; expression: string; timezone: string };

// Identifies a root row, so focus can return to it after going back.
const pageKey = (sel: Selection): string => (
  sel.kind === 'agent' ? `agent:${sel.agentId}` : sel.kind
);

const agentLabel = (agent: WorkspaceAgentResponse): string => (
  agent.isDefault ? 'Main' : (agent.displayName || agent.agentName || 'Untitled')
);

const plural = (n: number, word: string): string => `${n} ${word}${n === 1 ? '' : 's'}`;

const scheduleSummary = (details: WorkspaceDetails | null): string => {
  if (!details?.scheduleEnabled) return 'Off';
  const kind = details.scheduleKind;
  let when = 'On';
  if (kind?.type === 'cron') {
    when = `cron ${kind.expression}`;
  } else if (kind?.type === 'interval') {
    const mins = kind.intervalMinutes;
    when = mins % 60 === 0 ? `every ${mins / 60} h` : `every ${mins} min`;
  }
  return details.schedulePaused ? `${when} · paused` : when;
};

// ──────────────────────────────────────────────────────────────────────────
// Modal shell
// ──────────────────────────────────────────────────────────────────────────

interface WorkspaceSettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  workspaceId: string;
  details: WorkspaceDetails | null;
  /** Page to open on; the root index when omitted. */
  initialSelection?: Selection | null;
  onChanged?: () => void;
}

// Keep in sync with WorkspaceContextBar: the self-loading context bar
// listens for this to refetch its workspace details after a settings save.
const WORKSPACE_SETTINGS_CHANGED_EVENT = 'workspace-settings-changed';

const WorkspaceSettingsModal = ({
  isOpen,
  onClose,
  workspaceId,
  details,
  initialSelection,
  onChanged,
}: WorkspaceSettingsModalProps) => {
  // Structural compare via a stringified key — parents commonly pass
  // inline literals like `{ kind: 'agent', agentId }`, which have fresh JS
  // identity every render. A pure reference dep would snap the modal
  // back to the initial page on every parent re-render.
  const initialSelectionKey = JSON.stringify(initialSelection ?? null);
  const initialPage = useMemo<Selection | null>(
    () => JSON.parse(initialSelectionKey),
    [initialSelectionKey],
  );

  // null = the root index.
  const [page, setPage] = useState<Selection | null>(initialPage);
  // Bumped to remount the page: that is how Discard drops its draft.
  const [pageVersion, setPageVersion] = useState(0);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Set while the unsaved-changes prompt is up: where to go once answered.
  const [pendingLeave, setPendingLeave] = useState<(() => void) | null>(null);
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

  // Only the visible page is ever driven through its handle.
  const pageRef = useRef<SectionHandle>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const indexRef = useRef<HTMLDivElement>(null);
  // The root row the current page was entered from; focus returns to it.
  const originRow = useRef<string | null>(null);
  // What had focus when the leave prompt opened; Stay hands it back.
  const leaveFocus = useRef<HTMLElement | null>(null);

  const showPage = useCallback((next: Selection | null) => {
    setPage(next);
    setDirty(false);
    setSaveError(null);
    setPendingLeave(null);
  }, []);

  const openPage = useCallback((sel: Selection) => {
    originRow.current = pageKey(sel);
    showPage(sel);
  }, [showPage]);

  // Fresh state when the modal re-opens (or when the caller hands a
  // meaningfully different initialSelection).
  useEffect(() => {
    if (!isOpen) return;
    originRow.current = initialPage ? pageKey(initialPage) : null;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Resets navigation and save state when the modal re-opens or the caller's initial page changes; the state is keyed on props, not on rendered state.
    showPage(initialPage);
    setSaving(false);
  }, [isOpen, initialPage, showPage]);

  // Focus follows navigation: the page heading on entry, the originating
  // row on the way back, or the first row when there is none.
  useEffect(() => {
    if (!isOpen) return;
    if (page) {
      headingRef.current?.focus();
      return;
    }
    const index = indexRef.current;
    const origin = originRow.current
      && index?.querySelector<HTMLElement>(`[data-row="${originRow.current}"]`);
    (origin || index?.querySelector<HTMLElement>('[data-row]'))?.focus();
  }, [isOpen, page]);

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

  // Saves the visible page and stays on it. Resolves true on success.
  const savePage = useCallback(async () => {
    const handle = pageRef.current;
    if (!handle || saving) return false;
    const checked = handle.validate();
    if (!checked.ok) {
      setSaveError(checked.error ?? 'Validation failed.');
      return false;
    }
    setSaveError(null);
    setSaving(true);
    let createdId: string | undefined;
    try {
      const result = await handle.submit();
      if (!result.ok) {
        setSaveError(result.error || 'Save failed.');
        return false;
      }
      createdId = result.createdId;
    } catch (err) {
      setSaveError(err instanceof Error ? err.message : String(err));
      return false;
    } finally {
      setSaving(false);
    }
    // A create continues on the new record's own page before the refetch,
    // so a Save during it updates instead of creating another. Otherwise
    // the save bar unmounts with focus, so hand it to the heading.
    if (createdId) {
      const created: Selection = { kind: 'agent', agentId: createdId };
      originRow.current = pageKey(created);
      showPage(created);
    } else {
      headingRef.current?.focus();
    }
    window.dispatchEvent(new CustomEvent(WORKSPACE_SETTINGS_CHANGED_EVENT));
    await Promise.resolve(onChanged?.());
    return true;
  }, [saving, onChanged, showPage]);

  const discardPage = useCallback(() => {
    setPageVersion((v) => v + 1);
    setDirty(false);
    setSaveError(null);
    headingRef.current?.focus();
  }, []);

  const leave = useCallback((go: () => void) => {
    if (saving) return;
    if (!dirty) {
      go();
      return;
    }
    leaveFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setPendingLeave(() => go);
  }, [saving, dirty]);

  // Closes the prompt and puts focus back where it was before it opened.
  const stay = useCallback(() => {
    setPendingLeave(null);
    const back = leaveFocus.current;
    (back?.isConnected && back !== document.body ? back : headingRef.current)?.focus();
  }, []);

  const goBack = useCallback(() => leave(() => showPage(null)), [leave, showPage]);
  const handleClose = useCallback(() => leave(onClose), [leave, onClose]);

  const answerLeave = useCallback(async (answer: 'stay' | 'discard' | 'save') => {
    const go = pendingLeave;
    if (!go || answer === 'stay') {
      stay();
      return;
    }
    setPendingLeave(null);
    if (answer === 'save' && !(await savePage())) {
      stay();
      return;
    }
    go();
  }, [pendingLeave, savePage, stay]);

  // Escape: answer "Stay" to an open prompt, let a search field clear
  // itself first, then back one level, closing only at the root. It only
  // reaches this modal while nothing is stacked over it: the global
  // Settings modal opens on top of this one and takes the key first.
  const handleEscape = useCallback(() => {
    if (pendingLeave) {
      stay();
      return;
    }
    const focused = document.activeElement;
    if (focused instanceof HTMLInputElement && focused.type === 'search' && focused.value) return;
    if (page) goBack();
    else handleClose();
  }, [pendingLeave, page, goBack, handleClose, stay]);

  useOverlayLayer(isOpen, handleEscape);

  const handleOverlay = useCallback((e: React.MouseEvent) => {
    if (e.target === e.currentTarget) handleClose();
  }, [handleClose]);

  const agents = useMemo(
    () => details?.assignedAgents || [],
    [details?.assignedAgents]
  );
  const sortedAgents = useMemo(() => (
    [...agents].sort((a, b) => (a.isDefault === b.isDefault ? 0 : a.isDefault ? -1 : 1))
  ), [agents]);

  // The agent is gone, so there is nothing to keep or ask about.
  const handleAgentRemoved = useCallback(() => {
    originRow.current = null;
    showPage(null);
    window.dispatchEvent(new CustomEvent(WORKSPACE_SETTINGS_CHANGED_EVENT));
    onChanged?.();
  }, [showPage, onChanged]);

  if (!isOpen) return null;

  const agentSummary = (agent: WorkspaceAgentResponse): string => {
    const provider = deps.providerConnections.find((c) => c.id === agent.providerConnectionIds[0]);
    return [
      plural(agent.skillIds.length, 'skill'),
      `${agent.selectedMcpServerIds.length} MCP`,
      provider?.name,
      agent.enabled ? null : 'disabled',
    ].filter(Boolean).join(' · ');
  };

  const pageTitle = (sel: Selection): string => {
    switch (sel.kind) {
      case 'general': return 'General';
      case 'schedule': return 'Schedule';
      case 'team': return 'Team';
      case 'new-main': return 'Main — set up';
      case 'new-agent': return 'Add to crew';
      case 'agent': {
        const agent = agents.find((a) => a.id === sel.agentId);
        return agent ? agentLabel(agent) : 'Agent';
      }
    }
  };

  const renderPage = (sel: Selection) => {
    if (sel.kind === 'general') {
      return (
        <GeneralSection
          ref={pageRef}
          workspaceId={workspaceId}
          details={details}
          saving={saving}
          onDirtyChange={setDirty}
        />
      );
    }
    if (sel.kind === 'schedule') {
      return (
        <ScheduleSection
          ref={pageRef}
          workspaceId={workspaceId}
          details={details}
          saving={saving}
          onDirtyChange={setDirty}
          onDetailsRefresh={onChanged}
        />
      );
    }
    if (sel.kind === 'agent') {
      // Only the Main is edited here. A teammate's behavior belongs to its
      // shared definition, so its row opens the local-overlay editor instead.
      // An id missing from the roster is a Main just created whose refetch
      // has not landed (or failed): teammates are only opened from their row.
      const isTeammate = agents.find((a) => a.id === sel.agentId)?.isDefault === false;
      if (isTeammate) {
        return (
          <AssignmentSection
            ref={pageRef}
            workspaceId={workspaceId}
            agentId={sel.agentId ?? undefined}
            saving={saving}
            onDirtyChange={setDirty}
            onChanged={onChanged}
            onUnassigned={handleAgentRemoved}
          />
        );
      }

      return (
        <AgentBehaviorForm
          ref={pageRef}
          workspaceId={workspaceId}
          agentId={sel.agentId ?? null}
          details={details}
          deps={deps}
          saving={saving}
          onDirtyChange={setDirty}
          onDeleted={handleAgentRemoved}
        />
      );
    }
    // A workspace whose Main could not be recovered from a pre-library config
    // has no agent row to edit. Without this the settings modal would offer no
    // way back to a working workspace at all.
    if (sel.kind === 'new-main') {
      return (
        <AgentBehaviorForm
          ref={pageRef}
          workspaceId={workspaceId}
          agentId={null}
          details={details}
          deps={deps}
          saving={saving}
          onDirtyChange={setDirty}
        />
      );
    }
    if (sel.kind === 'new-agent') {
      return <AssignmentSection workspaceId={workspaceId} onChanged={onChanged} />;
    }
    if (sel.kind === 'team') {
      return (
        <TeamPolicySection ref={pageRef} workspaceId={workspaceId} saving={saving} onDirtyChange={setDirty} />
      );
    }
    return null;
  };

  const workspaceTitle = `${details?.title || 'Workspace'} — Settings`;
  const crewCount = agents.filter((a) => !a.isDefault).length;

  const index = (
    <div className={styles.index} ref={indexRef} onKeyDown={moveRowFocus}>
      <section className={styles.indexGroup} aria-label="Workspace">
        <h3 className={styles.indexGroupTitle}>Workspace</h3>
        <IndexRow rowKey="general" label="General" summary={details?.rootPath ?? ''} onOpen={() => openPage({ kind: 'general' })} />
        <IndexRow rowKey="schedule" label="Schedule" summary={scheduleSummary(details)} onOpen={() => openPage({ kind: 'schedule' })} />
        <IndexRow rowKey="team" label="Team" summary={crewCount ? `${crewCount} crew` : 'No crew'} onOpen={() => openPage({ kind: 'team' })} />
      </section>

      <section className={styles.indexGroup} aria-label="Agents">
        <h3 className={styles.indexGroupTitle}>Agents</h3>
        {!details?.defaultWorkspaceAgentId && (
          <IndexRow rowKey="new-main" label="Main — set up" summary="No main agent yet" onOpen={() => openPage({ kind: 'new-main' })} />
        )}
        {sortedAgents.map((agent) => (
          <IndexRow
            key={agent.id}
            rowKey={pageKey({ kind: 'agent', agentId: agent.id })}
            label={agentLabel(agent)}
            summary={agentSummary(agent)}
            icon={(
              <AgentAvatar
                identity={identityFor(agent)}
                size={20}
                activity={agent.enabled ? 'none' : 'disabled'}
              />
            )}
            onOpen={() => openPage({ kind: 'agent', agentId: agent.id })}
          />
        ))}
        <IndexRow rowKey="new-agent" label="+ Add to crew" summary="" onOpen={() => openPage({ kind: 'new-agent' })} />
      </section>
    </div>
  );

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={handleOverlay}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        <header className={styles.header}>
          {page ? (
            <nav className={styles.breadcrumb} aria-label="Breadcrumb">
              <button type="button" className={styles.crumbBack} onClick={goBack} disabled={saving}>
                ‹ {workspaceTitle}
              </button>
              <span className={styles.crumbSeparator} aria-hidden="true">›</span>
              <h2 className={styles.title} ref={headingRef} tabIndex={-1}>
                {pageTitle(page)}
              </h2>
            </nav>
          ) : (
            <h2 className={styles.title}>{workspaceTitle}</h2>
          )}
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

        {page ? (
          <div className={styles.page} key={`${pageKey(page)}:${pageVersion}`}>
            {renderPage(page)}
            {(dirty || pendingLeave || saveError) && (
              <SaveBar
                saving={saving}
                error={saveError}
                leaving={!!pendingLeave}
                onAnswer={answerLeave}
                onDiscard={discardPage}
                onSave={savePage}
              />
            )}
          </div>
        ) : index}
      </div>
    </div>,
    document.body
  );
};

// ↑/↓ between root rows; Tab still works as usual.
const moveRowFocus = (e: React.KeyboardEvent<HTMLDivElement>) => {
  if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
  const rows = Array.from(e.currentTarget.querySelectorAll<HTMLElement>('[data-row]'));
  const at = rows.indexOf(document.activeElement as HTMLElement);
  if (at < 0) return;
  e.preventDefault();
  const step = e.key === 'ArrowDown' ? 1 : rows.length - 1;
  rows[(at + step) % rows.length]?.focus();
};

const IndexRow = ({ rowKey, label, summary, icon, onOpen }: {
  rowKey: string;
  label: string;
  summary: string;
  icon?: React.ReactNode;
  onOpen: () => void;
}) => (
  <button type="button" data-row={rowKey} className={styles.indexRow} onClick={onOpen}>
    {icon}
    <span className={styles.indexRowLabel}>{label}</span>
    <span className={styles.indexRowSummary}>{summary}</span>
    <span className={styles.indexRowChevron} aria-hidden="true">›</span>
  </button>
);

// Shown at the foot of a page only while there is something to say: unsaved
// changes, a failed save, or the question asked when leaving a dirty page.
const SaveBar = ({ saving, error, leaving, onAnswer, onDiscard, onSave }: {
  saving: boolean;
  error: string | null;
  leaving: boolean;
  onAnswer: (answer: 'stay' | 'discard' | 'save') => void;
  onDiscard: () => void;
  onSave: () => void;
}) => (
  <div className={styles.saveBar} role="region" aria-label="Unsaved changes">
    {error ? (
      <span className={styles.saveBarError} role="alert">{error}</span>
    ) : (
      <span className={styles.saveBarText} role={leaving ? 'alert' : undefined}>
        {leaving ? 'Save your changes before leaving?' : 'Unsaved changes'}
      </span>
    )}
    {leaving ? (
      <>
        {/* Focused so Enter is the harmless answer. */}
        <button type="button" className={styles.secondaryButton} onClick={() => onAnswer('stay')} autoFocus>
          Stay
        </button>
        <button type="button" className={styles.secondaryButton} onClick={() => onAnswer('discard')}>
          Discard
        </button>
        <button type="button" className={styles.primaryButton} onClick={() => onAnswer('save')} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </>
    ) : (
      <>
        <button type="button" className={styles.secondaryButton} onClick={onDiscard} disabled={saving}>
          Discard
        </button>
        <button type="button" className={styles.primaryButton} onClick={onSave} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </>
    )}
  </div>
);

// ──────────────────────────────────────────────────────────────────────────
// Workspace / General
// ──────────────────────────────────────────────────────────────────────────

const GeneralSection = ({ ref, workspaceId, details, saving, onDirtyChange }: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  details: WorkspaceDetails | null;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
}) => {
  const [title, setTitle] = useState(details?.title || '');
  const [error, setError] = useState<string | null>(null);
  // Brief "Copied!" affordance after the workspace-id chip is clicked.
  // Auto-resets so a second click can confirm again.
  const [idCopied, setIdCopied] = useState(false);
  const handleCopyId = useCallback(async () => {
    const id = details?.workspaceId;
    if (!id) return;
    try {
      await navigator.clipboard.writeText(id);
      setIdCopied(true);
      window.setTimeout(() => setIdCopied(false), 1200);
    } catch {
      // Stay silent on failure — the UUID is still selectable as text.
    }
  }, [details?.workspaceId]);

  // Resync the draft when the parent's details title changes (e.g., a save
  // completed and the parent refetched). The dependency array is the whole
  // protection: a refetch that returns the same title does not re-run this,
  // so an in-flight save's loopback leaves the draft alone. There is no
  // draft-matching guard — a title that genuinely changed (a concurrent
  // rename) does overwrite an unsaved draft.
  // eslint-disable-next-line react-hooks/set-state-in-effect -- deliberate: the draft mirrors the parent's details title, and the `details?.title` dependency is what keeps a same-value refetch from clobbering it.
  useEffect(() => { setTitle(details?.title || ''); }, [details?.title]);

  const isDirty = title.trim() !== (details?.title || '').trim();

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
            disabled={!details?.workspaceId}
            title={idCopied ? 'Copied!' : 'Click to copy'}
            aria-label={idCopied ? 'Workspace ID copied' : 'Copy workspace ID'}
          >
            <code>{details?.workspaceId}</code>
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

// Narrows to ScheduleKind (not the whole details object) so callers can pass
// `details?.scheduleKind` and the effect's closure matches its deps.
const initialScheduleKindFromDetails = (kind: ScheduleKind | null | undefined): ScheduleKindDraft => {
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
  details,
  saving,
  onDirtyChange,
  onDetailsRefresh,
}: {
  ref: React.Ref<SectionHandle>;
  workspaceId: string;
  details: WorkspaceDetails | null;
  saving: boolean;
  onDirtyChange?: (isDirty: boolean) => void;
  onDetailsRefresh?: () => void;
}) => {
  const [enabled, setEnabled] = useState(!!details?.scheduleEnabled);
  const [scheduleKind, setScheduleKind] = useState(() =>
    initialScheduleKindFromDetails(details?.scheduleKind)
  );
  // Local busy flag for the imperative-only actions (Pause/Resume, Run
  // now). Save goes through the modal's save bar, so we don't track
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
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Resyncs schedule state from the details when the parent's enabled flag or kind changes; the lint cannot model a 2-field derived draft that survives an in-flight save's loopback.
    setEnabled(!!details?.scheduleEnabled);
    setScheduleKind(initialScheduleKindFromDetails(details?.scheduleKind));
  }, [details?.scheduleEnabled, details?.scheduleKind]);

  const paused = !!details?.schedulePaused;

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
      onDetailsRefresh?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to update pause state.');
    } finally {
      setLocalBusy(false);
    }
  }, [paused, workspaceId, onDetailsRefresh]);

  const handleRunNow = useCallback(async () => {
    setLocalBusy(true);
    setError(null);
    try {
      await invoke('workspace_run_now', { workspaceId });
      onDetailsRefresh?.();
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to trigger run.');
    } finally {
      setLocalBusy(false);
    }
  }, [workspaceId, onDetailsRefresh]);

  const isDirty =
    enabled !== !!details?.scheduleEnabled
    || JSON.stringify(scheduleKind) !== JSON.stringify(initialScheduleKindFromDetails(details?.scheduleKind));

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
                    className={styles.linkButton}
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

      {details?.scheduleEnabled && (
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

      {details?.scheduleEnabled && !paused && (
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

export default WorkspaceSettingsModal;
