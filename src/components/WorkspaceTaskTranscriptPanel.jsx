import React, { useEffect, useState } from 'react';
import { assistantClient, useAssistantStore } from '../assistant';
import ChatMessageList from './AssistantChat/ChatMessageList';
import styles from './WorkspaceTaskTranscriptPanel.module.css';

const TASK_STATUS_LABEL = {
  pending: 'Pending',
  running: 'Running',
  completed: 'Completed',
  failed: 'Failed',
  needs_user_input: 'Needs input',
  cancelled: 'Cancelled',
};

const EMPTY_MESSAGES = [];
const EMPTY_TOOL_CALLS = [];
const EMPTY_STREAMING = {};

export default function WorkspaceTaskTranscriptPanel({ task, onClose }) {
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
        const [session, messages, runs, toolCalls] = await Promise.all([
          assistantClient.getSession(sessionId),
          assistantClient.loadSessionMessages(sessionId),
          assistantClient.listRuns(sessionId),
          assistantClient.listToolCalls(sessionId, null),
        ]);
        if (cancelled) return;
        if (!session) {
          setBootstrapError('Session not found.');
          return;
        }
        useAssistantStore
          .getState()
          .loadSessionData(sessionId, session, messages || [], runs || [], toolCalls || []);
      } catch (err) {
        if (cancelled) return;
        setBootstrapError(
          typeof err === 'string' ? err : err?.message || 'Failed to load transcript.'
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
            <ChatMessageList
              messages={messages}
              toolCalls={toolCalls}
              streamingText={streamingText}
              isStreaming={isStreaming}
              userLabel="Main agent"
            />
          </div>
        )}
      </div>
    </aside>
  );
}
