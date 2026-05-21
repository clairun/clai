import React, { useEffect, useMemo, useState, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { useChatManager } from '../contexts/ChatManagerContext';
import { assistantClient, useAssistantStore } from '../assistant';
import { listWorkspaces, deleteWorkspace, getWorkspaceSnapshot } from '../workspace/client';
import ChatMessageList from '../components/AssistantChat/ChatMessageList';
import InlineApprovalCard from '../components/InlineApprovalCard';
import { useFleet } from '../contexts/FleetContext';
import { useFleetActivity } from '../hooks/useFleetActivity';
import { usePermissionAttention } from '../hooks/usePermissionAttention';
import styles from './Fleet.module.css';

const REFRESH_INTERVAL_MS = 5000;

const EMPTY_TOOL_CALLS = [];
const EMPTY_STREAMING = {};

const formatTimestamp = (timestamp) => {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  return date.toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};

const TASK_STATUS_LABEL = {
  blocked: 'Blocked',
  failed: 'Failed',
  needs_user_input: 'Needs input',
};

const Fleet = () => {
  const navigate = useNavigate();
  const [workspaces, setWorkspaces] = useState([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState('');
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState(null);
  const [selectedSnapshot, setSelectedSnapshot] = useState(null);
  const [snapshotError, setSnapshotError] = useState('');
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

  // Fetch a fresh snapshot for the selected workspace — gives us the session
  // id + recent messages so the detail-pane chat preview can render.
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
        if (!cancelled) setSelectedSnapshot(snap || null);
      })
      .catch((err) => {
        if (cancelled) return;
        setSnapshotError(typeof err === 'string' ? err : err?.message || 'Failed to load workspace.');
        setSelectedSnapshot(null);
      });
    return () => { cancelled = true; };
  }, [selectedWorkspaceId]);

  // Live-subscribe to the selected workspace's session so the preview updates
  // as messages stream in (events flow through MainLayout's useAssistantEvents).
  const detailSessionId = selectedSnapshot?.session?.id || null;
  const sessionState = useAssistantStore((state) =>
    detailSessionId ? state.sessions[detailSessionId] : null
  );

  useEffect(() => {
    if (!detailSessionId) return undefined;
    if (useAssistantStore.getState().sessions[detailSessionId]) return undefined;

    let cancelled = false;
    const load = async () => {
      try {
        const [session, messages, runs, toolCalls] = await Promise.all([
          assistantClient.getSession(detailSessionId),
          assistantClient.loadSessionMessages(detailSessionId),
          assistantClient.listRuns(detailSessionId),
          assistantClient.listToolCalls(detailSessionId, null),
        ]);
        if (cancelled || !session) return;
        useAssistantStore
          .getState()
          .loadSessionData(detailSessionId, session, messages || [], runs || [], toolCalls || []);
      } catch {
        // Snapshot already contains enough fallback for the preview.
      }
    };
    load();
    return () => { cancelled = true; };
  }, [detailSessionId]);

  const handleOpenWorkspace = useCallback((id) => {
    if (!id) return;
    navigate(`/workspace/${id}`);
  }, [navigate]);

  const handleDeleteWorkspace = useCallback(async (id) => {
    try {
      await deleteWorkspace(id);
      if (selectedWorkspaceId === id) {
        setSelectedWorkspaceId(null);
      }
      await loadWorkspaces();
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || 'Failed to delete workspace.');
    }
  }, [loadWorkspaces, selectedWorkspaceId]);

  const detailMessages = sessionState?.messages || selectedSnapshot?.messages || [];
  const detailToolCalls = sessionState?.toolCalls || selectedSnapshot?.toolCalls || EMPTY_TOOL_CALLS;
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
      agentId: candidate.id,
      name: candidate.displayName || candidate.agentName || candidate.id,
      description: candidate.agentDescription || '',
      providerConnectionIds: candidate.providerConnectionIds || [],
      selectedMcpServerIds: candidate.selectedMcpServerIds || [],
      execution: candidate.execution || undefined,
      sessionId: selectedSnapshot.session?.id || null,
      tabId: null,
    });
  }, [selectedSnapshot, selectAgent]);

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
            const classes = [styles.workspaceCard];
            if (isSelected) classes.push(styles.workspaceCardSelected);
            if (isProcessing) classes.push(styles.workspaceCardProcessing);
            if (hasPendingApprovals) classes.push(styles.workspaceCardAttention);
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
                    <span className={styles.cardTitle}>{ws.title}</span>
                    {ws.scheduleEnabled ? (
                      <span className={`${styles.workspaceBadge} ${styles.workspaceBadgePeriodic}`}>
                        {ws.intervalMinutes ? `Periodic · ${ws.intervalMinutes}m` : 'Periodic'}
                      </span>
                    ) : (
                      <span className={styles.workspaceBadge}>Workspace</span>
                    )}
                    {pendingPermissionCounts[ws.id] > 0 && (
                      <span
                        className={`${styles.workspaceBadge} ${styles.workspaceBadgePermission}`}
                        title={`${pendingPermissionCounts[ws.id]} command${pendingPermissionCounts[ws.id] === 1 ? '' : 's'} need approval`}
                      >
                        Needs approval ({pendingPermissionCounts[ws.id]})
                      </span>
                    )}
                  </div>
                  <div className={styles.cardHeaderActions}>
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
                        handleDeleteWorkspace(ws.id);
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
                {(ws.attentionTaskCount || 0) > 0 && (
                  <div className={styles.taskAttentionPreview}>
                    <span className={styles.taskAttentionBadge}>
                      {ws.attentionTaskCount} task{ws.attentionTaskCount === 1 ? '' : 's'} need attention
                    </span>
                    {ws.latestAttentionTaskTitle && (
                      <span className={styles.taskAttentionText}>
                        {ws.latestAttentionTaskTitle}
                      </span>
                    )}
                  </div>
                )}
                <div className={styles.workspaceTime}>
                  {formatTimestamp(ws.updatedAt || ws.createdAt)}
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
                      {selectedWorkspace.intervalMinutes
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
                </>
              ) : (
                <>
                  <div className={styles.emptyDetail}>
                    No conversation yet. Open the workspace to start chatting with its manager.
                  </div>
                  <InlineApprovalCard workspaceId={selectedWorkspace.id} />
                </>
              )}
            </div>
          </aside>
        )}
      </div>
    </div>
  );
};

export default Fleet;
