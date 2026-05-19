import React, { useCallback, useEffect, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import {
  getAgentTemplates,
  getMcpServers,
  getSkills,
  workspaceCreateAgent,
  workspaceDeleteAgent,
  workspaceGetAgent,
  workspaceUpdateAgent,
} from '../api/client';
import AgentFormModal from '../components/Settings/AgentFormModal';
import WorkspaceTaskTranscriptPanel from '../components/WorkspaceTaskTranscriptPanel';
import WorkspaceFilePreviewPanel from '../components/WorkspaceFilePreviewPanel';
import { assistantClient, useAssistantStore } from '../assistant';
import ChatMessageList from '../components/AssistantChat/ChatMessageList';
import { useChatManager } from '../contexts/ChatManagerContext';
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts';
import {
  acknowledgeWorkspaceTask,
  getWorkspaceSnapshot,
  submitWorkspaceTaskFeedback,
} from '../workspace/client';
import styles from './Workspace.module.css';

const DEFAULT_WORKSPACE_ID = 'default';
const REFRESH_INTERVAL_MS = 5000;

const formatTimestamp = (timestamp) => {
  if (!timestamp) return 'Never';
  return new Date(timestamp).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
};

const formatRelativeTime = (timestamp) => {
  if (!timestamp) return 'Never';
  const diffMs = Date.now() - timestamp;
  const diffSec = Math.max(0, Math.floor(diffMs / 1000));
  if (diffSec < 60) return `${diffSec}s ago`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
};

const formatNextRun = (seconds) => {
  if (seconds === null || seconds === undefined) return null;
  if (seconds <= 0) return 'Due now';
  if (seconds < 60) return `In ${seconds}s`;
  if (seconds < 3600) return `In ${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `In ${Math.floor(seconds / 3600)}h`;
  return `In ${Math.floor(seconds / 86400)}d`;
};

const getLastRunInfo = (runs) => {
  if (!runs || runs.length === 0) return null;
  // runs are sorted newest first from backend
  const last = [...runs].sort((a, b) => (b.startedAt || 0) - (a.startedAt || 0))[0];
  return last;
};

const RUN_STATUS_LABEL = {
  completed: 'Completed',
  completed_with_warnings: 'Warnings',
  failed: 'Failed',
  running: 'Running',
  queued: 'Queued',
  cancelled: 'Cancelled',
};

const TASK_STATUS_LABEL = {
  queued: 'Queued',
  running: 'Running',
  completed: 'Completed',
  failed: 'Failed',
  blocked: 'Blocked',
  needs_user_input: 'Needs input',
};

const isTaskAttention = (task) => (
  (task.status === 'blocked' || task.status === 'failed' || task.status === 'needs_user_input')
  && !task.attentionAcknowledgedAt
  && !task.userResponseAt
);

const WorkspaceAgentsPanel = ({
  workspaceId,
  snapshot,
  busy,
  error,
  onOpenCreate,
  onOpenEdit,
  onRemove,
}) => {
  const assignedAgents = snapshot?.assignedAgents || [];
  const isManageable = snapshot?.kind !== 'agent' && workspaceId !== DEFAULT_WORKSPACE_ID;

  // The workspace's "manager" is implicit — surfaced via the header gear icon.
  // Drawer lists only attached helper agents; the "+ Add" affordance lives in
  // the drawer header so we don't duplicate the "Agents" title here.
  const memberAgents = assignedAgents.filter((agent) => !agent.isDefault);

  if (!isManageable && memberAgents.length === 0) {
    return null;
  }

  return (
    <section className={styles.agentRoster} aria-label="Workspace agents">
      {error && <div className={styles.agentRosterError}>{error}</div>}

      {memberAgents.length > 0 ? (
        <div className={styles.agentRosterList}>
          {memberAgents.map((agent) => (
            <div key={agent.id} className={styles.agentRosterItem}>
              <div className={styles.agentRosterIdentity}>
                <div className={styles.agentRosterNameRow}>
                  <span className={styles.agentRosterName}>{agent.displayName}</span>
                </div>
                {agent.agentDescription && (
                  <p className={styles.agentRosterDescription}>{agent.agentDescription}</p>
                )}
              </div>
              {isManageable && (
                <div className={styles.agentRosterActions}>
                  <button
                    type="button"
                    className={styles.agentAction}
                    onClick={() => onOpenEdit(agent.id)}
                    disabled={!!busy}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className={styles.agentActionDanger}
                    onClick={() => onRemove(agent.id)}
                    disabled={!!busy}
                  >
                    Remove
                  </button>
                </div>
              )}
            </div>
          ))}
        </div>
      ) : (
        <div className={styles.agentRosterEmpty}>
          The workspace itself is the entry-point agent — its configuration is
          edited via the gear icon next to the workspace title. Agents added
          here are optional helpers the workspace can call as tools.
        </div>
      )}
    </section>
  );
};

const WorkspaceTasksPanel = ({ workspaceId, tasks, onChanged, onViewTask }) => {
  const visibleTasks = tasks || [];
  const [feedbackDrafts, setFeedbackDrafts] = useState({});
  const [busyTaskId, setBusyTaskId] = useState('');
  const [error, setError] = useState('');

  const handleAcknowledge = useCallback(async (taskId) => {
    if (busyTaskId) return;
    setBusyTaskId(taskId);
    setError('');
    try {
      await acknowledgeWorkspaceTask(workspaceId, taskId);
      await onChanged();
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to acknowledge task.'));
    } finally {
      setBusyTaskId('');
    }
  }, [busyTaskId, onChanged, workspaceId]);

  const handleSubmitFeedback = useCallback(async (taskId) => {
    if (busyTaskId) return;
    const response = (feedbackDrafts[taskId] || '').trim();
    if (!response) {
      setError('Feedback cannot be empty.');
      return;
    }

    setBusyTaskId(taskId);
    setError('');
    try {
      await submitWorkspaceTaskFeedback(workspaceId, taskId, response);
      setFeedbackDrafts((current) => ({ ...current, [taskId]: '' }));
      await onChanged();
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to submit feedback.'));
    } finally {
      setBusyTaskId('');
    }
  }, [busyTaskId, feedbackDrafts, onChanged, workspaceId]);

  return (
    <section className={styles.taskActivity} aria-label="Workspace task activity">
      <div className={styles.taskActivityHeader}>
        <div className={styles.agentRosterTitleBlock}>
          <h2 className={styles.agentRosterTitle}>Task Activity</h2>
          <span className={styles.agentRosterMeta}>{visibleTasks.length} recent</span>
        </div>
      </div>

      {error && <div className={styles.agentRosterError}>{error}</div>}

      {visibleTasks.length > 0 ? (
        <div className={styles.taskList}>
          {visibleTasks.map((task) => {
            const statusLabel = TASK_STATUS_LABEL[task.status] || task.status;
            const detail = task.error || task.resultSummary || task.instructions;
            const needsAttention = isTaskAttention(task);
            const draft = feedbackDrafts[task.id] || '';
            return (
              <div key={task.id} className={styles.taskItem}>
                <div className={styles.taskMain}>
                  <div className={styles.taskTitleRow}>
                    <span className={styles.taskTitle}>{task.title}</span>
                    <span className={`${styles.taskStatus} ${styles[`taskStatus_${task.status}`] || ''}`}>
                      {statusLabel}
                    </span>
                  </div>
                  <div className={styles.taskMeta}>
                    <span>{task.assignedAgentDisplayName}</span>
                    <span className={styles.metricSeparator}>{'\u00B7'}</span>
                    <span>{formatRelativeTime(task.updatedAt)}</span>
                  </div>
                  {detail && (
                    <p className={styles.taskSummary}>{detail}</p>
                  )}
                  {task.userResponse && (
                    <p className={styles.taskUserResponse}>
                      User response: {task.userResponse}
                    </p>
                  )}
                  {needsAttention && task.status === 'needs_user_input' && (
                    <div className={styles.taskFeedbackBox}>
                      <textarea
                        className={styles.taskFeedbackInput}
                        value={draft}
                        onChange={(event) => setFeedbackDrafts((current) => ({
                          ...current,
                          [task.id]: event.target.value,
                        }))}
                        placeholder="Reply for the manager"
                        rows={3}
                        disabled={busyTaskId === task.id}
                      />
                      <div className={styles.taskActions}>
                        <button
                          type="button"
                          className={styles.taskActionPrimary}
                          onClick={() => handleSubmitFeedback(task.id)}
                          disabled={busyTaskId === task.id || !draft.trim()}
                        >
                          Submit response
                        </button>
                        <button
                          type="button"
                          className={styles.taskAction}
                          onClick={() => handleAcknowledge(task.id)}
                          disabled={busyTaskId === task.id}
                        >
                          Mark reviewed
                        </button>
                        {task.sessionId && (
                          <button
                            type="button"
                            className={styles.taskAction}
                            onClick={() => onViewTask?.(task)}
                          >
                            View log
                          </button>
                        )}
                      </div>
                    </div>
                  )}
                  {needsAttention && task.status !== 'needs_user_input' && (
                    <div className={styles.taskActions}>
                      <button
                        type="button"
                        className={styles.taskAction}
                        onClick={() => handleAcknowledge(task.id)}
                        disabled={busyTaskId === task.id}
                      >
                        Mark reviewed
                      </button>
                      {task.sessionId && (
                        <button
                          type="button"
                          className={styles.taskAction}
                          onClick={() => onViewTask?.(task)}
                        >
                          View log
                        </button>
                      )}
                    </div>
                  )}
                  {!needsAttention && task.sessionId && (
                    <div className={styles.taskActions}>
                      <button
                        type="button"
                        className={styles.taskAction}
                        onClick={() => onViewTask?.(task)}
                      >
                        View log
                      </button>
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      ) : (
        <div className={styles.agentRosterEmpty}>No delegated tasks yet.</div>
      )}

    </section>
  );
};

const WorkspaceAttentionBanner = ({ tasks }) => {
  const attentionTasks = (tasks || []).filter(isTaskAttention);

  if (attentionTasks.length === 0) {
    return null;
  }

  const primary = attentionTasks[0];
  const statusLabel = TASK_STATUS_LABEL[primary.status] || primary.status;
  const detail = primary.error || primary.resultSummary || primary.instructions;

  return (
    <section className={styles.attentionBanner} aria-label="Workspace attention">
      <div className={styles.attentionBannerHeader}>
        <span className={styles.attentionBannerTitle}>
          {attentionTasks.length} task{attentionTasks.length === 1 ? '' : 's'} need attention
        </span>
        <span className={`${styles.taskStatus} ${styles[`taskStatus_${primary.status}`] || ''}`}>
          {statusLabel}
        </span>
      </div>
      <div className={styles.attentionBannerTask}>{primary.title}</div>
      {detail && <p className={styles.attentionBannerDetail}>{detail}</p>}
    </section>
  );
};

/**
 * Compact workspace header with breadcrumb navigation, status, and inline metrics.
 */
const WorkspaceHeader = ({
  snapshot,
  workspaceId,
  isGenericWorkspace,
  messages,
  memories,
  artifacts,
  navigate,
  activePanel,
  setActivePanel,
  onOpenWorkspaceSettings,
}) => {
  const isAgent = snapshot?.kind === 'agent';
  const lastRun = getLastRunInfo(snapshot?.runs);
  const nextRunText = formatNextRun(snapshot?.nextRunInSeconds);
  // Manager is invisible to the user — exclude it from the headline count so
  // the chip and the drawer (which already filters !isDefault) agree.
  const assignedAgentCount = (snapshot?.assignedAgents || []).filter((a) => !a.isDefault).length;
  const taskCount = snapshot?.tasks?.length || 0;

  // Click a counter to open its panel; click again (or click another) to switch.
  // null = no panel open, chat takes the full content area.
  const togglePanel = (panel) => {
    setActivePanel((current) => (current === panel ? null : panel));
  };

  const renderCounter = (panel, count, label, clickable = true) => {
    const isActive = activePanel === panel;
    if (!clickable) {
      return <span className={styles.metric}>{count} {label}</span>;
    }
    return (
      <button
        type="button"
        className={`${styles.metricButton} ${isActive ? styles.metricButtonActive : ''}`}
        onClick={() => togglePanel(panel)}
        title={`Toggle ${label} panel`}
      >
        {count} {label}
      </button>
    );
  };

  return (
    <div className={styles.header}>
      <div className={styles.headerLeft}>
        <button
          type="button"
          className={styles.breadcrumb}
          onClick={() => navigate('/fleet')}
        >
          Fleet
        </button>
        <span className={styles.breadcrumbSeparator}>/</span>
        <h1 className={styles.title}>
          {snapshot?.title || (isGenericWorkspace ? 'Workspace' : workspaceId)}
        </h1>
        <span className={styles.kindBadge}>
          {isAgent ? 'Agent' : 'General'}
        </span>
        {isAgent && snapshot?.enabled === false && (
          <span className={styles.disabledBadge}>Disabled</span>
        )}
        {onOpenWorkspaceSettings && (
          <button
            type="button"
            className={styles.workspaceSettingsButton}
            onClick={onOpenWorkspaceSettings}
            title="Workspace settings"
            aria-label="Open workspace settings"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
          </button>
        )}
      </div>
      <div className={styles.headerRight}>
        {isAgent && lastRun && (
          <>
            <span className={`${styles.statusDot} ${styles[`status_${lastRun.status}`]}`} />
            <span className={styles.metric}>
              {RUN_STATUS_LABEL[lastRun.status] || lastRun.status}
            </span>
            <span className={styles.metricSeparator}>{'\u00B7'}</span>
            <span className={styles.metric}>
              Last: {formatRelativeTime(lastRun.completedAt || lastRun.startedAt)}
            </span>
            {nextRunText && (
              <>
                <span className={styles.metricSeparator}>{'\u00B7'}</span>
                <span className={styles.metric}>Next: {nextRunText}</span>
              </>
            )}
            <span className={styles.metricSeparator}>{'\u00B7'}</span>
          </>
        )}
        {renderCounter(null, messages.length, 'msgs', false)}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('agents', assignedAgentCount, 'agents')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('tasks', taskCount, 'tasks')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('memories', memories.length, 'memories')}
        <span className={styles.metricSeparator}>{'\u00B7'}</span>
        {renderCounter('artifacts', artifacts.length, 'artifacts')}
      </div>
    </div>
  );
};

// Chat is the workspace's primary surface. Memories, artifacts, tasks, and
// member agents live in the drawer (toggled from the header counters) and
// open in modals when inspected — the chat is never hidden.
const ChatFirstLayout = ({ sessionId, messages, toolCalls, streamingText, isStreaming }) => (
  <div className={styles.chatFirstContent}>
    {messages.length > 0 ? (
      <ChatMessageList
        messages={messages}
        toolCalls={toolCalls}
        streamingText={streamingText}
        isStreaming={isStreaming}
      />
    ) : (
      <div className={styles.chatFirstEmpty}>
        <div className={styles.chatFirstEmptyIcon}>
          <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
        </div>
        <p className={styles.chatFirstEmptyTitle}>Start a conversation</p>
        <p className={styles.chatFirstEmptyText}>
          Type a message in the terminal below to begin. This workspace can search the web, create documents, and use any attached MCP servers.
        </p>
      </div>
    )}
  </div>
);

const Workspace = () => {
  const params = useParams();
  const navigate = useNavigate();
  const { toggleChat } = useChatManager();
  const workspaceId = params.workspaceId || DEFAULT_WORKSPACE_ID;
  const isGenericWorkspace = workspaceId === DEFAULT_WORKSPACE_ID;
  const [snapshot, setSnapshot] = useState(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState('');
  // Which "drawer" is open in response to a counter click in the header.
  // null = chat-only. 'agents' | 'tasks' | 'memories' | 'artifacts' otherwise.
  const [activePanel, setActivePanel] = useState(null);
  // Slide-out side panels (only one may be open at a time):
  //   - previewEntry: { kind: 'memory' | 'artifact', entry } — file preview
  //   - viewingTask:  task object — task transcript log
  // Opening one clears the other; closing the drawer clears both.
  const [previewEntry, setPreviewEntry] = useState(null);
  const [viewingTask, setViewingTask] = useState(null);

  const openPreviewEntry = useCallback((next) => {
    setViewingTask(null);
    setPreviewEntry(next);
  }, []);

  const openTaskTranscript = useCallback((task) => {
    setPreviewEntry(null);
    setViewingTask(task);
  }, []);

  // The side panels are contextual to the open drawer chip — switching to a
  // panel that doesn't own the slide-out content clears it.
  useEffect(() => {
    if (activePanel !== 'memories' && activePanel !== 'artifacts') {
      setPreviewEntry(null);
    }
    if (activePanel !== 'tasks') {
      setViewingTask(null);
    }
  }, [activePanel]);

  const isSidePanelOpen = !!previewEntry || !!viewingTask;

  // ── Agent form (lifted up so the Workspace Settings entry can live on the
  //    header, not inside the agents drawer) ────────────────────────────────
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingAgent, setEditingAgent] = useState(null);
  const [agentBusy, setAgentBusy] = useState('');
  const [agentError, setAgentError] = useState('');
  const [formDeps, setFormDeps] = useState({
    mcpServers: [],
    providerConnections: [],
    skills: [],
    agentTemplates: [],
  });
  const sessionId = snapshot?.session?.id || null;
  const sessionState = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId] : null
  );

  // Register Ctrl/Cmd+Shift+C to toggle chat panel — only for agent workspaces.
  // General workspaces embed chat directly in the page.
  useKeyboardShortcuts({
    onToggleChat: () => {
      if (snapshot?.kind === 'agent') {
        toggleChat();
      }
    },
  });

  const loadSnapshot = useCallback(async (showSpinner = false) => {
    if (showSpinner) {
      setIsLoading(true);
    }

    try {
      const nextSnapshot = await getWorkspaceSnapshot(workspaceId);
      setSnapshot(nextSnapshot);
      setError('');

      if (nextSnapshot?.session) {
        const store = useAssistantStore.getState();
        store.loadSessionData(
          nextSnapshot.session.id,
          nextSnapshot.session,
          nextSnapshot.messages || [],
          nextSnapshot.runs || [],
          nextSnapshot.toolCalls || []
        );
        // Bridge workspace session to the chat panel via synthetic tab key
        store.setActiveSessionForTab(`workspace:${workspaceId}`, nextSnapshot.session.id);
      }
    } catch (err) {
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to load workspace.'));
    } finally {
      setIsLoading(false);
    }
  }, [workspaceId]);

  // ── Agent form handlers (lifted from WorkspaceAgentsPanel) ─────────────
  const managerAgentId = snapshot?.assignedAgents?.find((a) => a.isDefault)?.id || null;

  const loadFormDependencies = useCallback(async () => {
    const [serversResult, skillsResult, connectionsResult, templatesResult] = await Promise.allSettled([
      getMcpServers(),
      getSkills(),
      assistantClient.listProviderConnections(),
      getAgentTemplates(),
    ]);
    setFormDeps({
      mcpServers: serversResult.status === 'fulfilled' ? (serversResult.value || []) : [],
      skills: skillsResult.status === 'fulfilled' ? (skillsResult.value || []) : [],
      providerConnections: connectionsResult.status === 'fulfilled' ? (connectionsResult.value || []) : [],
      agentTemplates: templatesResult.status === 'fulfilled' ? (templatesResult.value || []) : [],
    });
  }, []);

  const openMemberCreate = useCallback(async () => {
    setAgentError('');
    await loadFormDependencies();
    setEditingAgent(null);
    setIsFormOpen(true);
  }, [loadFormDependencies]);

  const openAgentEdit = useCallback(async (workspaceAgentId) => {
    if (agentBusy) return;
    setAgentBusy(`edit:${workspaceAgentId}`);
    setAgentError('');
    try {
      const [detail] = await Promise.all([
        workspaceGetAgent(workspaceId, workspaceAgentId),
        loadFormDependencies(),
      ]);
      if (!detail) {
        throw new Error('Workspace agent not found.');
      }
      setEditingAgent(detail);
      setIsFormOpen(true);
    } catch (err) {
      setAgentError(typeof err === 'string' ? err : (err?.message || 'Failed to load agent.'));
    } finally {
      setAgentBusy('');
    }
  }, [agentBusy, loadFormDependencies, workspaceId]);

  const openWorkspaceSettings = useCallback(async () => {
    if (!managerAgentId) return;
    await openAgentEdit(managerAgentId);
  }, [managerAgentId, openAgentEdit]);

  const handleFormSubmit = useCallback(async (formData) => {
    setAgentError('');
    try {
      if (editingAgent) {
        await workspaceUpdateAgent({
          workspaceId,
          agentId: editingAgent.id,
          name: formData.name,
          description: formData.description,
          selectedSkillIds: formData.selectedSkillIds || [],
          selectedMcpServerIds: formData.selectedMcpServerIds || [],
          providerConnectionIds: formData.providerConnectionIds || [],
          execution: formData.execution,
          exposedTools: formData.exposedTools || [],
          scheduleEnabled: !!formData.scheduleEnabled,
          intervalMinutes: formData.intervalMinutes || 0,
          enabled: formData.enabled !== false,
        });
      } else {
        await workspaceCreateAgent({
          workspaceId,
          name: formData.name,
          description: formData.description,
          selectedSkillIds: formData.selectedSkillIds || [],
          selectedMcpServerIds: formData.selectedMcpServerIds || [],
          providerConnectionIds: formData.providerConnectionIds || [],
          execution: formData.execution,
          exposedTools: formData.exposedTools || [],
          scheduleEnabled: !!formData.scheduleEnabled,
          intervalMinutes: formData.intervalMinutes || 0,
          enabled: formData.enabled !== false,
        });
      }
      setIsFormOpen(false);
      setEditingAgent(null);
      await loadSnapshot(false);
    } catch (err) {
      setAgentError(typeof err === 'string' ? err : (err?.message || 'Failed to save agent.'));
    }
  }, [editingAgent, loadSnapshot, workspaceId]);

  const handleFormClose = useCallback(() => {
    setIsFormOpen(false);
    setEditingAgent(null);
  }, []);

  const handleAgentRemove = useCallback(async (workspaceAgentId) => {
    if (agentBusy) return;
    setAgentBusy(`remove:${workspaceAgentId}`);
    setAgentError('');
    try {
      await workspaceDeleteAgent(workspaceId, workspaceAgentId);
      await loadSnapshot(false);
    } catch (err) {
      setAgentError(typeof err === 'string' ? err : (err?.message || 'Failed to remove agent.'));
    } finally {
      setAgentBusy('');
    }
  }, [agentBusy, loadSnapshot, workspaceId]);

  useEffect(() => {
    loadSnapshot(true);
    const interval = window.setInterval(() => loadSnapshot(false), REFRESH_INTERVAL_MS);
    return () => window.clearInterval(interval);
  }, [loadSnapshot]);

  const memories = snapshot?.memories || [];
  const artifacts = snapshot?.artifacts || [];
  const messages = sessionState?.messages || snapshot?.messages || [];
  const toolCalls = sessionState?.toolCalls || snapshot?.toolCalls || [];
  const streamingText = sessionState?.streamingTextByMessageId || {};
  const isStreaming = sessionState?.isStreaming || false;
  const tasks = snapshot?.tasks || [];

  return (
    <div className={styles.workspacePage}>
      <WorkspaceHeader
        snapshot={snapshot}
        workspaceId={workspaceId}
        isGenericWorkspace={isGenericWorkspace}
        messages={messages}
        memories={memories}
        artifacts={artifacts}
        navigate={navigate}
        activePanel={activePanel}
        setActivePanel={setActivePanel}
        onOpenWorkspaceSettings={managerAgentId ? openWorkspaceSettings : null}
      />

      {error && <div className={styles.errorBanner}>{error}</div>}

      <WorkspaceAttentionBanner tasks={tasks} />

      <div className={styles.workspaceBody}>
        <div className={`${styles.workspaceMain} ${isSidePanelOpen ? styles.workspaceMainWithPreview : ''}`}>
          <ChatFirstLayout
            sessionId={sessionId}
            messages={messages}
            toolCalls={toolCalls}
            streamingText={streamingText}
            isStreaming={isStreaming}
          />
        </div>

        {snapshot && activePanel && previewEntry && (
          <WorkspaceFilePreviewPanel
            workspaceId={workspaceId}
            kind={previewEntry.kind}
            entry={previewEntry.entry}
            onClose={() => setPreviewEntry(null)}
          />
        )}

        {snapshot && activePanel === 'tasks' && viewingTask && (
          <WorkspaceTaskTranscriptPanel
            task={viewingTask}
            onClose={() => setViewingTask(null)}
          />
        )}

        {snapshot && activePanel && (
          <aside className={styles.workspaceDrawer} aria-label={`${activePanel} drawer`}>
            <div className={styles.workspaceDrawerHeader}>
              <span className={styles.workspaceDrawerTitle}>
                {activePanel.charAt(0).toUpperCase() + activePanel.slice(1)}
              </span>
              <div className={styles.workspaceDrawerActions}>
                {activePanel === 'agents'
                  && snapshot?.kind !== 'agent'
                  && workspaceId !== DEFAULT_WORKSPACE_ID && (
                  <button
                    type="button"
                    className={styles.workspaceDrawerAction}
                    onClick={openMemberCreate}
                    disabled={!!agentBusy}
                  >
                    + Add Agent
                  </button>
                )}
                <button
                  type="button"
                  className={styles.workspaceDrawerClose}
                  onClick={() => {
                    setActivePanel(null);
                    setPreviewEntry(null);
                    setViewingTask(null);
                  }}
                  title="Close panel"
                  aria-label="Close panel"
                >
                  ×
                </button>
              </div>
            </div>

            <div className={styles.workspaceDrawerBody}>
              {activePanel === 'agents' && (
                <WorkspaceAgentsPanel
                  workspaceId={workspaceId}
                  snapshot={snapshot}
                  busy={agentBusy}
                  error={agentError}
                  onOpenCreate={openMemberCreate}
                  onOpenEdit={openAgentEdit}
                  onRemove={handleAgentRemove}
                />
              )}

              {activePanel === 'tasks' && (
                <WorkspaceTasksPanel
                  workspaceId={workspaceId}
                  tasks={tasks}
                  onChanged={() => loadSnapshot(false)}
                  onViewTask={openTaskTranscript}
                />
              )}

              {activePanel === 'memories' && (
                <div className={styles.drawerList}>
                  {memories.length > 0 ? memories.map((entry) => (
                    <button
                      type="button"
                      key={entry.path}
                      className={styles.drawerListItem}
                      onClick={() => openPreviewEntry({ kind: 'memory', entry })}
                    >
                      <div className={styles.drawerListName}>{entry.name}</div>
                      <div className={styles.drawerListMeta}>
                        {entry.path}
                        {entry.updatedAt ? ` · ${formatTimestamp(entry.updatedAt)}` : ''}
                      </div>
                    </button>
                  )) : (
                    <div className={styles.drawerEmpty}>
                      The workspace hasn&apos;t stored anything in memory yet.
                    </div>
                  )}
                </div>
              )}

              {activePanel === 'artifacts' && (
                <div className={styles.drawerList}>
                  {artifacts.length > 0 ? artifacts.map((entry) => (
                    <button
                      type="button"
                      key={entry.path}
                      className={styles.drawerListItem}
                      onClick={() => openPreviewEntry({ kind: 'artifact', entry })}
                    >
                      <div className={styles.drawerListName}>{entry.name}</div>
                      <div className={styles.drawerListMeta}>
                        {entry.path}
                        {entry.updatedAt ? ` · ${formatTimestamp(entry.updatedAt)}` : ''}
                      </div>
                    </button>
                  )) : (
                    <div className={styles.drawerEmpty}>
                      No artifacts in this workspace yet.
                    </div>
                  )}
                </div>
              )}
            </div>
          </aside>
        )}
      </div>

      <AgentFormModal
        isOpen={isFormOpen}
        onClose={handleFormClose}
        onSubmit={handleFormSubmit}
        agent={editingAgent}
        mcpServers={formDeps.mcpServers}
        providerConnections={formDeps.providerConnections}
        skills={formDeps.skills}
        agentTemplates={formDeps.agentTemplates}
        mode={editingAgent?.id && managerAgentId === editingAgent.id ? 'workspace' : 'member'}
        workspaceTitle={snapshot?.title}
      />
    </div>
  );
};

export default Workspace;
