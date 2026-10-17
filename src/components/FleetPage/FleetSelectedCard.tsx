import { useCallback, useMemo } from 'react';
import { useNavigate } from 'react-router';
import { assistantClient, useAssistantStore } from '../../assistant';
import ChatMessageList from '../AssistantChat/ChatMessageList';
import { resolveSessionOwner } from '../AssistantChat/messageSender';
import AskUserPanel from '../AskUserPanel/AskUserPanel';
import InlineApprovalCard from '../InlineApprovalCard';
import InlinePathGrantCard from '../InlinePathGrantCard';
import { mainIdentity } from '../Agents/agentIdentity';
import { cardAttention, cardPhase, type FleetCard } from '../../fleet/fleetModel';
import type { AssistantMessage, AssistantRun, ToolInvocation } from '../../generated/bindings';
import { phaseLabel } from './FleetCard';
import { useFleetChat } from './useFleetChat';
import styles from './FleetPage.module.css';

const MAIN_IDENTITY = mainIdentity();
const EMPTY_MESSAGES: AssistantMessage[] = [];
const EMPTY_TOOL_CALLS: ToolInvocation[] = [];
const EMPTY_IDS: string[] = [];
const EMPTY_STREAMING: Record<string, string> = {};
const EMPTY_RUNS: AssistantRun[] = [];

interface FleetSelectedCardProps {
  card: FleetCard;
  onClose: () => void;
}

/** The selected card: the workspace's chat, in full, with its pending requests. */
const FleetSelectedCard = ({ card, onClose }: FleetSelectedCardProps) => {
  const navigate = useNavigate();
  const { workspaceId, mainSessionId: sessionId } = card;
  const { roster, error, loading } = useFleetChat(workspaceId, sessionId);
  const owner = useMemo(() => resolveSessionOwner(roster), [roster]);
  const session = useAssistantStore((s) => (sessionId ? s.sessions[sessionId] : undefined));
  const streamingText = useAssistantStore(
    (s) => (sessionId && s.streamingText[sessionId]) || EMPTY_STREAMING
  );
  const messages = session?.messages ?? EMPTY_MESSAGES;
  const runs = session?.runs ?? EMPTY_RUNS;
  const lastRun = runs.reduce<AssistantRun | null>(
    (latest, run) => (!latest || run.startedAt > latest.startedAt ? run : latest),
    null
  );
  const runError =
    lastRun?.status === 'failed' ? lastRun.error?.trim() || 'The run failed.' : null;

  const handleDeleteQueued = useCallback(
    (messageId: string) => {
      if (!sessionId) return;
      assistantClient.deleteQueuedMessage(workspaceId, sessionId, messageId).catch((err) => {
        console.error('[Fleet] Failed to delete queued message:', err);
      });
    },
    [sessionId, workspaceId]
  );
  const handleEditQueued = useCallback(
    async (messageId: string, text: string) => {
      if (!sessionId) return;
      try {
        await assistantClient.editQueuedMessage(workspaceId, sessionId, messageId, text);
      } catch (err) {
        console.error('[Fleet] Failed to edit queued message:', err);
      }
    },
    [sessionId, workspaceId]
  );
  const handleLoadOlder = useCallback(() => {
    if (!sessionId) return;
    const store = useAssistantStore.getState();
    const current = store.sessions[sessionId];
    if (!current?.hasOlderMessages || !current.olderMessageCursor || current.isLoadingOlderMessages) {
      return;
    }
    store.setOlderMessagesLoading(sessionId, true);
    assistantClient
      .loadSessionMessagesPage({ workspaceId, sessionId, before: current.olderMessageCursor, limit: 50 })
      .then((page) =>
        useAssistantStore
          .getState()
          .prependMessagePage(sessionId, page.messages, page.toolCalls, page.nextCursor ?? null, page.hasMore, page.totalCount)
      )
      .catch((err) => {
        console.error('[Fleet] Failed to load older messages:', err);
        useAssistantStore.getState().setOlderMessagesLoading(sessionId, false);
      });
  }, [sessionId, workspaceId]);

  const title = card.title || 'Workspace';

  return (
    <div className={styles.cardInner}>
      <header className={styles.cardHeader}>
        <h2 className={styles.selectedTitle}>{title}</h2>
        <span className={styles.phase} data-phase={cardPhase(card)}>
          {phaseLabel(cardPhase(card), cardAttention(card))}
        </span>
        <button
          type="button"
          className={styles.headerButton}
          onClick={() => navigate(`/workspace/${encodeURIComponent(workspaceId)}`)}
        >
          Open workspace
        </button>
        <button type="button" className={styles.closeButton} onClick={onClose} aria-label="Deselect workspace" title="Deselect">
          ×
        </button>
      </header>
      <div className={styles.chat}>
        {messages.length > 0 ? (
          <ChatMessageList
            key={workspaceId}
            workspaceId={workspaceId}
            messages={messages}
            toolCalls={session?.toolCalls ?? EMPTY_TOOL_CALLS}
            streamingText={streamingText}
            isStreaming={!!session?.isStreaming}
            runError={runError}
            runStartedAt={session?.runStartedAt ?? null}
            runningIdentity={MAIN_IDENTITY}
            queuedMessageIds={session?.queuedMessageIds ?? EMPTY_IDS}
            onDeleteQueuedMessage={handleDeleteQueued}
            onEditQueuedMessage={handleEditQueued}
            hasOlderMessages={!!session?.hasOlderMessages}
            isLoadingOlderMessages={!!session?.isLoadingOlderMessages}
            onLoadOlderMessages={handleLoadOlder}
            taskRoster={roster}
            owner={owner}
          />
        ) : (
          <p className={styles.chatEmpty}>
            {error ?? (loading ? 'Loading conversation…' : 'No messages yet. Type below to talk to this workspace.')}
          </p>
        )}
        <AskUserPanel sessionId={sessionId} />
        <InlineApprovalCard workspaceId={workspaceId} />
        <InlinePathGrantCard workspaceId={workspaceId} />
      </div>
    </div>
  );
};

export default FleetSelectedCard;
