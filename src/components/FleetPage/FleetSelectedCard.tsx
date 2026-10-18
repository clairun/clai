import { useCallback, useEffect, useMemo } from 'react';
import { useNavigate } from 'react-router';
import { useAssistantStore } from '../../assistant';
import type { AgentIdentity } from '../Agents/agentIdentity';
import { markWorkspaceOpened } from '../../workspace/client';
import {
  cardAttention,
  cardPhase,
  fullMessageText,
  requestScene,
  type FleetCard,
  type FleetLive,
  type FleetLoudness,
  type FleetScene as Scene,
} from '../../fleet/fleetModel';
import type { AssistantMessage, AssistantRun } from '../../generated/bindings';
import { LiveFaces, PhaseLabel, stickerFor } from './FleetCard';
import FleetRequestPanel from './FleetRequestPanel';
import FleetScene, { type StageExtras } from './FleetScene';
import { useFleetChat } from './useFleetChat';
import styles from './FleetPage.module.css';

const EMPTY_MESSAGES: AssistantMessage[] = [];

/** Main's last finished message in full; the card keeps a clipped copy. */
const lastAssistantText = (messages: readonly AssistantMessage[]): { id: string; text: string } | null => {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i]!;
    if (message.role !== 'assistant') continue;
    const text = fullMessageText(message);
    if (text) return { id: message.id, text };
  }
  return null;
};

interface FleetSelectedCardProps {
  card: FleetCard;
  /** What the panel says, in full (`cardScene` with `stage`); `null` while it shows an open request. */
  scene: Scene | null;
  /** Keys of the rows that shout (`fleetLoudness`). */
  shouts: ReadonlySet<string>;
  live: FleetLive;
  selected: boolean;
  /** Picked agent faces, by agent definition id. */
  faces: Record<string, AgentIdentity>;
  /** The page's clock: the last Fleet event or the minute tick, whichever is later. */
  now: number;
  /** Drawn loud: never while selected. */
  loud?: FleetLoudness;
  onSelect: (workspaceId: string | null) => void;
}

/**
 * The stage card: the same comic panel as a small card, with nothing left
 * out. Every running task, every result in full, Main's last message as
 * markdown and, in Earlier, the workspace's 50 most recent tasks (what
 * `list_recent_tasks` loads; older ones are not paged in), in the order they
 * happened; it scrolls, kept at the newest. An open request
 * takes the panel, as on a small card, with every decision the chat offers.
 */
const FleetSelectedCard = ({ card, scene, shouts, live, selected, faces, now, loud = null, onSelect }: FleetSelectedCardProps) => {
  const navigate = useNavigate();
  const { workspaceId, mainSessionId: sessionId } = card;
  const { error, loading } = useFleetChat(workspaceId, sessionId);
  const session = useAssistantStore((s) => (sessionId ? s.sessions[sessionId] : undefined));
  const streaming = useAssistantStore((s) => (sessionId ? s.streamingText[sessionId] : undefined));
  const messages = session?.messages ?? EMPTY_MESSAGES;
  const isStreaming = !!session?.isStreaming;
  const lastRun = (session?.runs ?? []).reduce<AssistantRun | null>(
    (latest, run) => (!latest || run.startedAt > latest.startedAt ? run : latest),
    null
  );
  const runError = !isStreaming && lastRun?.status === 'failed' ? lastRun.error?.trim() || 'The run failed.' : null;

  // Selected is seen: the rail's unread dot clears, as on the Workspace page
  // (see the same stamp there), and again on leaving while a run is in flight.
  useEffect(() => {
    if (!selected) return undefined;
    markWorkspaceOpened(workspaceId).catch(() => {});
    return () => {
      markWorkspaceOpened(workspaceId).catch(() => {});
    };
  }, [selected, workspaceId, isStreaming]);
  const onOpenTask = useCallback(
    (taskId: string) => navigate(`/workspace/${encodeURIComponent(workspaceId)}`, { state: { openTaskId: taskId } }),
    [navigate, workspaceId]
  );

  const request = useMemo(() => requestScene(card, live), [card, live]);
  const last = useMemo(() => lastAssistantText(messages), [messages]);
  const streamed = streaming ? Object.values(streaming).join('').trim() : '';
  // The session's copy is the card's message in full, unless an event brought a newer one.
  const message = last && (!card.lastMessage || card.lastMessage.id === last.id) ? last.text : (card.lastMessage?.text ?? null);
  const stage: StageExtras = { onOpenTask, message, streaming: isStreaming && streamed ? streamed : null };

  const title = card.title || 'Workspace';
  const phase = cardPhase(card, live);

  return (
    <div className={styles.cardInner} onClick={selected ? undefined : () => onSelect(workspaceId)}>
      <header className={styles.cardHeader}>
        <h2 className={styles.selectedTitle}>
          {selected ? (
            title
          ) : (
            <button
              type="button"
              className={styles.cardTitle}
              onClick={(e) => {
                e.stopPropagation();
                onSelect(workspaceId);
              }}
              aria-label={`Select ${title}`}
            >
              {title}
            </button>
          )}
        </h2>
        <LiveFaces card={card} live={live} faces={faces} />
        <PhaseLabel phase={phase} attention={cardAttention(card)} sticker={stickerFor(loud, request)} />
        <button
          type="button"
          className={styles.headerButton}
          onClick={() => navigate(`/workspace/${encodeURIComponent(workspaceId)}`)}
        >
          Open workspace
        </button>
        {selected && (
          <button type="button" className={styles.closeButton} onClick={() => onSelect(null)} aria-label="Deselect workspace" title="Deselect">
            ×
          </button>
        )}
      </header>
      {request ? (
        <FleetRequestPanel scene={request} workspaceId={workspaceId} faces={faces} loud={loud === 'needs'} now={now} stage />
      ) : (
        scene && <FleetScene scene={scene} shouts={shouts} now={now} faces={faces} stage={stage} />
      )}
      {runError && <p className={styles.runError}>Main's last run failed: {runError}</p>}
      {error && <p className={styles.nothing}>{error}</p>}
      {loading && !scene?.toYou && !request && <p className={styles.nothing}>Loading conversation…</p>}
    </div>
  );
};

export default FleetSelectedCard;
