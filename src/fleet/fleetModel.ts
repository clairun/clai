/**
 * The Fleet's state: one card per workspace, built by a pure reducer over
 * the app-global event streams (assistant, approvals, path grants) and a
 * startup snapshot. Streaming variants (deltas, tool calls, message updates)
 * return the state untouched, so they never reach React. Runs and tasks are
 * not copied here: they come from `fleetActivityStore` and `taskStore`
 * (`FleetLive`).
 *
 * What a card shows follows the owner's rules: live activity is highlighted,
 * finished subtasks and resolved requests stay as low-emphasis history, the
 * last assistant message appears only once the workspace has stopped, and
 * the user's own message shows until the next message or event arrives.
 */
import type {
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantUiEvent,
  FleetWorkspace,
  PathGrantRequest,
  PermissionRequest,
  TaskStatus,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import { taskDisplayStatus } from '../utils/taskDisplay';
import { buildTaskTree, type TaskNode } from '../utils/taskTree';

/** A workspace with no event in this window and nothing live gets no card. */
export const RECENT_WINDOW_MS = 12 * 60 * 60 * 1000;
/** Resolved items a card keeps on display. */
export const HISTORY_LIMIT = 12;
const NOTE_CHARS = 600;

export type FleetRequestKind = 'approval' | 'grant';

/** Something the workspace waits on the user for: a question, a command approval or a path grant. */
export interface FleetRequest {
  kind: FleetRequestKind | 'ask';
  text: string;
  agentName: string | null;
  at: number;
  resolvedAt: number | null;
}

export interface FleetNote {
  id: string;
  text: string;
  at: number;
}

export interface FleetCard {
  workspaceId: string;
  title: string;
  /** From the snapshot, or set when the user sends or clears from the terminal. */
  mainSessionId: string | null;
  lastActivityAt: number;
  /** By `kind:id`. */
  requests: Record<string, FleetRequest>;
  lastMessage: FleetNote | null;
  userMessage: FleetNote | null;
}

export interface FleetState {
  cards: Record<string, FleetCard>;
}

/** What the other stores know: in-flight runs (any session) and tasks, by workspace. */
export interface FleetLive {
  runs: Record<string, readonly string[]>;
  tasks: Record<string, Record<string, WorkspaceTaskResponse>>;
}

export const EMPTY_FLEET: FleetState = { cards: {} };

export const emptyCard = (workspaceId: string, title = ''): FleetCard => ({
  workspaceId,
  title,
  mainSessionId: null,
  lastActivityAt: 0,
  requests: {},
  lastMessage: null,
  userMessage: null,
});

const updateCard = (
  state: FleetState,
  workspaceId: string,
  update: (card: FleetCard) => FleetCard
): FleetState => {
  const current = state.cards[workspaceId] ?? emptyCard(workspaceId);
  const next = update(current);
  if (next === current && state.cards[workspaceId]) return state;
  return { cards: { ...state.cards, [workspaceId]: next } };
};

const bump = (card: FleetCard, at: number): FleetCard =>
  at > card.lastActivityAt ? { ...card, lastActivityAt: at } : card;

const clip = (text: string): string => {
  const trimmed = text.trim();
  return trimmed.length > NOTE_CHARS ? `${trimmed.slice(0, NOTE_CHARS - 1)}…` : trimmed;
};

export const messageText = (message: AssistantMessage): string =>
  clip(
    message.content
      .map((part) => (part.type === 'text' ? part.text : ''))
      .filter(Boolean)
      .join('\n')
  );

export const isTaskLive = (status: TaskStatus): boolean =>
  status === 'queued' || status === 'running' || status === 'stopping';

/** A message or event the card displays arrived: the user's own message gives way. */
const dropUserMessage = (card: FleetCard): FleetCard =>
  card.userMessage ? { ...card, userMessage: null } : card;

const requestKey = (kind: FleetRequest['kind'], id: string) => `${kind}:${id}`;

const resolve = (card: FleetCard, key: string, at: number): FleetCard => {
  const request = card.requests[key];
  if (!request || request.resolvedAt != null) return card;
  return { ...card, requests: { ...card.requests, [key]: { ...request, resolvedAt: at } } };
};

const askRequest = (question: string, at: number): FleetRequest => ({
  kind: 'ask',
  text: question,
  agentName: null,
  at,
  resolvedAt: null,
});

/** The variants the Fleet reads. Everything else, streaming above all, is ignored. */
const FLEET_EVENTS: ReadonlySet<AssistantUiEvent['type']> = new Set([
  'message_created',
  'message_deleted',
  'run_queued',
  'run_started',
  'run_completed',
  'run_failed',
  'run_cancelled',
  'assistant_message_completed',
  'ask_user_requested',
  'ask_user_resolved',
  'task_updated',
]);

export const isFleetEvent = (envelope: AssistantEventEnvelope): boolean =>
  FLEET_EVENTS.has(envelope.event.type);

export const applyAssistantEvent = (
  state: FleetState,
  envelope: AssistantEventEnvelope
): FleetState => {
  const { event, sessionId, workspaceId } = envelope;
  if (!FLEET_EVENTS.has(event.type)) return state;
  const at = Number(envelope.timestamp);

  return updateCard(state, workspaceId, (current) => {
    const card = bump(current, at);
    const inMain = card.mainSessionId === sessionId;
    switch (event.type) {
      case 'message_created': {
        const { message } = event.payload;
        if (!inMain) return card;
        return message.role === 'user' && message.origin.kind === 'user'
          ? { ...card, userMessage: { id: message.id, text: messageText(message), at } }
          : dropUserMessage(card);
      }
      case 'message_deleted':
        return card.userMessage?.id === event.payload.message_id ? { ...card, userMessage: null } : card;
      case 'assistant_message_completed': {
        if (!inMain) return card;
        const text = messageText(event.payload.message);
        const next = dropUserMessage(card);
        return text ? { ...next, lastMessage: { id: event.payload.message.id, text, at } } : next;
      }
      case 'ask_user_requested':
        return {
          ...dropUserMessage(card),
          requests: {
            ...card.requests,
            [requestKey('ask', event.payload.pending_id)]: askRequest(event.payload.question, at),
          },
        };
      case 'ask_user_resolved':
        return resolve(card, requestKey('ask', event.payload.pending_id), at);
      case 'task_updated':
        return dropUserMessage(card);
      default:
        // Run lifecycle: activity, but not a displayed event.
        return card;
    }
  });
};

const requestText = (request: PermissionRequest | PathGrantRequest): string =>
  'command' in request
    ? request.command
    : `${request.requestedAccess === 'read_write' ? 'Write' : 'Read'} ${request.requestedPath}`;

const toRequest = (
  kind: FleetRequestKind,
  request: PermissionRequest | PathGrantRequest,
  at: number
): FleetRequest => ({
  kind,
  text: requestText(request),
  agentName: request.agentName,
  at,
  resolvedAt: null,
});

export const applyRequest = (
  state: FleetState,
  kind: FleetRequestKind,
  request: PermissionRequest | PathGrantRequest,
  at: number
): FleetState => {
  if (!request.workspaceId) return state;
  const key = requestKey(kind, request.requestId);
  return updateCard(state, request.workspaceId, (card) =>
    card.requests[key]
      ? card
      : {
          ...dropUserMessage(bump(card, at)),
          requests: { ...card.requests, [key]: toRequest(kind, request, at) },
        }
  );
};

export const applyRequestResolved = (
  state: FleetState,
  kind: FleetRequestKind,
  requestId: string,
  at: number
): FleetState => {
  const key = requestKey(kind, requestId);
  const owner = Object.values(state.cards).find((card) => card.requests[key]?.resolvedAt === null);
  if (!owner) return state;
  return updateCard(state, owner.workspaceId, (card) => resolve(card, key, at));
};

/** The user sent `message` (or cleared the chat, `message` null) on `sessionId`: it is the main. */
export const applyUserSent = (
  state: FleetState,
  workspaceId: string,
  sessionId: string,
  message: AssistantMessage | null,
  at: number
): FleetState =>
  updateCard(state, workspaceId, (card) => ({
    ...bump(card, at),
    mainSessionId: sessionId,
    userMessage: message ? { id: message.id, text: messageText(message), at } : null,
  }));

/** Requests still open in `current` but absent from `open` were resolved meanwhile; new open ones are added. */
const reconcile = (
  current: Record<string, FleetRequest>,
  open: Record<string, FleetRequest>,
  now: number
): Record<string, FleetRequest> => {
  const next = { ...open };
  for (const [key, request] of Object.entries(current)) {
    next[key] = request.resolvedAt == null && !open[key] ? { ...request, resolvedAt: now } : request;
  }
  return next;
};

/**
 * Apply a snapshot. The Fleet becomes exactly the snapshot's workspaces, so a
 * deleted one (or one no longer among the most recent) loses its card;
 * history the store already holds for the others is kept. Events buffered
 * while it loaded are replayed afterwards by the store.
 */
export const seedFleet = (
  state: FleetState,
  snapshot: readonly FleetWorkspace[],
  now: number
): FleetState => {
  const cards: FleetState['cards'] = {};
  for (const ws of snapshot) {
    const card = state.cards[ws.workspaceId] ?? emptyCard(ws.workspaceId);
    const activity = Number(ws.lastActivityAt);
    const preview = ws.lastAssistantMessage;
    const lastMessage: FleetNote | null = preview
      ? { id: preview.messageId, text: preview.truncated ? `${preview.text}…` : preview.text, at: Number(preview.createdAt) }
      : null;
    // Approvals and grants carry no timestamp; the workspace's activity orders them among waiting cards.
    const open: Record<string, FleetRequest> = {};
    for (const a of ws.pendingAsks) open[requestKey('ask', a.pendingId)] = askRequest(a.question, Number(a.requestedAt));
    for (const r of ws.pendingPermissions) open[requestKey('approval', r.requestId)] = toRequest('approval', r, activity);
    for (const r of ws.pendingPathGrants) open[requestKey('grant', r.requestId)] = toRequest('grant', r, activity);
    cards[ws.workspaceId] = {
      ...card,
      title: ws.title || card.title,
      mainSessionId: ws.mainSessionId ?? card.mainSessionId,
      lastActivityAt: Math.max(card.lastActivityAt, activity),
      lastMessage:
        lastMessage && (!card.lastMessage || lastMessage.at >= card.lastMessage.at) ? lastMessage : card.lastMessage,
      requests: reconcile(card.requests, open, now),
    };
  }
  return { cards };
};

// ── Derived views ─────────────────────────────────────────────────────────

export type FleetPhase = 'waiting' | 'working' | 'stopped';

export interface FleetAttention {
  asks: number;
  approvals: number;
  grants: number;
  total: number;
  /** When the oldest open item appeared; orders waiting cards. */
  since: number | null;
}

export const cardAttention = (card: FleetCard): FleetAttention => {
  const open = Object.values(card.requests).filter((r) => r.resolvedAt == null);
  const count = (kind: FleetRequest['kind']) => open.filter((r) => r.kind === kind).length;
  return {
    asks: count('ask'),
    approvals: count('approval'),
    grants: count('grant'),
    total: open.length,
    since: open.length ? Math.min(...open.map((r) => r.at)) : null,
  };
};

const workspaceTasks = (card: FleetCard, live: FleetLive): WorkspaceTaskResponse[] =>
  Object.values(live.tasks[card.workspaceId] ?? {});

/** Working: any run in the workspace, main or not, or any live task. */
export const cardPhase = (card: FleetCard, live: FleetLive): FleetPhase => {
  if (cardAttention(card).total > 0) return 'waiting';
  if ((live.runs[card.workspaceId]?.length ?? 0) > 0) return 'working';
  return workspaceTasks(card, live).some((t) => isTaskLive(t.status)) ? 'working' : 'stopped';
};

export type FleetItemTone = 'attention' | 'live' | 'done' | 'failed';

export interface FleetItem {
  key: string;
  text: string;
  status: string;
  /** `taskStatus` pill key, for tasks. */
  statusKey?: string;
  tone: FleetItemTone;
  agents: { name: string; seed: string }[];
  at: number;
}

const TONE_RANK: Record<FleetItemTone, number> = { attention: 0, live: 1, failed: 2, done: 2 };

const agentOf = (task: WorkspaceTaskResponse) => ({
  name: task.assignedAgentDisplayName,
  seed: task.assignedAgentDefinitionId || task.assignedToWorkspaceAgentId,
});

const subtree = (node: TaskNode): WorkspaceTaskResponse[] => [node.task, ...node.children.flatMap(subtree)];

/** One item per top-level task, live while anything in its subtree is. */
const taskItems = (tasks: WorkspaceTaskResponse[]): FleetItem[] =>
  buildTaskTree(tasks).map((node) => {
    const root = node.task;
    const group = subtree(node);
    const live = group.some((t) => isTaskLive(t.status));
    const agents: FleetItem['agents'] = [];
    for (const t of group) {
      const agent = agentOf(t);
      if (!agents.some((a) => a.seed === agent.seed)) agents.push(agent);
    }
    const display =
      live && !isTaskLive(root.status) ? { key: 'running', label: 'Subtasks running' } : taskDisplayStatus(root);
    return {
      key: `task:${root.id}`,
      text: root.title,
      status: display.label,
      statusKey: display.key,
      tone: live ? 'live' : root.status === 'failed' ? 'failed' : 'done',
      agents,
      at: Math.max(...group.map((t) => Number(t.updatedAt))),
    } satisfies FleetItem;
  });

const OPEN_STATUS: Record<FleetRequest['kind'], string> = {
  ask: 'Question',
  approval: 'Approve command',
  grant: 'Grant access',
};

/**
 * Every live item plus the `HISTORY_LIMIT` newest resolved ones, in reading
 * order like a chat: history oldest first, then work in progress, then what
 * needs the user, so the bottom of the card is always the most current.
 */
export const cardItems = (card: FleetCard, live: FleetLive): FleetItem[] => {
  const items: FleetItem[] = taskItems(workspaceTasks(card, live));
  for (const [key, request] of Object.entries(card.requests)) {
    const open = request.resolvedAt == null;
    items.push({
      key,
      text: request.text,
      status: open ? OPEN_STATUS[request.kind] : request.kind === 'ask' ? 'Answered' : 'Resolved',
      tone: open ? 'attention' : 'done',
      agents: request.agentName ? [{ name: request.agentName, seed: request.agentName }] : [],
      at: request.resolvedAt ?? request.at,
    });
  }
  items.sort((a, b) => TONE_RANK[a.tone] - TONE_RANK[b.tone] || b.at - a.at);
  const liveCount = items.filter((i) => TONE_RANK[i.tone] < 2).length;
  return items
    .slice(0, liveCount + HISTORY_LIMIT)
    .sort((a, b) => TONE_RANK[b.tone] - TONE_RANK[a.tone] || a.at - b.at);
};

/** The last assistant message, only once the workspace has stopped. */
export const visibleLastMessage = (card: FleetCard, live: FleetLive): FleetNote | null =>
  cardPhase(card, live) === 'stopped' ? card.lastMessage : null;

// ── Which cards, where ───────────────────────────────────────────────────

export const CARD_MIN_WIDTH = 320;
export const CARD_MIN_HEIGHT = 200;
export const GRID_GAP = 16;
const MAX_COLS = 4;
const MAX_ROWS = 3;

export interface GridCapacity {
  cols: number;
  rows: number;
}

export const gridCapacity = (width: number, height: number): GridCapacity => ({
  cols: Math.min(MAX_COLS, Math.max(1, Math.floor((width + GRID_GAP) / (CARD_MIN_WIDTH + GRID_GAP)))),
  rows: Math.min(MAX_ROWS, Math.max(1, Math.floor((height + GRID_GAP) / (CARD_MIN_HEIGHT + GRID_GAP)))),
});

/**
 * The visible cards, in slot order. Eligible: the selected card, anything
 * live, and workspaces active within `RECENT_WINDOW_MS`. When they don't all
 * fit, the selected card wins, then waiting ones (oldest first), then the
 * most recent. Cards already visible keep their slots (`prevOrder`) and
 * newcomers take the next ones, so nothing moves to highlight a card.
 */
export const pickCards = (
  cards: readonly FleetCard[],
  options: { now: number; capacity: GridCapacity; selectedId: string | null; prevOrder: readonly string[]; live: FleetLive }
): string[] => {
  const { now, capacity, selectedId, prevOrder, live } = options;
  const meta = cards
    .map((card) => ({ id: card.workspaceId, card, phase: cardPhase(card, live), since: cardAttention(card).since ?? 0 }))
    .filter(
      (m) => m.id === selectedId || m.phase !== 'stopped' || m.card.lastActivityAt >= now - RECENT_WINDOW_MS
    );
  const rank = (m: (typeof meta)[number]): [number, number] =>
    m.id === selectedId ? [0, 0] : m.phase === 'waiting' ? [1, m.since] : [2, -m.card.lastActivityAt];
  const chosen = new Set(
    meta
      .sort((a, b) => {
        const [ra, ka] = rank(a);
        const [rb, kb] = rank(b);
        return ra - rb || ka - kb;
      })
      .slice(0, capacity.cols * capacity.rows)
      .map((m) => m.id)
  );
  const kept = prevOrder.filter((id) => chosen.has(id));
  return [...kept, ...meta.filter((m) => chosen.has(m.id) && !kept.includes(m.id)).map((m) => m.id)];
};

/** The card `step` slots after `selectedId` (±1), wrapping; from no selection, the first or the last. */
export const cycleCard = (order: readonly string[], selectedId: string | null, step: 1 | -1): string | null => {
  if (order.length === 0) return null;
  const at = selectedId ? order.indexOf(selectedId) : -1;
  if (at < 0) return step > 0 ? order[0]! : order[order.length - 1]!;
  return order[(at + step + order.length) % order.length]!;
};

/** One of eight hues per workspace, stable across runs. */
export const workspaceHue = (workspaceId: string): number => {
  let hash = 0;
  for (let i = 0; i < workspaceId.length; i += 1) hash = (hash * 31 + workspaceId.charCodeAt(i)) | 0;
  const HUES = [200, 160, 28, 330, 260, 90, 300, 12];
  return HUES[Math.abs(hash) % HUES.length]!;
};
