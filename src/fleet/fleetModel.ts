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
  AskUserOption,
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantUiEvent,
  FleetWorkspace,
  PathGrantRequest,
  PendingAsk,
  PermissionRequest,
  TaskStatus,
  WorkspaceTaskResponse,
} from '../generated/bindings';

/** A workspace with no event in this window and nothing live gets no card. */
export const RECENT_WINDOW_MS = 12 * 60 * 60 * 1000;
/** Resolved items a card keeps on display. */
export const HISTORY_LIMIT = 12;
const NOTE_CHARS = 600;

export interface FleetAsk {
  pendingId: string;
  sessionId: string;
  question: string;
  options: AskUserOption[] | null;
  multiSelect: boolean;
  extraContext: string | null;
  at: number;
  resolvedAt: number | null;
}

export type FleetRequestKind = 'approval' | 'grant';

export interface FleetRequest {
  id: string;
  kind: FleetRequestKind;
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
  asks: Record<string, FleetAsk>;
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
  asks: {},
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
      case 'ask_user_requested': {
        const p = event.payload;
        return {
          ...dropUserMessage(card),
          asks: {
            ...card.asks,
            [p.pending_id]: {
              pendingId: p.pending_id,
              sessionId,
              question: p.question,
              options: p.options,
              multiSelect: p.multi_select,
              extraContext: p.extra_context ?? null,
              at,
              resolvedAt: null,
            },
          },
        };
      }
      case 'ask_user_resolved': {
        const ask = card.asks[event.payload.pending_id];
        if (!ask || ask.resolvedAt != null) return card;
        return { ...card, asks: { ...card.asks, [ask.pendingId]: { ...ask, resolvedAt: at } } };
      }
      case 'task_updated':
        return dropUserMessage(card);
      default:
        // Run lifecycle: activity, but not a displayed event.
        return card;
    }
  });
};

const requestKey = (kind: FleetRequestKind, id: string) => `${kind}:${id}`;

const requestText = (request: PermissionRequest | PathGrantRequest): string =>
  'command' in request
    ? request.command
    : `${request.requestedAccess === 'read_write' ? 'Write' : 'Read'} ${request.requestedPath}`;

const toRequest = (
  kind: FleetRequestKind,
  request: PermissionRequest | PathGrantRequest,
  at: number
): FleetRequest => ({
  id: request.requestId,
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
  return updateCard(state, owner.workspaceId, (card) => ({
    ...card,
    requests: { ...card.requests, [key]: { ...card.requests[key]!, resolvedAt: at } },
  }));
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

export const askFromPending = (ask: PendingAsk): FleetAsk => ({
  pendingId: ask.pendingId,
  sessionId: ask.sessionId,
  question: ask.question,
  options: ask.options ?? null,
  multiSelect: ask.multiSelect,
  extraContext: ask.extraContext ?? null,
  at: Number(ask.requestedAt),
  resolvedAt: null,
});

/** Items still open in `current` but absent from `open` were resolved meanwhile; new open items are added. */
const reconcile = <T extends { resolvedAt: number | null }>(
  current: Record<string, T>,
  open: Record<string, T>,
  now: number
): Record<string, T> => {
  const next = { ...open };
  for (const [key, item] of Object.entries(current)) {
    next[key] = item.resolvedAt == null && !open[key] ? { ...item, resolvedAt: now } : item;
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
    // A request carries no timestamp; the workspace's activity orders it among waiting cards.
    const requests = [
      ...ws.pendingPermissions.map((r) => toRequest('approval', r, activity)),
      ...ws.pendingPathGrants.map((r) => toRequest('grant', r, activity)),
    ];
    cards[ws.workspaceId] = {
      ...card,
      title: ws.title || card.title,
      mainSessionId: ws.mainSessionId ?? card.mainSessionId,
      lastActivityAt: Math.max(card.lastActivityAt, activity),
      lastMessage:
        lastMessage && (!card.lastMessage || lastMessage.at >= card.lastMessage.at) ? lastMessage : card.lastMessage,
      asks: reconcile(card.asks, Object.fromEntries(ws.pendingAsks.map((a) => [a.pendingId, askFromPending(a)])), now),
      requests: reconcile(card.requests, Object.fromEntries(requests.map((r) => [requestKey(r.kind, r.id), r])), now),
    };
  }
  return { cards };
};

/** Titles from `listWorkspaces()`; a card missing from the list belongs to a deleted workspace. */
export const applyTitles = (state: FleetState, titles: Record<string, string>): FleetState => {
  let changed = false;
  const cards: FleetState['cards'] = {};
  for (const card of Object.values(state.cards)) {
    const title = titles[card.workspaceId];
    if (title === undefined) {
      changed = true;
      continue;
    }
    if (title !== card.title) changed = true;
    cards[card.workspaceId] = title === card.title ? card : { ...card, title };
  }
  return changed ? { cards } : state;
};

// ── Derived views ─────────────────────────────────────────────────────────

export type FleetPhase = 'waiting' | 'working' | 'stopped';

export const pendingAsks = (card: FleetCard): FleetAsk[] =>
  Object.values(card.asks)
    .filter((a) => a.resolvedAt == null)
    .sort((a, b) => a.at - b.at);

export interface FleetAttention {
  asks: number;
  approvals: number;
  grants: number;
  total: number;
  /** When the oldest open item appeared; orders waiting cards. */
  since: number | null;
}

export const cardAttention = (card: FleetCard): FleetAttention => {
  const asks = pendingAsks(card);
  const requests = Object.values(card.requests).filter((r) => r.resolvedAt == null);
  const times = [...asks, ...requests].map((item) => item.at);
  return {
    asks: asks.length,
    approvals: requests.filter((r) => r.kind === 'approval').length,
    grants: requests.filter((r) => r.kind === 'grant').length,
    total: times.length,
    since: times.length ? Math.min(...times) : null,
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
  kind: 'task' | 'ask' | 'approval' | 'grant';
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

const taskItems = (tasks: WorkspaceTaskResponse[]): FleetItem[] => {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const rootOf = (task: WorkspaceTaskResponse): WorkspaceTaskResponse => {
    let current = task;
    const seen = new Set<string>();
    while (current.parentTaskId && byId.has(current.parentTaskId) && !seen.has(current.id)) {
      seen.add(current.id);
      current = byId.get(current.parentTaskId)!;
    }
    return current;
  };
  const groups = new Map<string, WorkspaceTaskResponse[]>();
  for (const task of tasks) {
    const root = rootOf(task);
    groups.set(root.id, [...(groups.get(root.id) ?? []), task]);
  }
  return [...groups.entries()].map(([rootId, group]) => {
    const root = byId.get(rootId)!;
    const live = group.some((t) => isTaskLive(t.status));
    const agents: FleetItem['agents'] = [];
    for (const t of [root, ...group]) {
      const agent = agentOf(t);
      if (!agents.some((a) => a.seed === agent.seed)) agents.push(agent);
    }
    let status: string;
    let statusKey: string;
    if (live && !isTaskLive(root.status)) {
      status = 'Subtasks running';
      statusKey = 'running';
    } else if (root.status === 'failed' && root.failureReason === 'blocked') {
      status = 'Blocked';
      statusKey = 'blocked';
    } else {
      statusKey = root.status;
      status = TASK_LABEL[root.status];
    }
    return {
      key: `task:${rootId}`,
      kind: 'task',
      text: root.title,
      status,
      statusKey,
      tone: live ? 'live' : root.status === 'failed' ? 'failed' : 'done',
      agents,
      at: Math.max(...group.map((t) => Number(t.updatedAt))),
    } satisfies FleetItem;
  });
};

const TASK_LABEL: Record<TaskStatus, string> = {
  queued: 'Queued',
  running: 'Running',
  stopping: 'Stopping',
  stopped: 'Stopped',
  completed: 'Done',
  failed: 'Failed',
};

/** Live items first (attention, then work in progress), then up to `HISTORY_LIMIT` resolved items, newest first. */
export const cardItems = (card: FleetCard, live: FleetLive): FleetItem[] => {
  const items: FleetItem[] = taskItems(workspaceTasks(card, live));
  for (const ask of Object.values(card.asks)) {
    const open = ask.resolvedAt == null;
    items.push({
      key: `ask:${ask.pendingId}`,
      kind: 'ask',
      text: ask.question,
      status: open ? 'Question' : 'Answered',
      tone: open ? 'attention' : 'done',
      agents: [],
      at: ask.resolvedAt ?? ask.at,
    });
  }
  for (const request of Object.values(card.requests)) {
    const open = request.resolvedAt == null;
    items.push({
      key: `${request.kind}:${request.id}`,
      kind: request.kind,
      text: request.text,
      status: open ? (request.kind === 'approval' ? 'Approve command' : 'Grant access') : 'Resolved',
      tone: open ? 'attention' : 'done',
      agents: request.agentName ? [{ name: request.agentName, seed: request.agentName }] : [],
      at: request.resolvedAt ?? request.at,
    });
  }
  items.sort((a, b) => TONE_RANK[a.tone] - TONE_RANK[b.tone] || b.at - a.at);
  const liveCount = items.filter((i) => TONE_RANK[i.tone] < 2).length;
  return items.slice(0, liveCount + HISTORY_LIMIT);
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

export interface PlacedCard {
  id: string;
  cols: number;
  rows: number;
}

const MAX_GROWN_WAITING = 2;

/**
 * Pick, order and size the visible cards. Eligible: the selected card,
 * anything live, and workspaces active within `RECENT_WINDOW_MS`. When they
 * don't all fit, the selected card wins, then waiting ones (oldest first),
 * then the most recent. On screen: selected, waiting, then cards already
 * visible keep their place (`prevOrder`), then newcomers by recency, so
 * activity alone never reshuffles the grid.
 *
 * Sizes are checked against the grid's `row dense` auto-placement, which
 * puts each card at the first spot its shape fits: a card grows only where
 * it fits and leaves a cell for every card after it, so nothing spills
 * below the grid.
 */
export const placeCards = (
  cards: readonly FleetCard[],
  options: { now: number; capacity: GridCapacity; selectedId: string | null; prevOrder: readonly string[]; live: FleetLive }
): PlacedCard[] => {
  const { now, capacity, selectedId, prevOrder, live } = options;
  const { cols, rows } = capacity;
  const meta = cards
    .map((card) => ({ id: card.workspaceId, card, phase: cardPhase(card, live), since: cardAttention(card).since ?? 0 }))
    .filter(
      (m) => m.id === selectedId || m.phase !== 'stopped' || m.card.lastActivityAt >= now - RECENT_WINDOW_MS
    );
  const selected = meta.filter((m) => m.id === selectedId);
  const waiting = meta.filter((m) => m.id !== selectedId && m.phase === 'waiting').sort((a, b) => a.since - b.since);
  const recent = meta
    .filter((m) => m.id !== selectedId && m.phase !== 'waiting')
    .sort((a, b) => b.card.lastActivityAt - a.card.lastActivityAt);

  const selectedSize = { cols: Math.min(2, cols), rows: Math.min(2, rows) };
  const room = cols * rows - (selected.length ? selectedSize.cols * selectedSize.rows : 0);
  const chosen = new Set([...waiting, ...recent].slice(0, Math.max(0, room)).map((m) => m.id));
  const rest = recent.filter((m) => chosen.has(m.id));
  const order = [
    ...selected,
    ...waiting.filter((m) => chosen.has(m.id)),
    ...prevOrder.flatMap((id) => rest.filter((m) => m.id === id)),
    ...rest.filter((m) => !prevOrder.includes(m.id)),
  ];

  const taken = new Array<boolean>(cols * rows).fill(false);
  const cellsFree = (at: number, w: number, h: number) => {
    if ((at % cols) + w > cols || Math.floor(at / cols) + h > rows) return false;
    for (let r = 0; r < h; r += 1) for (let c = 0; c < w; c += 1) if (taken[at + r * cols + c]) return false;
    return true;
  };
  const place = (w: number, h: number): boolean => {
    const at = taken.findIndex((_, i) => cellsFree(i, w, h));
    if (at < 0) return false;
    for (let r = 0; r < h; r += 1) for (let c = 0; c < w; c += 1) taken[at + r * cols + c] = true;
    return true;
  };

  let grown = 0;
  const placed: PlacedCard[] = [];
  order.forEach((m, index) => {
    const isSelected = m.id === selectedId;
    const free = taken.filter((t) => !t).length;
    const later = order.length - index - 1;
    const wants = isSelected
      ? selectedSize
      : m.phase === 'waiting' && grown < MAX_GROWN_WAITING && cols >= 2 && free - 2 >= later
        ? { cols: 2, rows: 1 }
        : { cols: 1, rows: 1 };
    if (wants.cols * wants.rows > 1 && place(wants.cols, wants.rows)) {
      if (!isSelected) grown += 1;
      placed.push({ id: m.id, ...wants });
    } else if (place(1, 1)) {
      placed.push({ id: m.id, cols: 1, rows: 1 });
    }
  });
  return placed;
};

/** One of eight hues per workspace, stable across runs. */
export const workspaceHue = (workspaceId: string): number => {
  let hash = 0;
  for (let i = 0; i < workspaceId.length; i += 1) hash = (hash * 31 + workspaceId.charCodeAt(i)) | 0;
  const HUES = [200, 160, 28, 330, 260, 90, 300, 12];
  return HUES[Math.abs(hash) % HUES.length]!;
};
