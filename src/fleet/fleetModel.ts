/**
 * The Fleet's state: one card per workspace, built by a pure reducer over
 * the app-global event streams (assistant, approvals, path grants) and a
 * startup seed. Streaming variants (deltas, tool calls, message updates)
 * return the state untouched, so they never reach React.
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
  FailureReason,
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

export interface FleetTask {
  id: string;
  parentTaskId: string | null;
  title: string;
  status: TaskStatus;
  failureReason: FailureReason | null;
  runId: string | null;
  agentName: string;
  agentSeed: string;
  updatedAt: number;
}

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
  mainSessionId: string | null;
  lastActivityAt: number;
  /** In-flight runs of the main session. */
  mainRuns: string[];
  tasks: Record<string, FleetTask>;
  /** Sessions owned by tasks, so their runs and messages are never taken for the main's. */
  taskSessions: string[];
  asks: Record<string, FleetAsk>;
  requests: Record<string, FleetRequest>;
  pendingApprovals: number;
  pendingGrants: number;
  lastMessage: FleetNote | null;
  userMessage: FleetNote | null;
}

export interface FleetState {
  cards: Record<string, FleetCard>;
}

export const EMPTY_FLEET: FleetState = { cards: {} };

export const emptyCard = (workspaceId: string, title = ''): FleetCard => ({
  workspaceId,
  title,
  mainSessionId: null,
  lastActivityAt: 0,
  mainRuns: [],
  tasks: {},
  taskSessions: [],
  asks: {},
  requests: {},
  pendingApprovals: 0,
  pendingGrants: 0,
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

export const toFleetTask = (task: WorkspaceTaskResponse): FleetTask => ({
  id: task.id,
  parentTaskId: task.parentTaskId,
  title: task.title,
  status: task.status,
  failureReason: task.failureReason,
  runId: task.runId,
  agentName: task.assignedAgentDisplayName,
  agentSeed: task.assignedAgentDefinitionId || task.assignedToWorkspaceAgentId,
  updatedAt: Number(task.updatedAt),
});

export const isTaskLive = (status: TaskStatus): boolean =>
  status === 'queued' || status === 'running' || status === 'stopping';

const mergeTask = (card: FleetCard, task: WorkspaceTaskResponse): FleetCard => {
  const next = toFleetTask(task);
  const current = card.tasks[task.id];
  if (current && next.updatedAt < current.updatedAt) return card;
  const taskSessions =
    task.sessionId && !card.taskSessions.includes(task.sessionId)
      ? [...card.taskSessions, task.sessionId]
      : card.taskSessions;
  return { ...card, tasks: { ...card.tasks, [task.id]: next }, taskSessions };
};

/** The main session, adopting `sessionId` when none is known yet and `adopt` allows it. */
const isMainSession = (card: FleetCard, sessionId: string, adopt: boolean): boolean => {
  if (card.mainSessionId) return card.mainSessionId === sessionId;
  return adopt && !card.taskSessions.includes(sessionId);
};

const withMain = (card: FleetCard, sessionId: string): FleetCard =>
  card.mainSessionId === sessionId ? card : { ...card, mainSessionId: sessionId };

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

  switch (event.type) {
    case 'message_created': {
      const { message } = event.payload;
      const fromUser = message.role === 'user' && message.origin.kind === 'user';
      return updateCard(state, workspaceId, (card) => {
        if (!isMainSession(card, sessionId, fromUser)) return bump(card, at);
        const main = withMain(bump(card, at), sessionId);
        if (fromUser) {
          return { ...main, userMessage: { id: message.id, text: messageText(message), at } };
        }
        return dropUserMessage(main);
      });
    }

    case 'message_deleted':
      return updateCard(state, workspaceId, (card) =>
        card.userMessage?.id === event.payload.message_id ? { ...card, userMessage: null } : card
      );

    case 'run_queued':
    case 'run_started': {
      const { run } = event.payload;
      return updateCard(state, workspaceId, (card) => {
        const bumped = bump(card, at);
        const adopt = run.trigger !== 'workspace_task';
        // A user message only ever goes to the main, so a user run on another
        // session means the main was replaced (`/clear`).
        const replaced =
          run.trigger === 'user_message' &&
          !!card.mainSessionId &&
          card.mainSessionId !== sessionId &&
          !card.taskSessions.includes(sessionId);
        if (!replaced && !(adopt && isMainSession(card, sessionId, true))) return bumped;
        const main = withMain(bumped, sessionId);
        return main.mainRuns.includes(run.id)
          ? main
          : { ...main, mainRuns: [...main.mainRuns, run.id] };
      });
    }

    case 'run_completed':
    case 'run_failed':
    case 'run_cancelled': {
      const { run } = event.payload;
      return updateCard(state, workspaceId, (card) => {
        const bumped = bump(card, at);
        return bumped.mainRuns.includes(run.id)
          ? { ...bumped, mainRuns: bumped.mainRuns.filter((id) => id !== run.id) }
          : bumped;
      });
    }

    case 'assistant_message_completed': {
      const { message } = event.payload;
      return updateCard(state, workspaceId, (card) => {
        if (!isMainSession(card, sessionId, false)) return bump(card, at);
        const text = messageText(message);
        const next = dropUserMessage(bump(card, at));
        return text ? { ...next, lastMessage: { id: message.id, text, at } } : next;
      });
    }

    case 'ask_user_requested': {
      const p = event.payload;
      return updateCard(state, workspaceId, (card) => ({
        ...dropUserMessage(bump(card, at)),
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
      }));
    }

    case 'ask_user_resolved':
      return updateCard(state, workspaceId, (card) => {
        const ask = card.asks[event.payload.pending_id];
        if (!ask || ask.resolvedAt != null) return bump(card, at);
        return {
          ...bump(card, at),
          asks: { ...card.asks, [ask.pendingId]: { ...ask, resolvedAt: at } },
        };
      });

    case 'task_updated':
      return updateCard(state, workspaceId, (card) => {
        const merged = mergeTask(card, event.payload.task);
        return merged === card ? card : dropUserMessage(bump(merged, at));
      });

    default:
      return state;
  }
};

const requestKey = (kind: FleetRequestKind, id: string) => `${kind}:${id}`;

export const applyRequest = (
  state: FleetState,
  kind: FleetRequestKind,
  request: PermissionRequest | PathGrantRequest,
  at: number
): FleetState => {
  if (!request.workspaceId) return state;
  const key = requestKey(kind, request.requestId);
  return updateCard(state, request.workspaceId, (card) => {
    if (card.requests[key]) return card;
    const text = requestText(request);
    return {
      ...dropUserMessage(bump(card, at)),
      requests: {
        ...card.requests,
        [key]: { id: request.requestId, kind, text, agentName: request.agentName, at, resolvedAt: null },
      },
    };
  });
};

const resolveRequests = (
  card: FleetCard,
  at: number,
  match: (request: FleetRequest) => boolean
): FleetCard => {
  let requests: FleetCard['requests'] | null = null;
  for (const [key, request] of Object.entries(card.requests)) {
    if (request.resolvedAt != null || !match(request)) continue;
    requests ??= { ...card.requests };
    requests[key] = { ...request, resolvedAt: at };
  }
  return requests ? { ...card, requests } : card;
};

export const applyRequestResolved = (
  state: FleetState,
  kind: FleetRequestKind,
  requestId: string,
  at: number
): FleetState => {
  const key = requestKey(kind, requestId);
  const owner = Object.values(state.cards).find((card) => card.requests[key]);
  if (!owner) return state;
  return updateCard(state, owner.workspaceId, (card) =>
    resolveRequests(card, at, (r) => r.kind === kind && r.id === requestId)
  );
};

/** The backend's absolute pending count. A decision emits only this, so 0 resolves every request of the kind. */
export const applyAttentionCount = (
  state: FleetState,
  kind: FleetRequestKind,
  workspaceId: string,
  count: number,
  at: number
): FleetState =>
  updateCard(state, workspaceId, (card) => {
    const field = kind === 'approval' ? 'pendingApprovals' : 'pendingGrants';
    let next: FleetCard = card[field] === count ? card : { ...card, [field]: count };
    if (count <= 0) next = resolveRequests(next, at, (r) => r.kind === kind);
    if (next === card) return card;
    return count > card[field] ? dropUserMessage(bump(next, at)) : next;
  });

/** The user sent `message` from the Fleet; the same path the echoed event takes. */
export const applyUserSent = (
  state: FleetState,
  workspaceId: string,
  sessionId: string,
  message: AssistantMessage,
  at: number
): FleetState =>
  updateCard(state, workspaceId, (card) => ({
    ...withMain(bump(card, at), sessionId),
    userMessage: { id: message.id, text: messageText(message), at },
  }));

export interface FleetSeedWorkspace {
  workspaceId: string;
  title: string;
  lastActivityAt: number;
  mainSessionId: string | null;
  activeRunIds: string[];
  tasks: WorkspaceTaskResponse[];
  lastMessage: FleetNote | null;
  asks: FleetAsk[];
  requests: FleetRequest[];
  pendingApprovals: number;
  pendingGrants: number;
}

/**
 * Apply a startup seed. Live state (runs, pending items, counts) is replaced
 * by the seed's; history the store already holds is kept. Events buffered
 * while the seed loaded are replayed afterwards by the store.
 */
export const seedFleet = (
  state: FleetState,
  seed: readonly FleetSeedWorkspace[],
  now: number
): FleetState => {
  let next = state;
  for (const ws of seed) {
    next = updateCard(next, ws.workspaceId, (card) => {
      let merged: FleetCard = {
        ...card,
        title: ws.title || card.title,
        mainSessionId: ws.mainSessionId ?? card.mainSessionId,
        lastActivityAt: Math.max(card.lastActivityAt, ws.lastActivityAt),
        mainRuns: ws.activeRunIds,
        pendingApprovals: ws.pendingApprovals,
        pendingGrants: ws.pendingGrants,
        lastMessage:
          ws.lastMessage && (!card.lastMessage || ws.lastMessage.at >= card.lastMessage.at)
            ? ws.lastMessage
            : card.lastMessage,
      };
      for (const task of ws.tasks) merged = mergeTask(merged, task);

      const seedAsks = new Set(ws.asks.map((a) => a.pendingId));
      const asks: FleetCard['asks'] = {};
      for (const ask of Object.values(merged.asks)) {
        asks[ask.pendingId] =
          ask.resolvedAt == null && !seedAsks.has(ask.pendingId) ? { ...ask, resolvedAt: now } : ask;
      }
      for (const ask of ws.asks) asks[ask.pendingId] = asks[ask.pendingId] ?? ask;

      const seedRequests = new Set(ws.requests.map((r) => requestKey(r.kind, r.id)));
      const requests: FleetCard['requests'] = {};
      for (const [key, request] of Object.entries(merged.requests)) {
        requests[key] =
          request.resolvedAt == null && !seedRequests.has(key) ? { ...request, resolvedAt: now } : request;
      }
      for (const request of ws.requests) {
        const key = requestKey(request.kind, request.id);
        requests[key] = requests[key] ?? request;
      }
      return { ...merged, asks, requests };
    });
  }
  return next;
};

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

const requestText = (request: PermissionRequest | PathGrantRequest): string =>
  'command' in request
    ? request.command
    : `${request.requestedAccess === 'read_write' ? 'Write' : 'Read'} ${request.requestedPath}`;

export const seedFromSnapshot = (ws: FleetWorkspace, now: number): FleetSeedWorkspace => {
  const request = (kind: FleetRequestKind, r: PermissionRequest | PathGrantRequest): FleetRequest => ({
    id: r.requestId,
    kind,
    text: requestText(r),
    agentName: r.agentName,
    at: now,
    resolvedAt: null,
  });
  const preview = ws.lastAssistantMessage;
  return {
    workspaceId: ws.workspaceId,
    title: ws.title,
    lastActivityAt: Number(ws.lastActivityAt),
    mainSessionId: ws.mainSessionId,
    activeRunIds: ws.activeRunId ? [ws.activeRunId] : [],
    tasks: ws.tasks,
    lastMessage: preview
      ? { id: preview.messageId, text: preview.truncated ? `${preview.text}…` : preview.text, at: Number(preview.createdAt) }
      : null,
    asks: ws.pendingAsks.map(askFromPending),
    requests: [
      ...ws.pendingPermissions.map((r) => request('approval', r)),
      ...ws.pendingPathGrants.map((r) => request('grant', r)),
    ],
    pendingApprovals: ws.pendingPermissions.length,
    pendingGrants: ws.pendingPathGrants.length,
  };
};

export const applyTitles = (state: FleetState, titles: Record<string, string>): FleetState => {
  let next = state;
  for (const [id, title] of Object.entries(titles)) {
    const card = next.cards[id];
    if (card && card.title !== title) next = updateCard(next, id, (c) => ({ ...c, title }));
  }
  return next;
};

// ── Derived views ─────────────────────────────────────────────────────────

export type FleetPhase = 'waiting' | 'working' | 'stopped';

export const pendingAsks = (card: FleetCard): FleetAsk[] =>
  Object.values(card.asks)
    .filter((a) => a.resolvedAt == null)
    .sort((a, b) => a.at - b.at);

const openRequests = (card: FleetCard, kind: FleetRequestKind): number =>
  Object.values(card.requests).filter((r) => r.kind === kind && r.resolvedAt == null).length;

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
  const approvals = Math.max(card.pendingApprovals, openRequests(card, 'approval'));
  const grants = Math.max(card.pendingGrants, openRequests(card, 'grant'));
  const times = [
    ...asks.map((a) => a.at),
    ...Object.values(card.requests)
      .filter((r) => r.resolvedAt == null)
      .map((r) => r.at),
  ];
  const total = asks.length + approvals + grants;
  return {
    asks: asks.length,
    approvals,
    grants,
    total,
    since: total === 0 ? null : times.length ? Math.min(...times) : card.lastActivityAt,
  };
};

export const hasLiveTask = (card: FleetCard): boolean =>
  Object.values(card.tasks).some((t) => isTaskLive(t.status));

export const cardPhase = (card: FleetCard): FleetPhase => {
  if (cardAttention(card).total > 0) return 'waiting';
  if (card.mainRuns.length > 0 || hasLiveTask(card)) return 'working';
  return 'stopped';
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

const taskItems = (card: FleetCard): FleetItem[] => {
  const tasks = Object.values(card.tasks);
  const rootOf = (task: FleetTask): FleetTask => {
    let current = task;
    const seen = new Set<string>();
    while (current.parentTaskId && card.tasks[current.parentTaskId] && !seen.has(current.id)) {
      seen.add(current.id);
      current = card.tasks[current.parentTaskId]!;
    }
    return current;
  };
  const groups = new Map<string, FleetTask[]>();
  for (const task of tasks) {
    const root = rootOf(task);
    const group = groups.get(root.id) ?? [];
    group.push(task);
    groups.set(root.id, group);
  }
  return [...groups.entries()].map(([rootId, group]) => {
    const root = card.tasks[rootId]!;
    const live = group.some((t) => isTaskLive(t.status));
    const agents: FleetItem['agents'] = [];
    for (const t of [root, ...group]) {
      if (!agents.some((a) => a.seed === t.agentSeed)) agents.push({ name: t.agentName, seed: t.agentSeed });
    }
    const failed = root.status === 'failed';
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
      tone: live ? 'live' : failed ? 'failed' : 'done',
      agents,
      at: Math.max(...group.map((t) => t.updatedAt)),
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
export const cardItems = (card: FleetCard): FleetItem[] => {
  const items: FleetItem[] = [...taskItems(card)];
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
    const approval = request.kind === 'approval';
    items.push({
      key: `${request.kind}:${request.id}`,
      kind: request.kind,
      text: request.text,
      status: open ? (approval ? 'Approve command' : 'Grant access') : 'Resolved',
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
export const visibleLastMessage = (card: FleetCard): FleetNote | null =>
  cardPhase(card) === 'stopped' ? card.lastMessage : null;

/**
 * The single seam for what a card says beyond its events. Today it is the
 * rule above (last message once stopped); a generated summary would replace
 * this function's source without touching the reducer, attention or layout.
 */
export const cardContent = (card: FleetCard): { body: string | null } => ({
  body: visibleLastMessage(card)?.text ?? null,
});

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
 * Pick and order the visible cards. Eligible: the selected card, anything
 * live, and workspaces active within `RECENT_WINDOW_MS`. Priority when they
 * don't all fit: selected, waiting (oldest first), then most recent.
 * Order on screen: waiting cards first, then cards already visible keep
 * their place (`prevOrder`), then newcomers by recency — so activity alone
 * never reshuffles the grid.
 */
export const placeCards = (
  cards: readonly FleetCard[],
  options: { now: number; capacity: GridCapacity; selectedId: string | null; prevOrder: readonly string[] }
): PlacedCard[] => {
  const { now, capacity, selectedId, prevOrder } = options;
  const meta = cards
    .map((card) => ({ card, phase: cardPhase(card), attention: cardAttention(card) }))
    .filter(
      ({ card, phase }) =>
        card.workspaceId === selectedId ||
        phase !== 'stopped' ||
        card.lastActivityAt >= now - RECENT_WINDOW_MS
    );
  const waitingSince = (m: (typeof meta)[number]) => m.attention.since ?? 0;
  const priority = [...meta].sort((a, b) => {
    const rank = (m: (typeof meta)[number]) =>
      m.card.workspaceId === selectedId ? 0 : m.phase === 'waiting' ? 1 : 2;
    return (
      rank(a) - rank(b) ||
      (rank(a) === 1 ? waitingSince(a) - waitingSince(b) : 0) ||
      b.card.lastActivityAt - a.card.lastActivityAt
    );
  });

  let free = capacity.cols * capacity.rows;
  let grown = 0;
  const chosen = new Map<string, PlacedCard & { waiting: boolean; since: number; recent: number }>();
  for (const m of priority) {
    const id = m.card.workspaceId;
    const waiting = m.phase === 'waiting';
    let cols = 1;
    let rows = 1;
    if (id === selectedId) {
      cols = Math.min(2, capacity.cols);
      rows = Math.min(2, capacity.rows);
    } else if (waiting && grown < MAX_GROWN_WAITING && capacity.cols >= 2) {
      cols = 2;
    }
    if (cols * rows > free) {
      cols = 1;
      rows = 1;
    }
    if (free < 1) break;
    if (cols > 1 && id !== selectedId) grown += 1;
    free -= cols * rows;
    chosen.set(id, { id, cols, rows, waiting, since: waitingSince(m), recent: m.card.lastActivityAt });
  }

  const all = [...chosen.values()];
  const waiting = all.filter((c) => c.waiting).sort((a, b) => a.since - b.since);
  const rest = all.filter((c) => !c.waiting);
  const kept = prevOrder.flatMap((id) => rest.filter((c) => c.id === id));
  const newcomers = rest.filter((c) => !prevOrder.includes(c.id)).sort((a, b) => b.recent - a.recent);
  return [...waiting, ...kept, ...newcomers].map(({ id, cols, rows }) => ({ id, cols, rows }));
};

/** One of eight hues per workspace, stable across runs. */
export const workspaceHue = (workspaceId: string): number => {
  let hash = 0;
  for (let i = 0; i < workspaceId.length; i += 1) hash = (hash * 31 + workspaceId.charCodeAt(i)) | 0;
  const HUES = [200, 160, 28, 330, 260, 90, 300, 12];
  return HUES[Math.abs(hash) % HUES.length]!;
};
