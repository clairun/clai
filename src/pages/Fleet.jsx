import React, { useEffect, useMemo, useState, useCallback } from 'react';
import { useNavigate } from 'react-router-dom';
import { useFleet } from '../contexts/FleetContext';
import { useChatManager } from '../contexts/ChatManagerContext';
import { assistantClient, useAssistantStore } from '../assistant';
import { createAgent, updateAgent, getMcpServers, getSkills, setAgentEnabled } from '../api/client';
import { fleetRunNow } from '../fleet/client';
import { listWorkspaces, deleteWorkspace } from '../workspace/client';
import ChatMessageList, { NoticesBanner } from '../components/AssistantChat/ChatMessageList';
import MarkdownMessage from '../components/Chat/MarkdownMessage';
import AgentFormModal from '../components/Settings/AgentFormModal';
import styles from './Fleet.module.css';

const formatRelativeTime = (timestamp) => {
  if (!timestamp) {
    return 'Never';
  }

  const diffMs = Date.now() - timestamp;
  const diffSec = Math.max(0, Math.floor(diffMs / 1000));

  if (diffSec < 60) return `${diffSec}s ago`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
};

const formatNextRun = (seconds) => {
  if (seconds === null || seconds === undefined) {
    return 'Not scheduled';
  }
  if (seconds <= 0) {
    return 'Due now';
  }
  if (seconds < 60) {
    return `In ${seconds}s`;
  }
  if (seconds < 3600) {
    return `In ${Math.floor(seconds / 60)}m`;
  }
  if (seconds < 86400) {
    return `In ${Math.floor(seconds / 3600)}h`;
  }
  return `In ${Math.floor(seconds / 86400)}d`;
};

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

const EMPTY_TOOL_CALLS = [];
const EMPTY_STREAMING = {};

const SUMMARY_ITEMS = [
  { key: 'total', label: 'Total', cardClass: 'summaryCardTotal', valueClass: 'summaryValueTotal' },
  { key: 'enabled', label: 'Enabled', cardClass: 'summaryCardEnabled', valueClass: 'summaryValueEnabled' },
  { key: 'running', label: 'Running', cardClass: 'summaryCardRunning', valueClass: 'summaryValueRunning' },
  { key: 'error', label: 'Error', cardClass: 'summaryCardError', valueClass: 'summaryValueError' },
  { key: 'idle', label: 'Idle', cardClass: 'summaryCardIdle', valueClass: 'summaryValueIdle' },
  { key: 'disabled', label: 'Disabled', cardClass: 'summaryCardDisabled', valueClass: 'summaryValueDisabled' },
];

const RUN_STATUS_CLASS = {
  completed: 'ribbonCompleted',
  completed_with_warnings: 'ribbonWarning',
  failed: 'ribbonFailed',
  cancelled: 'ribbonCancelled',
  running: 'ribbonRunning',
};

const TASK_STATUS_LABEL = {
  blocked: 'Blocked',
  failed: 'Failed',
  needs_user_input: 'Needs input',
};

const MiniRibbon = ({ entries }) => {
  if (!entries || entries.length === 0) return null;

  // Backend returns newest first — reverse for oldest-left, newest-right
  const recent = [...entries].reverse().slice(-12);

  return (
    <div className={styles.miniRibbon}>
      {recent.map((entry, i) => (
        <div
          key={entry.startedAt || i}
          className={`${styles.miniSegment} ${styles[RUN_STATUS_CLASS[entry.status]] || styles.ribbonCompleted}`}
          title={`${entry.status}${entry.startedAt ? ' — ' + formatTimestamp(entry.startedAt) : ''}`}
        />
      ))}
    </div>
  );
};

const RunRibbon = ({ runs }) => {
  if (!runs || runs.length === 0) {
    return (
      <div className={styles.ribbonRow}>
        <span className={styles.ribbonLabel}>Runs</span>
        <span className={styles.ribbonEmpty}>No runs recorded</span>
      </div>
    );
  }

  // Sort ascending by time so oldest is on the left, most recent on the right
  const recent = [...runs]
    .sort((a, b) => (a.startedAt || 0) - (b.startedAt || 0))
    .slice(-24);

  return (
    <div className={styles.ribbonRow}>
      <span className={styles.ribbonLabel}>Runs</span>
      <div className={styles.ribbon}>
        {recent.map((run, i) => (
          <div
            key={run.id || run.startedAt || i}
            className={`${styles.ribbonSegment} ${styles[RUN_STATUS_CLASS[run.status]] || styles.ribbonCompleted}`}
            title={`${run.status}${run.startedAt ? ' — ' + formatTimestamp(run.startedAt) : ''}${run.error ? '\n' + run.error : ''}${run.notices?.length ? '\n' + run.notices.length + ' warning(s)' : ''}`}
          />
        ))}
      </div>
      <span className={styles.ribbonCount}>{runs.length} total</span>
    </div>
  );
};

const Fleet = () => {
  const navigate = useNavigate();
  const [detailSection, setDetailSection] = useState('chat');
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingAgent, setEditingAgent] = useState(null);
  const [mcpServers, setMcpServers] = useState([]);
  const [skills, setSkills] = useState([]);
  const [providerConnections, setProviderConnections] = useState([]);
  const [workspaces, setWorkspaces] = useState([]);
  const {
    summary,
    agents,
    selectedAgent,
    selectedAgentId,
    selectAgent,
    isLoading,
    error,
    refresh,
  } = useFleet();
  const { closeChat, isCurrentChatOpen } = useChatManager();

  // Close the sidebar chat when entering Fleet
  useEffect(() => {
    if (isCurrentChatOpen()) {
      closeChat();
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Load general workspaces
  const loadWorkspaces = useCallback(async () => {
    try {
      const all = await listWorkspaces();
      setWorkspaces(all || []);
    } catch {
      setWorkspaces([]);
    }
  }, []);

  useEffect(() => {
    loadWorkspaces();
  }, [loadWorkspaces]);
  const sessionState = useAssistantStore((state) =>
    selectedAgent?.sessionId ? state.sessions[selectedAgent.sessionId] : null
  );

  useEffect(() => {
    if (!selectedAgent?.sessionId) {
      return;
    }

    const existing = useAssistantStore.getState().sessions[selectedAgent.sessionId];
    if (existing) {
      return;
    }

    let cancelled = false;

    const load = async () => {
      try {
        const [session, messages, runs, toolCalls] = await Promise.all([
          assistantClient.getSession(selectedAgent.sessionId),
          assistantClient.loadSessionMessages(selectedAgent.sessionId),
          assistantClient.listRuns(selectedAgent.sessionId),
          assistantClient.listToolCalls(selectedAgent.sessionId),
        ]);

        if (cancelled || !session) {
          return;
        }

        useAssistantStore
          .getState()
          .loadSessionData(selectedAgent.sessionId, session, messages, runs, toolCalls);
      } catch {
        // Snapshot already contains enough fallback data for the card/detail preview.
      }
    };

    load();

    return () => {
      cancelled = true;
    };
  }, [selectedAgent?.sessionId]);

  const handleOpenWorkspace = useCallback(() => {
    if (!selectedAgent) return;
    navigate(`/workspace/${selectedAgent.agentId}`);
  }, [selectedAgent, navigate]);

  const handleToggleEnabled = useCallback(async (agentId, currentlyEnabled) => {
    try {
      await setAgentEnabled(agentId, !currentlyEnabled);
      refresh();
    } catch (err) {
      console.error('[Fleet] Toggle enabled failed:', err);
    }
  }, [refresh]);

  const handleRunNow = useCallback(async () => {
    if (!selectedAgent) return;
    try {
      await fleetRunNow(selectedAgent.agentId);
      // Give the scheduler a moment to pick it up, then refresh
      setTimeout(() => refresh(), 2000);
    } catch (err) {
      console.error('[Fleet] Run now failed:', err);
    }
  }, [selectedAgent, refresh]);

  const loadFormDependencies = useCallback(async () => {
    const [serversResult, skillsResult, connectionsResult] = await Promise.allSettled([
      getMcpServers(),
      getSkills(),
      assistantClient.listProviderConnections(),
    ]);

    if (serversResult.status === 'fulfilled') {
      setMcpServers(serversResult.value || []);
    }

    if (skillsResult.status === 'fulfilled') {
      setSkills(skillsResult.value || []);
    }

    if (connectionsResult.status === 'fulfilled') {
      setProviderConnections(connectionsResult.value || []);
    }
  }, []);

  const openCreateForm = useCallback(async () => {
    await loadFormDependencies();
    setEditingAgent(null);
    setIsFormOpen(true);
  }, [loadFormDependencies]);

  const openEditForm = useCallback(async () => {
    if (!selectedAgent) return;
    await loadFormDependencies();
    setEditingAgent({
      id: selectedAgent.agentId,
      name: selectedAgent.name,
      description: selectedAgent.description,
      intervalMinutes: selectedAgent.intervalMinutes,
      selectedMcpServerIds: selectedAgent.selectedMcpServerIds || [],
      selectedSkillIds: selectedAgent.selectedSkillIds || [],
      providerConnectionIds: selectedAgent.providerConnectionIds || [],
      execution: selectedAgent.execution || undefined,
    });
    setIsFormOpen(true);
  }, [loadFormDependencies, selectedAgent]);

  const handleFormSubmit = useCallback(async (formData) => {
    if (editingAgent) {
      await updateAgent({ id: editingAgent.id, ...formData });
    } else {
      await createAgent(formData);
    }
    setIsFormOpen(false);
    setEditingAgent(null);
    refresh();
  }, [editingAgent, refresh]);

  const handleFormClose = useCallback(() => {
    setIsFormOpen(false);
    setEditingAgent(null);
  }, []);

  const detailMessages = sessionState?.messages || [];
  const detailToolCalls = sessionState?.toolCalls || EMPTY_TOOL_CALLS;
  const detailStreamingText = sessionState?.streamingTextByMessageId || EMPTY_STREAMING;
  const detailIsStreaming = sessionState?.isStreaming || false;
  const detailRuns = sessionState?.runs || [];
  const generalWorkspaces = useMemo(
    () => workspaces.filter((workspace) => workspace.kind !== 'agent'),
    [workspaces]
  );
  const attentionWorkspaces = useMemo(
    () => workspaces.filter((workspace) => (workspace.attentionTaskCount || 0) > 0),
    [workspaces]
  );
  const workspaceAttentionById = useMemo(
    () => new Map(workspaces.map((workspace) => [workspace.id, workspace])),
    [workspaces]
  );

  return (
    <div className={styles.fleetPage}>
      <div className={styles.header}>
        <div>
          <div className={styles.titleRow}>
            <h1 className={styles.title}>Fleet</h1>
            {summary && (
              <span className={styles.titleBadge}>
                {summary.enabled} active
              </span>
            )}
          </div>
          <p className={styles.subtitle}>
            Supervise the agent fleet, inspect activity, and intervene when needed.
          </p>
        </div>
        <button type="button" className={styles.primaryButton} onClick={openCreateForm}>
          + New Agent
        </button>
      </div>

      {summary && (
        <div className={styles.summaryGrid}>
          {SUMMARY_ITEMS.map(({ key, label, cardClass, valueClass }) => (
            <div key={key} className={`${styles.summaryCard} ${styles[cardClass]}`}>
              <span className={styles.summaryLabel}>{label}</span>
              <strong className={`${styles.summaryValue} ${styles[valueClass]}`}>
                {summary[key]}
              </strong>
            </div>
          ))}
        </div>
      )}

      {error && <div className={styles.errorBanner}>{error}</div>}

      <div className={styles.content}>
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
                      onClick={() => navigate(`/workspace/${workspace.id}`)}
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

          {agents.map((agent) => {
            const isSelected = agent.agentId === selectedAgentId;
            const attention = workspaceAttentionById.get(agent.agentId);
            return (
              <button
                key={agent.agentId}
                type="button"
                className={`${styles.agentCard} ${isSelected ? styles.agentCardSelected : ''}`}
                onClick={() => selectAgent(agent.agentId)}
              >
                <div className={styles.cardHeader}>
                  <div className={styles.cardTitleBlock}>
                    <span className={styles.cardTitle}>{agent.name}</span>
                    <span className={`${styles.statusPill} ${styles[`status_${agent.status}`]}`}>
                      {agent.status.replace('_', ' ')}
                    </span>
                  </div>
                  <span
                    className={`${styles.enabledToggle} ${agent.enabled ? styles.enabledToggleOn : ''}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      handleToggleEnabled(agent.agentId, agent.enabled);
                    }}
                    role="switch"
                    aria-checked={agent.enabled}
                    title={agent.enabled ? 'Click to disable' : 'Click to enable'}
                  >
                    {agent.enabled ? 'Enabled' : 'Disabled'}
                  </span>
                </div>

                <div className={styles.metaGrid}>
                  <span>Interval: <strong>{agent.intervalMinutes}m</strong></span>
                  <span>Last: <strong>{formatRelativeTime(agent.lastCompletedAt || agent.lastStartedAt)}</strong></span>
                  <span>Next: <strong>{formatNextRun(agent.nextRunInSeconds)}</strong></span>
                </div>

                <MiniRibbon entries={agent.recentRunStatuses} />

                {(agent.providerConnectionNames?.length > 0 || agent.selectedMcpServerNames?.length > 0) && (
                  <div className={styles.mcpBadges}>
                    {agent.providerConnectionNames?.map((name) => (
                      <span key={name} className={styles.providerBadge}>{name}</span>
                    ))}
                    {agent.selectedMcpServerNames?.map((name) => (
                      <span key={name} className={styles.mcpBadge}>{name}</span>
                    ))}
                  </div>
                )}

                {agent.lastError && (
                  <p className={styles.errorPreview}>{agent.lastError}</p>
                )}
                {(attention?.attentionTaskCount || 0) > 0 && (
                  <div className={styles.taskAttentionPreview}>
                    <span className={styles.taskAttentionBadge}>
                      {attention.attentionTaskCount} task{attention.attentionTaskCount === 1 ? '' : 's'} need attention
                    </span>
                    {attention.latestAttentionTaskTitle && (
                      <span className={styles.taskAttentionText}>
                        {attention.latestAttentionTaskTitle}
                      </span>
                    )}
                  </div>
                )}
              </button>
            );
          })}

          {!isLoading && agents.length === 0 && (
            <div className={styles.emptyState}>
              <h2 className={styles.emptyStateTitle}>No scheduled agents configured</h2>
              <p className={styles.emptyStateText}>
                Create a scheduled agent to get started.
              </p>
              <button type="button" className={styles.primaryButton} onClick={openCreateForm} style={{ marginTop: 12 }}>
                + New Agent
              </button>
            </div>
          )}

          {generalWorkspaces.length > 0 && (
            <>
              <div className={styles.sectionDivider}>
                <span className={styles.sectionDividerLabel}>Past Workspaces</span>
                <span className={styles.sectionDividerCount}>{generalWorkspaces.length}</span>
              </div>
              {generalWorkspaces.map((ws) => (
                <div
                  key={ws.id}
                  className={styles.workspaceCard}
                  onClick={() => navigate(`/workspace/${ws.id}`)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => { if (e.key === 'Enter') navigate(`/workspace/${ws.id}`); }}
                >
                  <div className={styles.cardHeader}>
                    <div className={styles.cardTitleBlock}>
                      <span className={styles.cardTitle}>{ws.title}</span>
                      <span className={styles.workspaceBadge}>Workspace</span>
                    </div>
                    <button
                      type="button"
                      className={styles.deleteBtn}
                      onClick={(e) => {
                        e.stopPropagation();
                        deleteWorkspace(ws.id).then(() => loadWorkspaces());
                      }}
                      title="Delete workspace"
                    >
                      {'\u2715'}
                    </button>
                  </div>
                  <div className={styles.metaGrid}>
                    <span>{ws.messageCount} msgs</span>
                    <span>{ws.artifactCount} artifacts</span>
                    <span>{ws.memoryCount} memories</span>
                    <span>{ws.assignedAgentCount || 0} agents</span>
                    {ws.defaultManagerName && <span>Manager: <strong>{ws.defaultManagerName}</strong></span>}
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
              ))}
            </>
          )}
        </div>

        <div className={styles.detailPane}>
          {selectedAgent ? (
            <>
              <div className={styles.detailHeader}>
                <h2 className={styles.detailTitle}>{selectedAgent.name}</h2>
                <div className={styles.detailActions}>
                  <button
                    type="button"
                    className={styles.accentButton}
                    onClick={handleRunNow}
                    disabled={selectedAgent.status === 'running' || selectedAgent.status === 'disabled'}
                  >
                    Run Now
                  </button>
                  <button
                    type="button"
                    className={styles.secondaryButton}
                    onClick={openEditForm}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className={styles.secondaryButton}
                    onClick={handleOpenWorkspace}
                  >
                    Open Workspace
                  </button>
                </div>
              </div>

              <RunRibbon runs={selectedAgent.recentRunStatuses} />

              <div className={styles.detailTabs}>
                <button
                  type="button"
                  className={`${styles.detailTab} ${detailSection === 'chat' ? styles.detailTabActive : ''}`}
                  onClick={() => setDetailSection('chat')}
                >
                  Chat
                </button>
                <button
                  type="button"
                  className={`${styles.detailTab} ${detailSection === 'prompt' ? styles.detailTabActive : ''}`}
                  onClick={() => setDetailSection('prompt')}
                >
                  Prompt
                </button>
              </div>

              <div className={styles.detailSection}>
                {detailSection === 'chat' ? (
                  detailMessages.length > 0 ? (
                    <>
                      {(() => {
                        const latestWarningRun = detailRuns.find((r) => r.status === 'completed_with_warnings' && r.notices?.length);
                        return latestWarningRun ? <NoticesBanner notices={latestWarningRun.notices} /> : null;
                      })()}
                      <ChatMessageList
                        messages={detailMessages}
                        toolCalls={detailToolCalls}
                        streamingText={detailStreamingText}
                        isStreaming={detailIsStreaming}
                      />
                    </>
                  ) : (
                    <div className={styles.emptyDetail}>
                      {selectedAgent.sessionId
                        ? 'Conversation history has not been loaded yet.'
                        : 'This agent has not started a conversation yet.'}
                    </div>
                  )
                ) : (
                  selectedAgent.description ? (
                    <div className={styles.detailDescription}>
                      <MarkdownMessage content={selectedAgent.description} />
                    </div>
                  ) : (
                    <div className={styles.emptyDetail}>
                      No prompt configured for this agent.
                    </div>
                  )
                )}
              </div>
            </>
          ) : (
            <div className={styles.emptyDetail}>
              Select an agent to inspect conversation history and operational metadata.
            </div>
          )}
        </div>
      </div>

      <AgentFormModal
        isOpen={isFormOpen}
        onClose={handleFormClose}
        onSubmit={handleFormSubmit}
        agent={editingAgent}
        mcpServers={mcpServers}
        skills={skills}
        providerConnections={providerConnections}
      />
    </div>
  );
};

export default Fleet;
