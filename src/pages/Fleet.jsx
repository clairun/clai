import React, { useEffect, useMemo, useState, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { useChatManager } from '../contexts/ChatManagerContext';
import { useAssistantStore } from '../assistant';
import {
  listWorkspaces,
  deleteWorkspace,
  getWorkspaceSnapshot,
  runWorkspaceNow,
  setWorkspaceSchedulePaused,
} from '../workspace/client';
import ChatMessageList from '../components/AssistantChat/ChatMessageList';
import ConfirmDialog from '../components/ConfirmDialog';
import InlineApprovalCard from '../components/InlineApprovalCard';
import InlinePathGrantCard from '../components/InlinePathGrantCard';
import WorkspaceSettingsModal from '../components/Settings/WorkspaceSettingsModal';
import { useFleet } from '../contexts/FleetContext';
import { useFleetActivity } from '../hooks/useFleetActivity';
import { usePermissionAttention } from '../hooks/usePermissionAttention';
import styles from './Fleet.module.css';

const REFRESH_INTERVAL_MS = 5000;

const EMPTY_TOOL_CALLS = [];
const EMPTY_STREAMING = {};

const formatNextRun = (seconds) => {
  if (seconds == null) return '';
  if (seconds <= 0) return 'Due now';
  if (seconds < 60) return `Next run in ${seconds}s`;
  if (seconds < 3600) return `Next run in ${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `Next run in ${Math.floor(seconds / 3600)}h`;
  return `Next run in ${Math.floor(seconds / 86400)}d`;
};

const TASK_STATUS_LABEL = {
  blocked: 'Blocked',
  failed: 'Failed',
  needs_user_input: 'Needs input',
};

const CARD_STATUS_LABEL = {
  idle: 'Idle',
  running: 'Running',
  attention: 'Needs attention',
  critical: 'Failed task',
};

const deriveCardStatus = (ws, isProcessing, hasPendingApprovals) => {
  if ((ws.failedTaskCount || 0) > 0) return 'critical';
  if (
    hasPendingApprovals ||
    (ws.blockedTaskCount || 0) > 0 ||
    (ws.needsUserInputTaskCount || 0) > 0
  ) return 'attention';
  if (isProcessing) return 'running';
  return 'idle';
};

const ATTENTION_PILLS = [
  { key: 'failed', countField: 'failedTaskCount', label: 'failed', tone: 'critical' },
  { key: 'blocked', countField: 'blockedTaskCount', label: 'blocked', tone: 'attention' },
  { key: 'needs_input', countField: 'needsUserInputTaskCount', label: 'needs input', tone: 'attention' },
];

const Fleet = () => {
  const navigate = useNavigate();
  const [workspaces, setWorkspaces] = useState([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState('');
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState(null);
  const [selectedSnapshot, setSelectedSnapshot] = useState(null);
  const [snapshotError, setSnapshotError] = useState('');
  // Workspace settings modal — opened by the cog icon on each card. We
  // mount the same `WorkspaceSettingsModal` used by the workspace page
  // here so users can edit settings without leaving Fleet. Snapshot is
  // fetched on demand because Fleet's card payload is intentionally
  // lighter than a full snapshot.
  const [settingsState, setSettingsState] = useState({
    open: false,
    workspaceId: null,
    snapshot: null,
  });
  // Pending delete confirmation. `null` when no dialog is open. Captures
  // title at request time so the dialog body stays stable even if the
  // workspace list refreshes mid-confirmation.
  const [pendingDelete, setPendingDelete] = useState(null);
  const [deleting, setDeleting] = useState(false);
  const { closeChat, isCurrentChatOpen } = useChatManager();
  const { selectAgent } = useFleet();
  const pendingPermissionCounts = usePermissionAttention();
  const activeRunsByWorkspace = useFleetActivity();

  // Close the sidebar chat when entering Fleet
  useEffect(() => {
    if (isCurrentChatOpen()) {
      closeChat();
    }
  }, []);

  // Load workspaces (initial + periodic refresh so chip counts stay live).
  const loadWorkspaces = useCallback(async () => {
    try {
      const all = await listWorkspaces();
      setWorkspaces(all || []);
      setError('');
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to load workspaces.');
      setWorkspaces([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    loadWorkspaces();
    const interval = window.setInterval(loadWorkspaces, REFRESH_INTERVAL_MS);
    return () => window.clearInterval(interval);
  }, [loadWorkspaces]);

  // Sort: scheduled-on-top, then most-recently-updated first.
  const sortedWorkspaces = useMemo(() => (
    [...workspaces].sort((a, b) => {
      const aSched = !!a.scheduleEnabled;
      const bSched = !!b.scheduleEnabled;
      if (aSched !== bSched) return aSched ? -1 : 1;
      return (b.updatedAt || 0) - (a.updatedAt || 0);
    })
  ), [workspaces]);

  const counters = useMemo(() => ({
    total: workspaces.length,
    periodic: workspaces.filter((w) => w.scheduleEnabled).length,
    // "running" now counts a workspace if EITHER a scheduled task is
    // running OR an interactive assistant run is in flight (the
    // useFleetActivity hook tracks the latter via the assistant event
    // stream — RunStarted / RunCompleted / RunFailed / RunCancelled).
    running: workspaces.filter(
      (w) => (w.runningTaskCount || 0) > 0 || (activeRunsByWorkspace[w.id] || 0) > 0
    ).length,
    attention: workspaces.filter((w) => (w.attentionTaskCount || 0) > 0).length,
  }), [workspaces, activeRunsByWorkspace]);

  const attentionWorkspaces = useMemo(
    () => sortedWorkspaces.filter((w) => (w.attentionTaskCount || 0) > 0),
    [sortedWorkspaces]
  );

  const selectedWorkspace = useMemo(
    () => sortedWorkspaces.find((w) => w.id === selectedWorkspaceId) || null,
    [sortedWorkspaces, selectedWorkspaceId]
  );

  // Fetch a fresh snapshot for the selected workspace and push its
  // messages/runs/toolCalls straight into the assistant store. Mirrors
  // Workspace.jsx's loadSnapshot — the snapshot endpoint already bakes
  // in everything we need, so a separate getSession/listMessages roundtrip
  // is wasted work and risks losing to a stub session entry created by
  // session_created events (initSession creates { messages: [] }).
  useEffect(() => {
    if (!selectedWorkspaceId) {
      setSelectedSnapshot(null);
      setSnapshotError('');
      return undefined;
    }
    let cancelled = false;
    setSnapshotError('');
    getWorkspaceSnapshot(selectedWorkspaceId)
      .then((snap) => {
        if (cancelled) return;
        setSelectedSnapshot(snap || null);
        if (snap?.session?.id) {
          useAssistantStore.getState().loadSessionData(
            snap.session.id,
            snap.session,
            snap.messages || [],
            snap.runs || [],
            snap.toolCalls || []
          );
        }
      })
      .catch((err) => {
        if (cancelled) return;
        setSnapshotError(typeof err === 'string' ? err : err?.message || 'Failed to load workspace.');
        setSelectedSnapshot(null);
      });
    return () => { cancelled = true; };
  }, [selectedWorkspaceId]);

  // Resolve which session the detail aside should subscribe to. The snapshot
  // endpoint prefers the workspace's *interactive* session (the chat the
  // user typed into), but periodic agent runs write to a *separate*
  // BackgroundJob session — so if the user has previously chatted with a
  // workspace that also runs scheduled jobs, the aside ends up pinned to
  // the interactive session forever and the periodic run's events stream
  // into a sibling session the view never reads from.
  //
  // To make the aside follow live activity: scan all sessions in the store
  // for ones matching this workspace, and prefer whichever has a
  // non-terminal run. Pin the choice (don't snap back after the run ends)
  // so the user can keep reading the agent's output. Reset on workspace
  // change.
  const liveActiveSessionId = useAssistantStore((state) => {
    if (!selectedWorkspaceId) return null;
    for (const [id, s] of Object.entries(state.sessions)) {
      if (s.session?.context?.workspaceId !== selectedWorkspaceId) continue;
      const hasActiveRun = (s.runs || []).some((r) =>
        ['queued', 'running', 'waiting_for_tool'].includes(r.status)
      );
      if (hasActiveRun) return id;
    }
    return null;
  });

  const [pinnedSessionId, setPinnedSessionId] = useState(null);
  useEffect(() => {
    setPinnedSessionId(null);
  }, [selectedWorkspaceId]);
  useEffect(() => {
    if (liveActiveSessionId && liveActiveSessionId !== pinnedSessionId) {
      setPinnedSessionId(liveActiveSessionId);
    }
  }, [liveActiveSessionId, pinnedSessionId]);

  const detailSessionId =
    pinnedSessionId || selectedSnapshot?.session?.id || null;
  const sessionState = useAssistantStore((state) =>
    detailSessionId ? state.sessions[detailSessionId] : null
  );

  const handleOpenWorkspace = useCallback((id) => {
    if (!id) return;
    navigate(`/workspace/${id}`);
  }, [navigate]);

  const handleOpenSettings = useCallback(async (id) => {
    if (!id) return;
    try {
      const snapshot = await getWorkspaceSnapshot(id);
      setSettingsState({ open: true, workspaceId: id, snapshot });
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to open workspace settings.');
    }
  }, []);

  const handleSettingsClose = useCallback(() => {
    setSettingsState({ open: false, workspaceId: null, snapshot: null });
  }, []);

  // After a save inside the modal: refetch the snapshot so the modal's
  // own view stays current (e.g., a newly created agent shows up in the
  // sidebar) and refresh the fleet card data (title/schedule changes).
  const handleSettingsChanged = useCallback(async () => {
    const id = settingsState.workspaceId;
    if (!id) return;
    try {
      const snapshot = await getWorkspaceSnapshot(id);
      setSettingsState((s) => (s.workspaceId === id ? { ...s, snapshot } : s));
    } catch { /* non-fatal — modal stays open with old snapshot */ }
    loadWorkspaces();
  }, [settingsState.workspaceId, loadWorkspaces]);

  const handleRequestDeleteWorkspace = useCallback((id, title) => {
    if (!id) return;
    setPendingDelete({ id, title: title || 'this workspace' });
  }, []);

  const handleCancelDelete = useCallback(() => {
    if (deleting) return;
    setPendingDelete(null);
  }, [deleting]);

  const handleConfirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    const { id } = pendingDelete;
    setDeleting(true);
    try {
      await deleteWorkspace(id);
      if (selectedWorkspaceId === id) {
        setSelectedWorkspaceId(null);
      }
      await loadWorkspaces();
      setPendingDelete(null);
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to delete workspace.');
    } finally {
      setDeleting(false);
    }
  }, [pendingDelete, loadWorkspaces, selectedWorkspaceId]);

  // Track the workspace whose "run now" is in flight so we can disable the
  // button and avoid double-firing. The scheduler itself refuses a second
  // force_ready while a run is active, but UI feedback should reflect that
  // before the round-trip completes.
  const [runNowBusyId, setRunNowBusyId] = useState(null);
  const handleRunNow = useCallback(async (id) => {
    if (!id || runNowBusyId) return;
    setRunNowBusyId(id);
    try {
      await runWorkspaceNow(id);
      setError('');
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to start run.');
    } finally {
      setRunNowBusyId(null);
    }
  }, [runNowBusyId]);

  // Pause/resume the workspace's periodic schedule. Optimistically flip the
  // local row so the button swaps immediately; the next loadWorkspaces tick
  // will reconcile if the backend disagrees.
  const [pauseBusyId, setPauseBusyId] = useState(null);
  const handleTogglePause = useCallback(async (id, currentlyPaused) => {
    if (!id || pauseBusyId) return;
    setPauseBusyId(id);
    const nextPaused = !currentlyPaused;
    setWorkspaces((prev) =>
      prev.map((w) => (w.id === id ? { ...w, schedulePaused: nextPaused } : w))
    );
    try {
      await setWorkspaceSchedulePaused(id, nextPaused);
      setError('');
      await loadWorkspaces();
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to update pause state.');
      setWorkspaces((prev) =>
        prev.map((w) => (w.id === id ? { ...w, schedulePaused: currentlyPaused } : w))
      );
    } finally {
      setPauseBusyId(null);
    }
  }, [pauseBusyId, loadWorkspaces]);

  // Length-aware fallback: `[] || x` evaluates to `[]` in JS, so a bare
  // `||` chain masks the snapshot data whenever the store holds an empty
  // stub (created by session_created -> initSession). The snapshot
  // fallback is only valid while we're rendering the *snapshot's*
  // session — once we've pinned to a sibling (the periodic-run session),
  // its messages must come from the live store alone, otherwise we'd
  // bleed the interactive transcript into the background-job view.
  const showingSnapshotSession =
    !pinnedSessionId ||
    pinnedSessionId === selectedSnapshot?.session?.id;
  const detailMessages = sessionState?.messages?.length
    ? sessionState.messages
    : showingSnapshotSession
    ? selectedSnapshot?.messages || []
    : [];
  const detailToolCalls = sessionState?.toolCalls?.length
    ? sessionState.toolCalls
    : showingSnapshotSession
    ? selectedSnapshot?.toolCalls || EMPTY_TOOL_CALLS
    : EMPTY_TOOL_CALLS;
  const detailStreamingText = sessionState?.streamingTextByMessageId || EMPTY_STREAMING;
  const detailIsStreaming = sessionState?.isStreaming || false;

  // Auto-select the workspace's default agent for the chat input. Only
  // the default agent is reachable from Fleet — other workspace agents
  // are exposed in the workspace view, not here. When the user
  // deselects a workspace (or the workspace has no default agent), we
  // clear the selection so the chat input falls into its
  // "no-agent-selected" state.
  useEffect(() => {
    if (!selectedSnapshot) {
      selectAgent(null);
      return;
    }
    const defaultId = selectedSnapshot.defaultWorkspaceAgentId;
    const candidate = (selectedSnapshot.assignedAgents || []).find(
      (agent) => agent.id === defaultId
    ) || (selectedSnapshot.assignedAgents || []).find((agent) => agent.isDefault);
    if (!candidate) {
      selectAgent(null);
      return;
    }
    // Reshape into the field names TerminalEmulatorWrapper expects
    // (legacy FleetAgentSnapshot shape: agentId / name / description /
    // selectedMcpServerIds / execution / providerConnectionIds /
    // sessionId / tabId).
    selectAgent({
      // The workspace id is what the backend uses to open the per-workspace
      // DB when creating a session. `agentId` is the agent's own id; they
      // are *not* the same — passing the agent id as workspaceId used to
      // produce "Workspace <agent-id> not found" the first time a chat
      // was opened on a fresh workspace.
      workspaceId: selectedSnapshot.workspaceId || selectedWorkspaceId,
      agentId: candidate.id,
      name: candidate.displayName || candidate.agentName || candidate.id,
      description: candidate.agentDescription || '',
      providerConnectionIds: candidate.providerConnectionIds || [],
      selectedMcpServerIds: candidate.selectedMcpServerIds || [],
      execution: candidate.execution || undefined,
      sessionId: selectedSnapshot.session?.id || null,
      tabId: null,
    });
  }, [selectedSnapshot, selectedWorkspaceId, selectAgent]);

  // Clear the selection when leaving Fleet entirely so a stale agent
  // doesn't leak into other routes (the rest of the app still uses
  // useFleet().selectedAgent in places).
  useEffect(() => {
    return () => {
      selectAgent(null);
    };
  }, [selectAgent]);

  const hasSelection = !!selectedWorkspace;

  return (
    <div className={styles.fleetPage}>
      <div className={styles.header}>
        <div className={styles.headerLeft}>
          <h1 className={styles.title}>Fleet</h1>
          <p className={styles.subtitle}>
            Supervise the agent fleet, inspect activity, and intervene when needed.
          </p>
        </div>
        <div className={styles.headerCounters} role="status" aria-label="Fleet summary">
          <span className={styles.counterChip}>
            <strong>{counters.total}</strong> workspace{counters.total === 1 ? '' : 's'}
          </span>
          <span className={styles.counterSep}>{'·'}</span>
          <span className={styles.counterChip}>
            <strong>{counters.periodic}</strong> periodic
          </span>
          <span className={styles.counterSep}>{'·'}</span>
          <span className={styles.counterChip}>
            <strong>{counters.running}</strong> running
          </span>
          <span className={styles.counterSep}>{'·'}</span>
          <span
            className={`${styles.counterChip} ${counters.attention > 0 ? styles.counterChipAttention : ''}`}
          >
            <strong>{counters.attention}</strong> need attention
          </span>
        </div>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      <div className={`${styles.content} ${hasSelection ? styles.contentWithDetail : ''}`}>
        <div className={styles.cardGrid}>
          {attentionWorkspaces.length > 0 && (
            <section className={styles.attentionPanel} aria-label="Workspace notifications">
              <div className={styles.attentionHeader}>
                <span className={styles.attentionTitle}>Needs Attention</span>
                <span className={styles.attentionCount}>{attentionWorkspaces.length}</span>
              </div>
              <div className={styles.attentionList}>
                {attentionWorkspaces.slice(0, 6).map((workspace) => {
                  const status = workspace.latestAttentionTaskStatus;
                  const statusLabel = TASK_STATUS_LABEL[status] || status || 'Task';
                  return (
                    <button
                      key={workspace.id}
                      type="button"
                      className={styles.attentionItem}
                      onClick={() => setSelectedWorkspaceId(workspace.id)}
                    >
                      <div className={styles.attentionItemHeader}>
                        <span className={styles.attentionWorkspaceTitle}>{workspace.title}</span>
                        <span className={`${styles.attentionStatus} ${styles[`attentionStatus_${status}`] || ''}`}>
                          {statusLabel}
                        </span>
                      </div>
                      {workspace.latestAttentionTaskTitle && (
                        <span className={styles.attentionTaskTitle}>
                          {workspace.latestAttentionTaskTitle}
                        </span>
                      )}
                      {workspace.latestAttentionTaskSummary && (
                        <span className={styles.attentionSummary}>
                          {workspace.latestAttentionTaskSummary}
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
            </section>
          )}

          {sortedWorkspaces.map((ws) => {
            const isSelected = ws.id === selectedWorkspaceId;
            const isProcessing =
              (ws.runningTaskCount || 0) > 0 || (activeRunsByWorkspace[ws.id] || 0) > 0;
            const hasPendingApprovals = (pendingPermissionCounts[ws.id] || 0) > 0;
            const cardStatus = deriveCardStatus(ws, isProcessing, hasPendingApprovals);
            const permsCount = pendingPermissionCounts[ws.id] || 0;
            const attentionPills = [
              ...ATTENTION_PILLS.map((p) => ({ ...p, count: ws[p.countField] || 0 })),
              ...(permsCount > 0
                ? [{
                    key: 'permission',
                    count: permsCount,
                    label: permsCount === 1 ? 'needs approval' : 'need approval',
                    tone: 'critical',
                  }]
                : []),
            ].filter((p) => p.count > 0);
            const isPaused = !!ws.schedulePaused;
            const intervalMinutes = ws.intervalMinutes;
            // Status-row composition.
            //  - The "every Xm" portion is rendered as an inline toggle —
            //    clicking it pauses/resumes the schedule, and when paused
            //    the text gets a strikethrough. The cadence text IS the
            //    control, so we don't need a separate pause button.
            //  - The runtime prefix ("Due now", "Next run in 2m",
            //    "Scheduled") only renders when the schedule is *active*;
            //    when paused there's nothing to count down to, so the
            //    crossed-out interval stands alone.
            //  - `fallbackStateLabel` only kicks in for the rare case of
            //    scheduled-but-no-intervalMinutes (e.g., workspace created
            //    mid-session before the scheduler has been re-populated)
            //    where there's no toggle to render.
            const showScheduleToggle = ws.scheduleEnabled && !!intervalMinutes;
            // Runtime suffix (rendered after the cadence toggle). Suppressed
            // entirely when paused (the struck-through interval is the only
            // signal needed) and when active-without-countdown if we have
            // an interval to show (else "every 30m · Scheduled" is just
            // restating itself).
            const runtimeSuffix = !ws.scheduleEnabled || isPaused
              ? null
              : typeof ws.nextRunInSeconds === 'number'
                ? formatNextRun(ws.nextRunInSeconds)
                : intervalMinutes
                  ? null
                  : 'Scheduled';
            const fallbackStateLabel =
              ws.scheduleEnabled && !intervalMinutes && isPaused ? 'Paused' : null;
            // Card state is single-valued (priority-ordered in
            // deriveCardStatus), so we apply ONE state class — replacing
            // the older dual-ring approach where `workspaceCardProcessing`
            // and `workspaceCardAttention` could both be active and their
            // overlapping pulse animations muddied each other's color.
            const classes = [
              styles.workspaceCard,
              styles[`workspaceCardState_${cardStatus}`],
            ];
            if (isSelected) classes.push(styles.workspaceCardSelected);
            return (
              <div
                key={ws.id}
                className={classes.join(' ')}
                onClick={() => setSelectedWorkspaceId(ws.id)}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => { if (e.key === 'Enter') setSelectedWorkspaceId(ws.id); }}
              >
                <div className={styles.cardHeader}>
                  <div className={styles.cardTitleBlock}>
                    <span
                      className={`${styles.statusDot} ${styles[`statusDot_${cardStatus}`]}`}
                      aria-hidden="true"
                      title={CARD_STATUS_LABEL[cardStatus]}
                    />
                    <span className={styles.cardTitle} title={ws.title}>{ws.title}</span>
                  </div>
                  <div className={styles.cardHeaderActions}>
                    {/* Run-now is enabled regardless of pause state — a
                     * paused schedule means "don't auto-tick," not "no
                     * manual runs." Pause/Resume lives on the cadence
                     * text in the status row (see below). */}
                    {ws.scheduleEnabled && (
                      <button
                        type="button"
                        className={styles.runNowBtn}
                        onClick={(e) => {
                          e.stopPropagation();
                          handleRunNow(ws.id);
                        }}
                        disabled={isProcessing || runNowBusyId === ws.id}
                        title={
                          isProcessing
                            ? 'Already running'
                            : runNowBusyId === ws.id
                            ? 'Starting…'
                            : 'Run now'
                        }
                        aria-label="Run now"
                      >
                        <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                          <path d="M8 5v14l11-7z" />
                        </svg>
                      </button>
                    )}
                    <button
                      type="button"
                      className={styles.settingsBtn}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleOpenSettings(ws.id);
                      }}
                      title="Workspace settings"
                      aria-label="Workspace settings"
                    >
                      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <circle cx="12" cy="12" r="3" />
                        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className={styles.openBtn}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleOpenWorkspace(ws.id);
                      }}
                      title="Open workspace"
                      aria-label="Open workspace"
                    >
                      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M7 17L17 7" />
                        <path d="M8 7h9v9" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className={styles.deleteBtn}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleRequestDeleteWorkspace(ws.id, ws.title);
                      }}
                      title="Delete workspace"
                      aria-label="Delete workspace"
                    >
                      {'✕'}
                    </button>
                  </div>
                </div>
                <div className={styles.metaGrid}>
                  <span>{ws.messageCount} msgs</span>
                  <span>{ws.artifactCount} artifacts</span>
                  <span>{ws.memoryCount} memories</span>
                  <span>{ws.assignedAgentCount || 0} agents</span>
                  {ws.runningTaskCount > 0 && <span>{ws.runningTaskCount} running</span>}
                </div>
                {/* Single always-rendered footer row. Schedule status on the
                 * left, attention pills (failed/blocked/needs-input/needs-
                 * approval) inline on the right. Empty content still
                 * reserves min-height, so toggling pause or resolving the
                 * last attention item never changes card height. */}
                <div className={styles.statusRow}>
                  <span className={styles.statusRowText}>
                    {showScheduleToggle && (
                      <button
                        type="button"
                        className={`${styles.scheduleToggle} ${isPaused ? styles.scheduleTogglePaused : ''}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          handleTogglePause(ws.id, isPaused);
                        }}
                        disabled={pauseBusyId === ws.id}
                        title={
                          pauseBusyId === ws.id
                            ? 'Updating…'
                            : isPaused
                            ? 'Resume schedule'
                            : 'Pause schedule'
                        }
                        aria-pressed={isPaused}
                      >
                        every {intervalMinutes}m
                      </button>
                    )}
                    {showScheduleToggle && runtimeSuffix ? ' · ' : ''}
                    {runtimeSuffix}
                    {fallbackStateLabel}
                  </span>
                  {attentionPills.length > 0 && (
                    <div
                      className={styles.attentionPillsInline}
                      title={ws.latestAttentionTaskTitle || undefined}
                    >
                      {attentionPills.map((pill) => (
                        <span
                          key={pill.key}
                          className={`${styles.attentionPill} ${styles[`attentionPill_${pill.tone}`]}`}
                        >
                          {pill.count} {pill.label}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            );
          })}

          {!isLoading && workspaces.length === 0 && (
            <div className={styles.emptyState}>
              <h2 className={styles.emptyStateTitle}>No workspaces yet</h2>
              <p className={styles.emptyStateText}>
                Use the &quot;Create Workspace&quot; button below to start.
              </p>
            </div>
          )}
        </div>

        {selectedWorkspace && (
          <aside className={styles.detailPane} key={selectedWorkspace.id}>
            <div className={styles.detailHeader}>
              <div className={styles.detailHeaderText}>
                <h2 className={styles.detailTitle}>{selectedWorkspace.title}</h2>
                {selectedWorkspace.scheduleEnabled && (
                  <div className={styles.detailSubtitle}>
                    <span className={styles.detailPillPeriodic}>
                      {selectedWorkspace.schedulePaused
                        ? selectedWorkspace.intervalMinutes
                          ? `Paused · every ${selectedWorkspace.intervalMinutes}m`
                          : 'Paused'
                        : selectedWorkspace.intervalMinutes
                        ? `Periodic · every ${selectedWorkspace.intervalMinutes}m`
                        : 'Periodic'}
                    </span>
                  </div>
                )}
              </div>
              <div className={styles.detailActions}>
                <button
                  type="button"
                  className={styles.accentButton}
                  onClick={() => handleOpenWorkspace(selectedWorkspace.id)}
                >
                  Open workspace
                </button>
                <button
                  type="button"
                  className={styles.detailClose}
                  onClick={() => setSelectedWorkspaceId(null)}
                  title="Close detail"
                  aria-label="Close detail"
                >
                  {'×'}
                </button>
              </div>
            </div>

            <div className={styles.detailSection}>
              {snapshotError ? (
                <div className={styles.emptyDetail}>{snapshotError}</div>
              ) : detailMessages.length > 0 ? (
                <>
                  <ChatMessageList
                    messages={detailMessages}
                    toolCalls={detailToolCalls}
                    streamingText={detailStreamingText}
                    isStreaming={detailIsStreaming}
                  />
                  <InlineApprovalCard workspaceId={selectedWorkspace.id} />
                  <InlinePathGrantCard workspaceId={selectedWorkspace.id} />
                </>
              ) : (
                <>
                  <div className={styles.emptyDetail}>
                    No conversation yet. Open the workspace to start chatting with its manager.
                  </div>
                  <InlineApprovalCard workspaceId={selectedWorkspace.id} />
                  <InlinePathGrantCard workspaceId={selectedWorkspace.id} />
                </>
              )}
            </div>
          </aside>
        )}
      </div>

      <WorkspaceSettingsModal
        isOpen={settingsState.open}
        onClose={handleSettingsClose}
        workspaceId={settingsState.workspaceId}
        snapshot={settingsState.snapshot}
        initialSelection={{ kind: 'general' }}
        onChanged={handleSettingsChanged}
      />

      <ConfirmDialog
        isOpen={!!pendingDelete}
        title="Delete workspace?"
        body={(
          <>
            <strong>{pendingDelete?.title}</strong> will be permanently
            deleted, along with its agents, chat history, schedules, and
            artifacts. This cannot be undone.
          </>
        )}
        confirmLabel="Delete workspace"
        cancelLabel="Cancel"
        confirmTone="danger"
        busy={deleting}
        onCancel={handleCancelDelete}
        onConfirm={handleConfirmDelete}
      />
    </div>
  );
};

export default Fleet;
