import React, { useEffect, useState } from 'react';
import { assistantClient, useAssistantStore } from '../assistant';
import type { AssistantMessage, ToolInvocation, WorkspaceTaskResponse } from '../generated/bindings';
import ChatMessageList from './AssistantChat/ChatMessageList';
import styles from './WorkspaceTaskTranscriptPanel.module.css';

const TASK_STATUS_LABEL: Record<string, string> = {
  pending: 'Pending',
  running: 'Running',
  completed: 'Completed',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

const EMPTY_MESSAGES: AssistantMessage[] = [];
const EMPTY_TOOL_CALLS: ToolInvocation[] = [];
const EMPTY_STREAMING: Record<string, string> = {};
const MESSAGE_PAGE_LIMIT = 100;

interface WorkspaceTaskTranscriptPanelProps {
  task: WorkspaceTaskResponse | null;
  onClose: () => void;
}

export default function WorkspaceTaskTranscriptPanel({
  task,
  onClose,
}: WorkspaceTaskTranscriptPanelProps) {
  const sessionId = task?.sessionId || null;
  const sessionState = useAssistantStore((state) =>
    sessionId ? state.sessions[sessionId] : null
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
          assistantClient.getSession(sessionId),
          assistantClient.loadSessionMessagesPage({
            sessionId,
            limit: MESSAGE_PAGE_LIMIT,
            includeAncestors: true,
          }),
          assistantClient.listRuns(sessionId),
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
  }, [sessionId]);

  if (!task) return null;

  const statusLabel = TASK_STATUS_LABEL[task.status] || task.status;
  const statusClass = styles[`status_${task.status}`] || '';

  const messages = sessionState?.messages || EMPTY_MESSAGES;
  const toolCalls = sessionState?.toolCalls || EMPTY_TOOL_CALLS;
  const streamingText = sessionState?.streamingTextByMessageId || EMPTY_STREAMING;
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
        sessionId,
        before: current.olderMessageCursor,
        limit: MESSAGE_PAGE_LIMIT,
        includeAncestors: true,
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
          <span className={styles.title} title={task.title}>{task.title}</span>
          <span className={`${styles.statusPill} ${statusClass}`}>{statusLabel}</span>
        </div>
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
        {(task.assignedAgentDisplayName || sessionId) && (
          <div className={styles.bodyMeta}>
            {task.assignedAgentDisplayName && (
              <span>{task.assignedAgentDisplayName}</span>
            )}
            {sessionId && (
              <>
                {task.assignedAgentDisplayName && <span className={styles.sep}>·</span>}
                <span className={styles.sessionId} title={sessionId}>
                  session {sessionId.slice(0, 8)}
                </span>
              </>
            )}
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
              toolCalls={toolCalls}
              streamingText={streamingText}
              isStreaming={isStreaming}
              runStartedAt={sessionState?.runStartedAt ?? null}
              userLabel="Main agent"
              hasOlderMessages={hasOlderMessages}
              isLoadingOlderMessages={isLoadingOlderMessages}
              onLoadOlderMessages={handleLoadOlderMessages}
            />
          </div>
        )}
      </div>
    </aside>
  );
}
