/**
 * The Fleet's state: one card per workspace, built by a pure reducer over
 * the app-global event streams (assistant, approvals, path grants) and a
 * startup snapshot. Streaming variants (deltas, tool calls, message updates)
 * return the state untouched, so they never reach React. Runs and tasks are
 * not copied here: they come from `fleetActivityStore` and `taskStore`
 * (`FleetLive`).
 *
 * What a card shows follows the owner's rules: live activity is highlighted,
 * finished subtasks and answered questions stay as low-emphasis history
 * (answered approvals and grants leave), the last completed assistant message
 * always shows, and the user's own message shows until the next message or
 * event arrives.
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
import type { PendingAskUser } from '../assistant/sessionStore';
import { taskDisplayStatus } from '../utils/taskDisplay';
import { buildTaskTree, type TaskNode } from '../utils/taskTree';

/** A workspace with no event in this window and nothing live gets no card. */
export const RECENT_WINDOW_MS = 12 * 60 * 60 * 1000;
const NOTE_CHARS = 600;

export type FleetRequestKind = 'approval' | 'grant';

/** A command approval or a path grant, as it arrives. */
export type FleetPendingRequest =
  | { kind: 'approval'; request: PermissionRequest }
  | { kind: 'grant'; request: PathGrantRequest };

/** An `ask_user` question, with the session that asked it. */
export type FleetAsk = PendingAskUser & { sessionId: string };

/**
 * Something the workspace waits on the user for: a question, a command
 * approval or a path grant, with what answering it needs.
 */
export type FleetRequest = {
  text: string;
  agentName: string | null;
  at: number;
  resolvedAt: number | null;
} & (
  | { kind: 'ask'; ask: FleetAsk }
  | { kind: 'approval'; request: PermissionRequest }
  | { kind: 'grant'; request: PathGrantRequest }
);

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

/** An answered question stays as history; an answered approval or grant leaves the card. */
const resolve = (card: FleetCard, key: string, at: number): FleetCard => {
  const request = card.requests[key];
  if (!request || request.resolvedAt != null) return card;
  const { [key]: _, ...rest } = card.requests;
  return { ...card, requests: request.kind === 'ask' ? { ...card.requests, [key]: { ...request, resolvedAt: at } } : rest };
};

const askRequest = (ask: FleetAsk, at: number): FleetRequest => ({
  kind: 'ask',
  ask,
  text: ask.question,
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
            [requestKey('ask', event.payload.pending_id)]: askRequest(
              {
                pendingId: event.payload.pending_id,
                question: event.payload.question,
                options: event.payload.options,
                multiSelect: event.payload.multi_select,
                extraContext: event.payload.extra_context ?? null,
                sessionId,
              },
              at
            ),
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

const toRequest = (pending: FleetPendingRequest, at: number): FleetRequest => ({
  ...pending,
  text: requestText(pending.request),
  agentName: pending.request.agentName,
  at,
  resolvedAt: null,
});

export const applyRequest = (state: FleetState, pending: FleetPendingRequest, at: number): FleetState => {
  const { workspaceId, requestId } = pending.request;
  if (!workspaceId) return state;
  const key = requestKey(pending.kind, requestId);
  return updateCard(state, workspaceId, (card) =>
    card.requests[key]
      ? card
      : {
          ...dropUserMessage(bump(card, at)),
          requests: { ...card.requests, [key]: toRequest(pending, at) },
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

/** Requests still open in `card` but absent from `open` were resolved meanwhile; new open ones are added. */
const reconcile = (card: FleetCard, open: Record<string, FleetRequest>, now: number): Record<string, FleetRequest> => {
  let next = { ...card, requests: { ...open, ...card.requests } };
  for (const key of Object.keys(card.requests)) if (!open[key]) next = resolve(next, key, now);
  return next.requests;
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
    for (const a of ws.pendingAsks) open[requestKey('ask', a.pendingId)] = askRequest(a, Number(a.requestedAt));
    for (const r of ws.pendingPermissions) open[requestKey('approval', r.requestId)] = toRequest({ kind: 'approval', request: r }, activity);
    for (const r of ws.pendingPathGrants) open[requestKey('grant', r.requestId)] = toRequest({ kind: 'grant', request: r }, activity);
    cards[ws.workspaceId] = {
      ...card,
      title: ws.title || card.title,
      mainSessionId: ws.mainSessionId ?? card.mainSessionId,
      lastActivityAt: Math.max(card.lastActivityAt, activity),
      lastMessage:
        lastMessage && (!card.lastMessage || lastMessage.at >= card.lastMessage.at) ? lastMessage : card.lastMessage,
      requests: reconcile(card, open, now),
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

/** The card's open requests, oldest first: the order they are answered in. */
export const openRequests = (card: FleetCard): FleetRequest[] =>
  Object.values(card.requests)
    .filter((r) => r.resolvedAt == null)
    .sort((a, b) => a.at - b.at);

export const cardAttention = (card: FleetCard): FleetAttention => {
  const open = openRequests(card);
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

export const agentOf = (task: WorkspaceTaskResponse): FleetAgentRef => ({
  name: task.assignedAgentDisplayName,
  seed: task.assignedAgentDefinitionId || task.assignedToWorkspaceAgentId,
});

const subtree = (node: TaskNode): WorkspaceTaskResponse[] => [node.task, ...node.children.flatMap(subtree)];

/** Every distinct agent with a task on the card, live ones first, then the most recent. */
export const cardAgents = (card: FleetCard, live: FleetLive): FleetAgentRef[] => {
  const tasks = workspaceTasks(card, live).sort(
    (a, b) => Number(isTaskLive(b.status)) - Number(isTaskLive(a.status)) || Number(b.updatedAt) - Number(a.updatedAt)
  );
  return [...new Map(tasks.map((t) => [agentOf(t).seed, agentOf(t)])).values()];
};

// ── The comic page: what a small card says ────────────────────────────────

const DAY_MS = 24 * 60 * 60 * 1000;
/** A finished task keeps the speech bubble this long, then moves to Earlier. */
export const FRESH_MS = 10 * 60 * 1000;
/** A card whose last word is older than this is stale: its drawing fades. */
export const STALE_MS = DAY_MS;
/** Speaking rows a card draws: running tasks first, then just-finished ones. */
export const TALK_LIMIT = 3;
export const EARLIER_LIMIT = 2;
const SENTENCE_CHARS = 200;

export interface FleetAgentRef {
  name: string;
  seed: string;
}

/** A top-level task in flight: Main hands it over, the agent thinks. */
export interface FleetRunningTalk {
  key: string;
  title: string;
  agent: FleetAgentRef;
  queued: boolean;
  /** Other agents working on its subtasks. */
  helpers: number;
}

/** A task that just ended: the agent answers, and Main's question shrinks. */
export interface FleetResultTalk {
  key: string;
  asked: string;
  result: string;
  failed: boolean;
  agent: FleetAgentRef;
  at: number;
}

export type FleetOutcome = 'done' | 'failed' | 'stopped' | 'answered';

export interface FleetEarlierItem {
  key: string;
  title: string;
  /** `null` for a question the user answered. */
  agent: FleetAgentRef | null;
  outcome: FleetOutcome;
  at: number;
}

export type FleetMood = 'working' | 'fresh' | 'idle' | 'stale';

export interface FleetSpeech extends FleetNote {
  /** The message as plain text, for a bubble that clamps its lines. */
  plain: string;
  /** The first sentence, where the card has room for little more. */
  headline: string;
}

/**
 * What a small card says, in comic form: the caption says when, the bubbles
 * say what. Everything comes from events (tasks, messages, answers) and the
 * time of the last one, so polling that changes nothing changes nothing here.
 */
export interface FleetScene {
  mood: FleetMood;
  caption: string;
  running: FleetRunningTalk[];
  finished: FleetResultTalk[];
  /** Main's own run is in flight. */
  mainBusy: boolean;
  /** Main's last message, to the reader. */
  toYou: FleetSpeech | null;
  userMessage: FleetNote | null;
  /** Newest first. */
  earlier: FleetEarlierItem[];
}

/** One markdown line as plain text: no block markers, emphasis or link targets. */
const plainLine = (line: string): string =>
  line
    .replace(/^\s*(?:#{1,6}\s+|>\s?|[-*+]\s+|\d+\.\s+)/, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\*\*|__|`|\*/g, '')
    .trim();

/** A markdown text as one plain paragraph, for a bubble that clamps its lines. */
export const plainText = (markdown: string | null | undefined): string =>
  (markdown ?? '').split('\n').map(plainLine).filter(Boolean).join(' ');

/**
 * The first sentence of a markdown text, as plain text: the headline of a
 * message or a result. Cut at the first sentence end, or at the first line.
 */
export const firstSentence = (markdown: string | null | undefined): string => {
  const plain = (markdown ?? '').split('\n').map(plainLine).find(Boolean) ?? '';
  const end = plain.search(/[.!?…](?=\s|$)/);
  const sentence = end >= 0 ? plain.slice(0, end + 1) : plain;
  return sentence.length > SENTENCE_CHARS ? `${sentence.slice(0, SENTENCE_CHARS - 1)}…` : sentence;
};

/** `14:05` on `now`'s day, `Oct 16` before. */
export const shortTime = (at: number, now: number): string => {
  const date = new Date(at);
  return date.toDateString() === new Date(now).toDateString()
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString([], { month: 'short', day: 'numeric' });
};

/** A run of the workspace that belongs to no task is its Main's. */
const mainRunning = (card: FleetCard, live: FleetLive): boolean => {
  const taskRuns = new Set(workspaceTasks(card, live).map((t) => t.runId));
  return (live.runs[card.workspaceId] ?? []).some((id) => !taskRuns.has(id));
};

const resultOf = (task: WorkspaceTaskResponse): string =>
  task.status === 'failed'
    ? firstSentence(task.error) || taskDisplayStatus(task).label
    : firstSentence(task.resultSummary) || task.title;

export const cardScene = (card: FleetCard, live: FleetLive, now: number): FleetScene => {
  const running: FleetRunningTalk[] = [];
  const finished: FleetResultTalk[] = [];
  const earlier: FleetEarlierItem[] = [];
  for (const node of buildTaskTree(workspaceTasks(card, live))) {
    const root = node.task;
    const group = subtree(node);
    const agent = agentOf(root);
    if (group.some((t) => isTaskLive(t.status))) {
      const helpers = new Set(group.map((t) => agentOf(t).seed).filter((seed) => seed !== agent.seed)).size;
      running.push({ key: `task:${root.id}`, title: root.title, agent, queued: root.status === 'queued', helpers });
      continue;
    }
    const at = Math.max(...group.map((t) => Number(t.completedAt ?? t.updatedAt)));
    const failed = root.status === 'failed';
    if (root.status !== 'stopped' && at >= now - FRESH_MS) {
      finished.push({ key: `task:${root.id}`, asked: root.title, result: resultOf(root), failed, agent, at });
    } else {
      earlier.push({ key: `task:${root.id}`, title: root.title, agent, outcome: failed ? 'failed' : root.status === 'stopped' ? 'stopped' : 'done', at });
    }
  }
  for (const [key, request] of Object.entries(card.requests)) {
    if (request.resolvedAt != null) earlier.push({ key, title: request.text, agent: null, outcome: 'answered', at: request.resolvedAt });
  }

  finished.sort((a, b) => b.at - a.at);
  const shownRunning = running.slice(0, TALK_LIMIT);
  const shownFinished = finished.slice(0, TALK_LIMIT - shownRunning.length);
  // Results with no room to speak are already history.
  for (const f of finished.slice(shownFinished.length)) {
    earlier.push({ key: f.key, title: f.asked, agent: f.agent, outcome: f.failed ? 'failed' : 'done', at: f.at });
  }
  earlier.sort((a, b) => b.at - a.at);

  const mainBusy = mainRunning(card, live);
  const lastWord = card.lastMessage?.at ?? card.lastActivityAt;
  const mood: FleetMood =
    running.length > 0 || mainBusy
      ? 'working'
      : shownFinished.length > 0
        ? 'fresh'
        : lastWord > 0 && now - lastWord >= STALE_MS
          ? 'stale'
          : 'idle';
  const days = Math.floor((now - lastWord) / DAY_MS);
  const caption = {
    working: running.length > 1 ? `Now · ${running.length} at once` : 'Now',
    fresh: `Just now · ${shortTime(shownFinished[0]?.at ?? now, now)}`,
    idle: lastWord > 0 ? `Last word · ${shortTime(lastWord, now)}` : 'Nothing yet',
    stale: days === 1 ? 'Yesterday' : `${days} days ago`,
  }[mood];

  return {
    mood,
    caption,
    running: shownRunning,
    finished: shownFinished,
    mainBusy,
    toYou: card.lastMessage
      ? { ...card.lastMessage, plain: plainText(card.lastMessage.text), headline: firstSentence(card.lastMessage.text) }
      : null,
    userMessage: card.userMessage,
    earlier: earlier.slice(0, EARLIER_LIMIT),
  };
};

/** An agent with work in flight: its tasks, or `main` for the workspace's own session. */
export interface LiveAgent {
  seed: string;
  name: string;
  main?: true;
  running: boolean;
  /** What it is doing, for a tooltip. */
  doing: string;
}

const quoted = (tasks: WorkspaceTaskResponse[], verb: string): string =>
  tasks.length === 1 ? `${verb} '${tasks[0]!.title}'` : `${verb}: ${tasks.length} tasks`;

/**
 * One entry per distinct agent with a live task, running agents first. A run
 * that belongs to no task is the workspace's own session, run by its Main.
 */
export const liveAgents = (card: FleetCard, live: FleetLive): LiveAgent[] => {
  const tasks = workspaceTasks(card, live);
  const byAgent = new Map<string, { name: string; running: WorkspaceTaskResponse[]; queued: WorkspaceTaskResponse[] }>();
  for (const task of tasks) {
    if (!isTaskLive(task.status)) continue;
    const { name, seed } = agentOf(task);
    const entry = byAgent.get(seed) ?? { name, running: [], queued: [] };
    (task.status === 'queued' ? entry.queued : entry.running).push(task);
    byAgent.set(seed, entry);
  }
  const agents: LiveAgent[] = [...byAgent].map(([seed, { name, running, queued }]) => ({
    seed,
    name,
    running: running.length > 0,
    doing: [running.length ? quoted(running, 'running') : '', queued.length ? quoted(queued, 'queued') : '']
      .filter(Boolean)
      .join(', '),
  }));
  if (mainRunning(card, live)) agents.unshift({ seed: 'main', name: 'Main', main: true, running: true, doing: 'running the workspace chat' });
  return agents.sort((a, b) => Number(b.running) - Number(a.running));
};

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

/** The stage: the top-left 2 × 2 slot (smaller on a smaller grid). */
export const stageSpan = (capacity: GridCapacity): GridCapacity => ({
  cols: Math.min(2, capacity.cols),
  rows: Math.min(2, capacity.rows),
});

/**
 * The tracks `count` cards fill, within `capacity`: the stage plus the columns
 * beside it, then full rows below. Sizing the grid by its capacity instead
 * leaves the unused rows and columns empty, so a short Fleet stops short of
 * the terminal.
 */
export const gridShape = (capacity: GridCapacity, count: number): GridCapacity => {
  const stage = stageSpan(capacity);
  const side = capacity.cols - stage.cols;
  const small = Math.max(0, count - 1);
  if (small <= side * stage.rows) {
    return { cols: stage.cols + Math.ceil(small / stage.rows), rows: stage.rows };
  }
  const below = Math.ceil((small - side * stage.rows) / capacity.cols);
  return { cols: capacity.cols, rows: Math.min(capacity.rows, stage.rows + below) };
};

/**
 * The visible cards, in slot order; slot 0 is the stage. Eligible: the
 * selected card, anything live, and workspaces active within
 * `RECENT_WINDOW_MS`. When they don't all fit, the selected card wins, then
 * waiting ones (oldest first), then the most recent. Cards already visible
 * keep their slots (`prevOrder`) and newcomers take the next ones. The stage
 * starts with the most recent card; selecting a card swaps it with the stage
 * card, and nothing else moves.
 */
export const pickCards = (
  cards: readonly FleetCard[],
  options: { now: number; capacity: GridCapacity; selectedId: string | null; prevOrder: readonly string[]; live: FleetLive }
): string[] => {
  const { now, capacity, selectedId, prevOrder, live } = options;
  const stage = stageSpan(capacity);
  const meta = cards
    .map((card) => ({ id: card.workspaceId, card, phase: cardPhase(card, live), since: cardAttention(card).since ?? 0 }))
    .filter(
      (m) => m.id === selectedId || m.phase !== 'stopped' || m.card.lastActivityAt >= now - RECENT_WINDOW_MS
    );
  const rank = (m: (typeof meta)[number]): [number, number] =>
    m.id === selectedId ? [0, 0] : m.phase === 'waiting' ? [1, m.since] : [2, -m.card.lastActivityAt];
  const chosen = meta
    .sort((a, b) => {
      const [ra, ka] = rank(a);
      const [rb, kb] = rank(b);
      return ra - rb || ka - kb;
    })
    .slice(0, capacity.cols * capacity.rows - stage.cols * stage.rows + 1);
  if (chosen.length === 0) return [];
  const ids = new Set(chosen.map((m) => m.id));
  let order = prevOrder.filter((id) => ids.has(id));
  if (order[0] !== prevOrder[0] || order.length === 0) {
    const mostRecent = chosen.reduce((a, b) => (b.card.lastActivityAt > a.card.lastActivityAt ? b : a)).id;
    order = [mostRecent, ...order.filter((id) => id !== mostRecent)];
  }
  order.push(...chosen.filter((m) => !order.includes(m.id)).map((m) => m.id));
  const at = selectedId ? order.indexOf(selectedId) : -1;
  if (at > 0) [order[0], order[at]] = [order[at]!, order[0]!];
  return order;
};

/** The card `step` slots after `selectedId` (±1), wrapping; from no selection, the first or the last. */
export const cycleCard = (order: readonly string[], selectedId: string | null, step: 1 | -1): string | null => {
  if (order.length === 0) return null;
  const at = selectedId ? order.indexOf(selectedId) : -1;
  if (at < 0) return step > 0 ? order[0]! : order[order.length - 1]!;
  return order[(at + step + order.length) % order.length]!;
};
