import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { assistantClient, useAssistantStore } from '../assistant';
import type {
  AssistantMessage,
  ToolInvocation,
  WorkspaceAgentResponse,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import ChatMessageList from './AssistantChat/ChatMessageList';
import ChatComposer from './AssistantChat/ChatComposer';
import { resolveAgent, resolveSessionOwner } from './AssistantChat/messageSender';
import AgentAvatar from './Agents/AgentAvatar';
import TaskBreadcrumb from './Agents/TaskBreadcrumb';
import StopChip from './Agents/StopChip';
import TaskStopButton from './Agents/TaskStopButton';
import { taskActivity } from './Agents/TaskList';
import { taskIdentity } from './Agents/agentIdentity';
import { isTaskActive, taskDisplayStatus } from '../utils/taskDisplay';
import { sendWorkspaceTaskMessage } from '../workspace/client';
import { useTaskStore } from '../workspace/taskStore';
import styles from './WorkspaceTaskTranscriptPanel.module.css';

const EMPTY_ROSTER: readonly WorkspaceAgentResponse[] = [];
const EMPTY_MESSAGES: AssistantMessage[] = [];
const EMPTY_TOOL_CALLS: ToolInvocation[] = [];
const EMPTY_IDS: string[] = [];
const EMPTY_STREAMING: Record<string, string> = {};
const MESSAGE_PAGE_LIMIT = 100;

const composerPlaceholder = (task: WorkspaceTaskResponse, agent: string): string =>
  isTaskActive(task)
    ? `Message ${agent}…`
    : 'Messages are unavailable for this task.';

const errorText = (err: unknown): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to send message.';

interface WorkspaceTaskTranscriptPanelProps {
  workspaceId: string;
  task: WorkspaceTaskResponse | null;
  /** The crew, so the header shows the assignee's face; empty is fine. */
  roster?: readonly WorkspaceAgentResponse[];
  onClose: () => void;
  /** Open another task's log from this one: a card in it, or an ancestor. */
  onOpenTask?: (taskId: string) => void;
  /** Return to the log this one was opened from; omit when there is none. */
  onBack?: () => void;
}

export default function WorkspaceTaskTranscriptPanel({
  workspaceId,
  task,
  // A constant, not a fresh `[]` per render: the cards below memoize on it.
  roster = EMPTY_ROSTER,
  onClose,
  onOpenTask,
  onBack,
}: WorkspaceTaskTranscriptPanelProps) {
  const sessionId = task?.sessionId || null;
  const sessionState = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId] : null
  );
  const streamingText = useAssistantStore(
    (state) => (sessionId && state.streamingText[sessionId]) || EMPTY_STREAMING
  );
  // Keyed on the assignee, not the task: the poll hands a new task object
  // every 5s, and a new owner would repaint the whole transcript.
  const assignedToWorkspaceAgentId = task?.assignedToWorkspaceAgentId ?? '';
  const assignedAgentDefinitionId = task?.assignedAgentDefinitionId ?? '';
  const assignedAgentDisplayName = task?.assignedAgentDisplayName ?? '';
  const owner = useMemo(
    () =>
      resolveSessionOwner(roster, {
        assignedToWorkspaceAgentId,
        assignedAgentDefinitionId,
        assignedAgentDisplayName,
      }),
    [roster, assignedToWorkspaceAgentId, assignedAgentDefinitionId, assignedAgentDisplayName]
  );
  const upsertTask = useTaskStore((state) => state.upsertTask);
  const taskId = task?.id ?? '';
  const handleSend = useCallback(
    async (text: string): Promise<{ error?: string }> => {
      try {
        const res = await sendWorkspaceTaskMessage(workspaceId, taskId, text);
        const store = useAssistantStore.getState();
        store.addMessage(res.message.sessionId, res.message);
        // A queued message waits for a run; `queued_messages_delivered` clears
        // the chip.
        if (res.queued) {
          store.markMessageQueued(res.message.sessionId, res.message.id);
        }
        upsertTask(res.task);
        return {};
      } catch (err) {
        return { error: errorText(err) };
      }
    },
    [workspaceId, taskId, upsertTask]
  );
  const [bootstrapError, setBootstrapError] = useState('');
  const [bootstrapping, setBootstrapping] = useState(false);

  // Bootstrap: if the session isn't already in the store, fetch and load it.
  // Live updates afterwards arrive through the global `useAssistantEvents`
  // subscription mounted in MainLayout, so the panel re-renders automatically
  // as new messages and tool calls stream in.
  useEffect(() => {
    if (!sessionId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- async bootstrap: clear UI state on sessionId change; the effect then either resolves synchronously (session is already in the store) or fetches the session and updates bootstrapping/error state on completion. The lint rule cannot model the async load.
      setBootstrapping(false);
      setBootstrapError('');
      return undefined;
    }
    if (useAssistantStore.getState().sessions[sessionId]) {
      setBootstrapping(false);
      setBootstrapError('');
      return undefined;
    }

    let cancelled = false;
    setBootstrapping(true);
    setBootstrapError('');

    const load = async () => {
      try {
        const [session, messagePage, runs] = await Promise.all([
          assistantClient.getSession(workspaceId, sessionId),
          assistantClient.loadSessionMessagesPage({
            workspaceId,
            sessionId,
            limit: MESSAGE_PAGE_LIMIT,
          }),
          assistantClient.listRuns(workspaceId, sessionId),
        ]);
        if (cancelled) return;
        if (!session) {
          setBootstrapError('Session not found.');
          return;
        }
        useAssistantStore
          .getState()
          .loadSessionData(
            sessionId,
            session,
            messagePage.messages || [],
            runs || [],
            messagePage.toolCalls || [],
            undefined,
            messagePage.nextCursor ?? null,
            messagePage.hasMore,
            messagePage.totalCount,
          );
      } catch (err) {
        if (cancelled) return;
        setBootstrapError(
          typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load transcript.'
        );
      } finally {
        if (!cancelled) setBootstrapping(false);
      }
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [sessionId, workspaceId]);

  if (!task) return null;

  const { key: statusKey, label: statusLabel } = taskDisplayStatus(task);
  const agentName = owner ? resolveAgent(owner, roster).name : task.assignedAgentDisplayName;
  const statusClass = styles[`status_${statusKey}`] || '';

  const messages = sessionState?.messages || EMPTY_MESSAGES;
  const toolCalls = sessionState?.toolCalls || EMPTY_TOOL_CALLS;
  const queuedMessageIds = sessionState?.queuedMessageIds || EMPTY_IDS;
  const isStreaming = !!sessionState?.isStreaming;
  const hasOlderMessages = !!sessionState?.hasOlderMessages;
  const isLoadingOlderMessages = !!sessionState?.isLoadingOlderMessages;
  const handleLoadOlderMessages = () => {
    if (!sessionId) return;
    const store = useAssistantStore.getState();
    const current = store.sessions[sessionId];
    if (!current?.hasOlderMessages || !current.olderMessageCursor || current.isLoadingOlderMessages) {
      return;
    }
    store.setOlderMessagesLoading(sessionId, true);
    assistantClient
      .loadSessionMessagesPage({
        workspaceId,
        sessionId,
        before: current.olderMessageCursor,
        limit: MESSAGE_PAGE_LIMIT,
      })
      .then((page) => {
        useAssistantStore
          .getState()
          .prependMessagePage(
            sessionId,
            page.messages,
            page.toolCalls,
            page.nextCursor ?? null,
            page.hasMore,
            page.totalCount
          );
      })
      .catch((err) => {
        console.error('[WorkspaceTaskTranscriptPanel] Failed to load older messages:', err);
        useAssistantStore.getState().setOlderMessagesLoading(sessionId, false);
      });
  };

  return (
    <aside
      className={styles.panel}
      role="region"
      aria-label={`Transcript for ${task.title}`}
    >
      <div className={styles.header}>
        <div className={styles.headerLeft}>
          {onBack && (
            <button
              type="button"
              className={styles.closeButton}
              onClick={onBack}
              aria-label="Back"
              title="Back"
            >
              ←
            </button>
          )}
          <AgentAvatar
            identity={taskIdentity(task, roster)}
            size={20}
            activity={taskActivity(task)}
            label={task.assignedAgentDisplayName}
          />
          <span className={styles.title} title={task.title}>{task.title}</span>
          <span className={`${styles.statusPill} ${statusClass}`}>{statusLabel}</span>
        </div>
        <TaskStopButton
          key={`${task.id}:${task.cycleNo}`}
          task={task}
          label={`Stop task ${task.title}`}
        />
        <button
          type="button"
          className={styles.closeButton}
          onClick={onClose}
          aria-label="Close transcript"
          title="Close"
        >
          ×
        </button>
      </div>

      <div className={styles.body}>
        {task.parentTaskId && onOpenTask && (
          <div className={styles.bodyMeta}>
            <TaskBreadcrumb task={task} onOpenTask={onOpenTask} />
          </div>
        )}
        {!sessionId && (
          <div className={styles.empty}>
            This task has no session transcript — it has not run yet, or its
            session was discarded.
          </div>
        )}
        {sessionId && bootstrapping && messages.length === 0 && (
          <div className={styles.empty}>Loading transcript…</div>
        )}
        {sessionId && bootstrapError && messages.length === 0 && (
          <div className={styles.error}>{bootstrapError}</div>
        )}
        {sessionId && !bootstrapping && !bootstrapError && messages.length === 0 && (
          <div className={styles.empty}>No messages recorded for this session.</div>
        )}
        {sessionId && messages.length > 0 && (
          <div className={styles.transcript}>
            {/* Keyed by session so switching between task transcripts resets
                the scroll to the bottom instead of inheriting the previous
                transcript's position. */}
            <ChatMessageList
              key={sessionId}
              messages={messages}
              workspaceId={workspaceId}
              toolCalls={toolCalls}
              streamingText={streamingText}
              isStreaming={isStreaming}
              // No running footer here: the header face above already shows
              // this agent running, and the store only knows `isStreaming` /
              // `runStartedAt` for runs it saw start live — a transcript
              // opened mid-run showed nothing at all, which read as a bug.
              hasOlderMessages={hasOlderMessages}
              isLoadingOlderMessages={isLoadingOlderMessages}
              onLoadOlderMessages={handleLoadOlderMessages}
              // A sub-agent can delegate further; its task cards get faces
              // from the same crew and open the child's log, with Back.
              taskRoster={roster}
              onOpenTask={onOpenTask}
              owner={owner}
              queuedMessageIds={queuedMessageIds}
            />
          </div>
        )}
        <div className={styles.chips}>
          <StopChip status={task.status} />
        </div>
      </div>
      <ChatComposer
        // Remount per task so a draft never carries over to another task.
        key={task.id}
        placeholder={composerPlaceholder(task, agentName)}
        disabled={!isTaskActive(task)}
        ariaLabel={`Message ${agentName} about ${task.title}`}
        onSend={handleSend}
      />
    </aside>
  );
}
