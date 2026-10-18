/**
 * The Fleet's state: one card per workspace, built by a pure reducer over
 * the app-global event streams (assistant, approvals, path grants) and a
 * startup snapshot. Streaming variants (deltas, tool calls, message updates)
 * return the state untouched, so they never reach React. Runs and tasks are
 * not copied here: they come from `fleetActivityStore` and `taskStore`
 * (`FleetLive`).
 *
 * A card keeps the messages said in the workspace (`said`): what the user,
 * Main and the agents told each other. `cardFeed` turns them, with the
 * tasks, into the miniature's highlights, in the order they happened.
 */
import type {
  AgentRefSnapshot,
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantUiEvent,
  FleetWorkspace,
  MessageOrigin,
  PathGrantRequest,
  PermissionRequest,
  TaskStatus,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import { isRunMarkerText } from '../assistant/runMarker';
import type { PendingAskUser } from '../assistant/sessionStore';
import { taskDisplayStatus } from '../utils/taskDisplay';

/** A workspace with no event in this window and nothing live gets no card. */
export const RECENT_WINDOW_MS = 12 * 60 * 60 * 1000;
const NOTE_CHARS = 600;
/** Messages a card keeps; a miniature shows fewer. */
export const SAID_LIMIT = 24;

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
} & (
  | { kind: 'ask'; ask: FleetAsk }
  | { kind: 'approval'; request: PermissionRequest }
  | { kind: 'grant'; request: PathGrantRequest }
);

/** A message said in the workspace, as the feed needs it. */
export interface FleetSaid {
  id: string;
  sessionId: string;
  role: 'user' | 'assistant';
  /** Who sent a user message; `null` for an assistant message. */
  origin: MessageOrigin | null;
  /** Clipped to `NOTE_CHARS`, ending in `…` when cut. */
  text: string;
  /** Cut short: the full message has to be fetched. */
  truncated: boolean;
  at: number;
}

export interface FleetCard {
  workspaceId: string;
  title: string;
  /** From the snapshot, or set when the user sends or clears from the terminal. */
  mainSessionId: string | null;
  lastActivityAt: number;
  /** When the user last opened the workspace (here or on its page); 0 = never. */
  lastOpenedAt: number;
  /** Open requests, by `kind:id`. */
  requests: Record<string, FleetRequest>;
  /** Oldest first, at most `SAID_LIMIT`. */
  said: FleetSaid[];
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
  lastOpenedAt: 0,
  requests: {},
  said: [],
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

const clip = (text: string): { text: string; truncated: boolean } => {
  const trimmed = text.trim();
  return trimmed.length > NOTE_CHARS
    ? { text: `${trimmed.slice(0, NOTE_CHARS - 1)}…`, truncated: true }
    : { text: trimmed, truncated: false };
};

/** A message's text parts, in full. */
export const fullMessageText = (message: AssistantMessage): string =>
  message.content
    .map((part) => (part.type === 'text' ? part.text : ''))
    .filter(Boolean)
    .join('\n')
    .trim();

export const isTaskLive = (status: TaskStatus): boolean =>
  status === 'queued' || status === 'running' || status === 'stopping';

const saidOf = (message: AssistantMessage, at: number): FleetSaid => ({
  id: message.id,
  sessionId: message.sessionId,
  role: message.role === 'assistant' ? 'assistant' : 'user',
  origin: message.role === 'assistant' ? null : message.origin,
  ...clip(fullMessageText(message)),
  at,
});

/**
 * Keeps `said` in time order and at most `SAID_LIMIT` long; an event replaces
 * what a seed said. An automation's run marker is never said.
 */
const addSaid = (card: FleetCard, said: readonly FleetSaid[], replace: boolean): FleetCard => {
  const held = new Set(card.said.map((s) => s.id));
  const incoming = said.filter(
    (s) => s.text && !(s.role === 'user' && isRunMarkerText(s.text)) && (replace || !held.has(s.id))
  );
  if (incoming.length === 0) return card;
  const ids = new Set(incoming.map((s) => s.id));
  const next = [...card.said.filter((s) => !ids.has(s.id)), ...incoming].sort((a, b) => a.at - b.at);
  return { ...card, said: next.slice(-SAID_LIMIT) };
};

const requestKey = (kind: FleetRequest['kind'], id: string) => `${kind}:${id}`;

/** An answered request leaves the card. */
const resolve = (card: FleetCard, key: string): FleetCard => {
  if (!card.requests[key]) return card;
  const { [key]: _, ...rest } = card.requests;
  return { ...card, requests: rest };
};

const askRequest = (ask: FleetAsk, at: number): FleetRequest => ({
  kind: 'ask',
  ask,
  text: ask.question,
  agentName: null,
  at,
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
    switch (event.type) {
      case 'message_created': {
        // Every session's user messages: the user's, an agent's and a task's
        // report. An assistant message is said when it completes.
        const { message } = event.payload;
        return message.role === 'user' ? addSaid(card, [saidOf(message, at)], true) : card;
      }
      case 'message_deleted': {
        const said = card.said.filter((s) => s.id !== event.payload.message_id);
        return said.length === card.said.length ? card : { ...card, said };
      }
      case 'assistant_message_completed':
        // Main's replies, to the user. A task's own replies reach Main as its report;
        // before the main session is known, keep them and let `cardFeed` choose.
        if (card.mainSessionId && card.mainSessionId !== sessionId) return card;
        return addSaid(card, [saidOf(event.payload.message, at)], true);
      case 'ask_user_requested':
        return {
          ...card,
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
        return resolve(card, requestKey('ask', event.payload.pending_id));
      default:
        // Run and task lifecycle: activity; the tasks themselves come from the task store.
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
});

export const applyRequest = (state: FleetState, pending: FleetPendingRequest, at: number): FleetState => {
  const { workspaceId, requestId } = pending.request;
  if (!workspaceId) return state;
  const key = requestKey(pending.kind, requestId);
  return updateCard(state, workspaceId, (card) =>
    card.requests[key] ? card : { ...bump(card, at), requests: { ...card.requests, [key]: toRequest(pending, at) } }
  );
};

export const applyRequestResolved = (state: FleetState, kind: FleetRequestKind, requestId: string): FleetState => {
  const key = requestKey(kind, requestId);
  const owner = Object.values(state.cards).find((card) => card.requests[key]);
  if (!owner) return state;
  return updateCard(state, owner.workspaceId, (card) => resolve(card, key));
};

/** The user sent `message` (or cleared the chat, `message` null) on `sessionId`: it is the main. */
export const applyUserSent = (
  state: FleetState,
  workspaceId: string,
  sessionId: string,
  message: AssistantMessage | null,
  at: number
): FleetState =>
  updateCard(state, workspaceId, (card) => {
    const next = { ...bump(card, at), mainSessionId: sessionId };
    return message ? addSaid(next, [saidOf(message, at)], true) : next;
  });

/** The user opened the workspace at `at`: the switcher's order knows it before the next snapshot. */
export const applyOpened = (state: FleetState, workspaceId: string, at: number): FleetState =>
  state.cards[workspaceId] ? updateCard(state, workspaceId, (card) => ({ ...card, lastOpenedAt: Math.max(card.lastOpenedAt, at) })) : state;

/**
 * Apply a snapshot. The Fleet becomes exactly the snapshot's workspaces, so a
 * deleted one (or one no longer among the most recent) loses its card;
 * history the store already holds for the others is kept, and a message an
 * event already brought is not said twice. Open requests become the
 * snapshot's. Events buffered while it loaded are replayed afterwards by the
 * store.
 */
export const seedFleet = (state: FleetState, snapshot: readonly FleetWorkspace[]): FleetState => {
  const cards: FleetState['cards'] = {};
  for (const ws of snapshot) {
    const card = state.cards[ws.workspaceId] ?? emptyCard(ws.workspaceId);
    const activity = Number(ws.lastActivityAt);
    const mainSessionId = ws.mainSessionId ?? card.mainSessionId;
    const seed: FleetSaid[] = ws.recentMessages.map((m) => ({
      id: m.messageId,
      sessionId: m.sessionId,
      role: m.role === 'assistant' ? 'assistant' : 'user',
      origin: m.role === 'assistant' ? null : m.origin,
      text: m.truncated ? `${m.text}…` : m.text,
      truncated: m.truncated,
      at: Number(m.createdAt),
    }));
    const preview = ws.lastAssistantMessage;
    if (preview && mainSessionId && !seed.some((m) => m.id === preview.messageId)) {
      seed.push({
        id: preview.messageId,
        sessionId: mainSessionId,
        role: 'assistant',
        origin: null,
        text: preview.truncated ? `${preview.text}…` : preview.text,
        truncated: preview.truncated,
        at: Number(preview.createdAt),
      });
    }
    // Approvals and grants carry no timestamp; the workspace's activity orders them among waiting cards.
    const requests: Record<string, FleetRequest> = {};
    for (const a of ws.pendingAsks) requests[requestKey('ask', a.pendingId)] = askRequest(a, Number(a.requestedAt));
    for (const r of ws.pendingPermissions) requests[requestKey('approval', r.requestId)] = toRequest({ kind: 'approval', request: r }, activity);
    for (const r of ws.pendingPathGrants) requests[requestKey('grant', r.requestId)] = toRequest({ kind: 'grant', request: r }, activity);
    // A request the store already holds keeps its own time.
    for (const key of Object.keys(requests)) if (card.requests[key]) requests[key] = card.requests[key];
    cards[ws.workspaceId] = addSaid(
      {
        ...card,
        title: ws.title || card.title,
        mainSessionId,
        lastActivityAt: Math.max(card.lastActivityAt, activity),
        lastOpenedAt: Math.max(card.lastOpenedAt, Number(ws.lastOpenedAt)),
        requests,
      },
      seed,
      false
    );
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
  Object.values(card.requests).sort((a, b) => a.at - b.at);

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

/** An agent as a message names it, with the same face seed as its tasks. */
const agentRef = (agent: AgentRefSnapshot): FleetAgentRef => ({
  name: agent.displayName,
  seed: agent.agentDefinitionId || agent.workspaceAgentId,
});

/** Every distinct agent with a task on the card, live ones first, then the most recent. */
export const cardAgents = (card: FleetCard, live: FleetLive): FleetAgentRef[] => {
  const tasks = workspaceTasks(card, live).sort(
    (a, b) => Number(isTaskLive(b.status)) - Number(isTaskLive(a.status)) || Number(b.updatedAt) - Number(a.updatedAt)
  );
  return [...new Map(tasks.map((t) => [agentOf(t).seed, agentOf(t)])).values()];
};


// ── A miniature: the workspace's conversation, as highlights ─────────────

const DAY_MS = 24 * 60 * 60 * 1000;
/** A task that just ended keeps the panel's caption "Just finished" this long. */
export const FRESH_MS = 10 * 60 * 1000;
/** A card whose last word is older than this is stale: its drawing fades. */
export const STALE_MS = DAY_MS;
/** Older highlights collapsed to one-line rows above the bubbles; older ones hide. */
export const ROWS = 2;
/** Highlights a miniature has; rows that don't fit hide from the top. */
export const FEED_LIMIT = 16;
const SENTENCE_CHARS = 200;
/**
 * How a task's assignment message starts (`assignment_text` in
 * `workspace_tasks.rs`): its task's creation says it, by title.
 */
const ASSIGNMENT_PREFIX = 'You have been assigned a workspace-local task';

export interface FleetAgentRef {
  name: string;
  seed: string;
}

export type FleetSpeaker = { kind: 'you' } | { kind: 'main' } | { kind: 'agent'; agent: FleetAgentRef };

export type FleetOutcome = 'done' | 'failed' | 'stopped';

/** One thing said in the workspace: who to whom, what, and how a task ended. */
export interface FleetLine {
  key: string;
  at: number;
  from: FleetSpeaker;
  to: FleetSpeaker;
  /** As plain text, for a bubble that clamps its lines. */
  text: string;
  /** As said, in markdown, for the zoomed bubble. */
  markdown: string;
  /** The message said, to fetch in full when `truncated`; `null` for a task's own line. */
  message: { sessionId: string; id: string; truncated: boolean } | null;
  /** The first sentence, for a one-line row. */
  headline: string;
  /** A task's result. */
  outcome: FleetOutcome | null;
  /** A task handed over and not ended yet: its agent is (or will be) working. */
  live: 'running' | 'queued' | null;
}

export type FleetMood = 'working' | 'fresh' | 'idle' | 'stale';

/**
 * What a miniature says: the caption says when, the lines say what, oldest
 * first. Lines come from events and tasks only, so polling that changes
 * nothing changes nothing here; the mood and caption age with the clock.
 */
export interface FleetFeed {
  mood: FleetMood;
  caption: FleetCaption;
  lines: FleetLine[];
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

/**
 * A panel's caption box: when, not what. Which caption is an event's doing;
 * its words are relative and are worded against a clock (`captionText`), so
 * they can age by the minute while the panel itself waits for events.
 */
export type FleetCaption =
  | { kind: 'working'; count: number }
  | { kind: 'fresh' | 'idle' | 'stale' | 'waiting'; at: number }
  | { kind: 'empty' };

const MINUTE_MS = 60_000;

/** `just now`, `12 min ago`, `3 h ago`, `yesterday`, `3 days ago`. */
export const ago = (at: number, clock: number): string => {
  const minutes = Math.floor((clock - at) / MINUTE_MS);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? 'yesterday' : `${days} days ago`;
};

/** How long something has waited: `under a minute`, `12 min`, `3 h`, `2 days`. */
const waited = (at: number, clock: number): string => {
  const minutes = Math.floor((clock - at) / MINUTE_MS);
  if (minutes < 1) return 'under a minute';
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h`;
  const days = Math.floor(hours / 24);
  return days === 1 ? '1 day' : `${days} days`;
};

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

export const captionText = (caption: FleetCaption, clock: number): string => {
  switch (caption.kind) {
    case 'working':
      return caption.count > 1 ? `Now · ${caption.count} at once` : 'Now';
    case 'fresh':
      return clock - caption.at < MINUTE_MS ? 'Just now' : `Just finished · ${ago(caption.at, clock)}`;
    case 'idle':
      return `Last word · ${ago(caption.at, clock)}`;
    case 'stale':
      return capitalize(ago(caption.at, clock));
    case 'waiting':
      return `Waiting on you · ${waited(caption.at, clock)}`;
    case 'empty':
      return 'Nothing yet';
  }
};

/** `14:05` on `now`'s day, `Oct 16` before. */
export const shortTime = (at: number, now: number): string => {
  const date = new Date(at);
  return date.toDateString() === new Date(now).toDateString()
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString([], { month: 'short', day: 'numeric' });
};

/** A run of the workspace that belongs to no task is its Main's. */
export const mainRunning = (card: FleetCard, live: FleetLive): boolean => {
  const taskRuns = new Set(workspaceTasks(card, live).map((t) => t.runId));
  return (live.runs[card.workspaceId] ?? []).some((id) => !taskRuns.has(id));
};


const YOU: FleetSpeaker = { kind: 'you' };
const MAIN: FleetSpeaker = { kind: 'main' };
const agentSpeaker = (agent: FleetAgentRef): FleetSpeaker => ({ kind: 'agent', agent });
const OUTCOMES: Partial<Record<TaskStatus, FleetOutcome>> = { completed: 'done', failed: 'failed', stopped: 'stopped' };

/** How a task ended, in a few words. */
const resultOf = (task: WorkspaceTaskResponse): string =>
  (task.status === 'failed' ? task.error : task.resultSummary)?.trim() ||
  (task.status === 'failed' ? taskDisplayStatus(task).label : task.title);

const line = (
  key: string,
  at: number,
  from: FleetSpeaker,
  to: FleetSpeaker,
  text: string,
  outcome: FleetOutcome | null = null,
  live: FleetLine['live'] = null,
  message: FleetLine['message'] = null
): FleetLine => ({ key, at, from, to, text: plainText(text), markdown: text, message, headline: firstSentence(text), outcome, live });

const fromSaid = (said: FleetSaid, from: FleetSpeaker, to: FleetSpeaker, outcome: FleetOutcome | null = null) =>
  line(said.id, said.at, from, to, said.text, outcome, null, { sessionId: said.sessionId, id: said.id, truncated: said.truncated });

/**
 * A miniature's highlights, oldest first: the user's messages and Main's
 * replies on the main session, what agents told each other (Main to a task,
 * a task's report to whoever handed it over), and each task handed over and
 * ended. Tool calls are never said. A task's report is how it ended; its own
 * status says so only when no report has arrived.
 */
export const cardFeed = (card: FleetCard, live: FleetLive, now: number): FleetFeed => {
  const tasks = workspaceTasks(card, live);
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const bySession = new Map(tasks.flatMap((t) => (t.sessionId ? [[t.sessionId, t] as const] : [])));
  const main = card.mainSessionId;
  const ownerOf = (sessionId: string): FleetSpeaker | null => {
    if (sessionId === main) return MAIN;
    const task = bySession.get(sessionId);
    return task ? agentSpeaker(agentOf(task)) : null;
  };

  const lines: FleetLine[] = [];
  const reported = new Set<string>();
  for (const said of card.said) {
    const to = said.role === 'assistant' ? (said.sessionId === main ? YOU : null) : ownerOf(said.sessionId);
    const origin = said.origin;
    if (!to) continue;
    if (!origin) {
      lines.push(fromSaid(said, MAIN, to));
    } else if (origin.kind === 'user') {
      lines.push(fromSaid(said, YOU, to));
    } else if (origin.kind === 'agent') {
      if (said.text.startsWith(ASSIGNMENT_PREFIX)) continue;
      const from = origin.agent.isDefault ? MAIN : agentSpeaker(agentRef(origin.agent));
      lines.push(fromSaid(said, from, to));
    } else {
      reported.add(`${origin.taskId}:${origin.cycleNo}`);
      lines.push(fromSaid(said, agentSpeaker(agentRef(origin.agent)), to, OUTCOMES[origin.status] ?? null));
    }
  }
  for (const task of tasks) {
    const agent = agentSpeaker(agentOf(task));
    const parent = task.parentTaskId ? byId.get(task.parentTaskId) : undefined;
    const boss = parent ? agentSpeaker(agentOf(parent)) : MAIN;
    const state = task.status === 'queued' ? 'queued' : isTaskLive(task.status) ? 'running' : null;
    lines.push(line(`task:${task.id}`, Number(task.createdAt), boss, agent, task.title, null, state));
    const outcome = OUTCOMES[task.status];
    if (outcome && !reported.has(`${task.id}:${task.cycleNo}`)) {
      lines.push(line(`ended:${task.id}`, Number(task.completedAt ?? task.updatedAt), agent, boss, resultOf(task), outcome));
    }
  }
  lines.sort((a, b) => a.at - b.at);
  const shown = lines.slice(-FEED_LIMIT);

  const working = tasks.filter((t) => isTaskLive(t.status)).length;
  const ended = [...shown].reverse().find((l) => l.outcome === 'done' || l.outcome === 'failed');
  const lastWord = shown.at(-1)?.at ?? card.lastActivityAt;
  const mood: FleetMood =
    working > 0 || mainRunning(card, live)
      ? 'working'
      : ended && now - ended.at < FRESH_MS
        ? 'fresh'
        : lastWord > 0 && now - lastWord >= STALE_MS
          ? 'stale'
          : 'idle';
  const caption: FleetCaption =
    mood === 'working'
      ? { kind: 'working', count: working }
      : mood === 'fresh'
        ? { kind: 'fresh', at: ended!.at }
        : lastWord > 0
          ? { kind: mood, at: lastWord }
          : { kind: 'empty' };
  return { mood, caption, lines: shown };
};

/** Bubbles a miniature draws before it keeps room for a row of what came before. */
export const MIN_BUBBLES = 3;

/**
 * How many of a miniature's highlights are bubbles and how many rows, given
 * each highlight's height as a bubble (oldest first, with its gap) and the
 * height the card has for them. Bubbles fill from the newest as long as they
 * fit; the newest is a bubble even when it doesn't. Past `MIN_BUBBLES`, a
 * bubble also leaves room for one row when older highlights remain. Up to
 * `ROWS` older ones collapse to rows of `rowHeight` above the bubbles, as
 * many as fit in what is left; `rowsEdge` is the rule under the last row.
 * Older ones hide.
 */
export const splitFeed = (
  heights: readonly number[],
  available: number,
  rowHeight: number,
  rowsEdge = 0
): { bubbles: number; rows: number } => {
  const n = heights.length;
  let used = 0;
  let bubbles = 0;
  while (bubbles < n) {
    const next = heights[n - 1 - bubbles]!;
    const reserve = bubbles >= MIN_BUBBLES && n - bubbles > 1 ? rowHeight + rowsEdge : 0;
    if (bubbles > 0 && used + next + reserve > available) break;
    used += next;
    bubbles += 1;
  }
  let rows = Math.min(ROWS, n - bubbles);
  while (rows > 0 && used + rows * rowHeight + rowsEdge > available) rows -= 1;
  return { bubbles, rows };
};

/**
 * How loud a beat still is: a "GO!" or "DONE!" is drawn in full for half of
 * `FRESH_MS`, at half strength for the rest, then not at all.
 */
export type FleetShout = 'full' | 'half' | null;

export const shoutLevel = (at: number, now: number): FleetShout =>
  now - at < FRESH_MS / 2 ? 'full' : now - at < FRESH_MS ? 'half' : null;

/**
 * How loud a panel is drawn: it needs the user, or a task on it just
 * finished well.
 */
export type FleetLoudness = 'needs' | 'done' | null;

export interface FleetPanel {
  card: FleetCard;
  /** `null` while the card shows an open request. */
  feed: FleetFeed | null;
  selected: boolean;
}

export interface FleetLoud {
  /** Loudness by workspace id; a card missing here is quiet. */
  cards: Record<string, FleetLoudness>;
  /** Keys of the lines that shout: the newest running task, the loud card's newest result. */
  rows: ReadonlySet<string>;
}

/**
 * What the page draws loud. Every card that needs the user; otherwise at
 * most one, the card with the newest good result. A selected card never is:
 * the user is already looking at it. Of the running tasks, only the newest
 * rushes in; the rest just think.
 */
export const fleetLoudness = (panels: FleetPanel[]): FleetLoud => {
  const cards: Record<string, FleetLoudness> = {};
  let go: FleetLine | undefined;
  let done: { id: string; line: FleetLine } | undefined;
  for (const { card, feed, selected } of panels) {
    if (!selected && openRequests(card).length > 0) cards[card.workspaceId] = 'needs';
    for (const l of feed?.lines ?? []) if (l.live === 'running' && (!go || l.at > go.at)) go = l;
    if (selected || feed?.mood !== 'fresh') continue;
    // A failure is news too, but not a cheer.
    for (const l of feed.lines) if (l.outcome === 'done' && (!done || l.at > done.line.at)) done = { id: card.workspaceId, line: l };
  }
  const rows = new Set(go ? [go.key] : []);
  if (done && Object.keys(cards).length === 0) {
    cards[done.id] = 'done';
    rows.add(done.line.key);
  }
  return { cards, rows };
};

/** A card that needs the user, as the comic draws it. */
export interface FleetRequestScene {
  request: FleetRequest;
  /** Open requests, this one included: they are answered one at a time, oldest first. */
  count: number;
  caption: FleetCaption;
  /** The asking agent, or `null` for Main. */
  asker: FleetAgentRef | null;
}

/**
 * A card that needs the user: its oldest open request, said by the agent that
 * asked it. A question names its session, which is a task's or the Main's; an
 * approval or a grant names its agent, and Main's name is no agent's.
 */
export const requestScene = (card: FleetCard, live: FleetLive): FleetRequestScene | null => {
  const open = openRequests(card);
  const request = open[0];
  if (!request) return null;
  let asker: FleetAgentRef | null = null;
  if (request.kind === 'ask') {
    const task = workspaceTasks(card, live).find((t) => t.sessionId === request.ask.sessionId);
    asker = task ? agentOf(task) : null;
  } else if (request.agentName) {
    asker = { name: request.agentName, seed: request.agentName };
  }
  return { request, count: open.length, caption: { kind: 'waiting', at: request.at }, asker };
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


// ── Which cards, in which order ─────────────────────────────────────────

/**
 * The cards, in order. Eligible: the selected card, anything live, and
 * workspaces active within `RECENT_WINDOW_MS`. Cards already shown keep
 * their place (`prevOrder`); newcomers follow, waiting ones first (oldest
 * first), then the most recent. Selecting a card moves nothing: it is only
 * highlighted.
 */
export const pickCards = (
  cards: readonly FleetCard[],
  options: { now: number; selectedId: string | null; prevOrder: readonly string[]; live: FleetLive }
): string[] => {
  const { now, selectedId, prevOrder, live } = options;
  const meta = cards
    .map((card) => ({ id: card.workspaceId, card, phase: cardPhase(card, live), since: cardAttention(card).since ?? 0 }))
    .filter(
      (m) => m.id === selectedId || m.phase !== 'stopped' || m.card.lastActivityAt >= now - RECENT_WINDOW_MS
    );
  const rank = (m: (typeof meta)[number]): [number, number] =>
    m.phase === 'waiting' ? [0, m.since] : [1, -m.card.lastActivityAt];
  const chosen = meta.sort((a, b) => {
    const [ra, ka] = rank(a);
    const [rb, kb] = rank(b);
    return ra - rb || ka - kb;
  });
  const ids = new Set(chosen.map((m) => m.id));
  const order = prevOrder.filter((id) => ids.has(id));
  order.push(...chosen.filter((m) => !order.includes(m.id)).map((m) => m.id));
  return order;
};

export const cycleCard = (order: readonly string[], selectedId: string | null, step: 1 | -1): string | null => {
  if (order.length === 0) return null;
  const at = selectedId ? order.indexOf(selectedId) : -1;
  if (at < 0) return step > 0 ? order[0]! : order[order.length - 1]!;
  return order[(at + step + order.length) % order.length]!;
};

/**
 * The switcher's order: most recently opened by the user first, so it doesn't
 * reshuffle while agents work; never opened, or a tie, by latest activity.
 */
export const recentlyOpened = (ids: readonly string[], cards: Record<string, FleetCard>): string[] =>
  ids
    .filter((id) => cards[id])
    .sort((a, b) => {
      const ca = cards[a]!;
      const cb = cards[b]!;
      return cb.lastOpenedAt - ca.lastOpenedAt || cb.lastActivityAt - ca.lastActivityAt;
    });

/** The grid: columns, and rows on screen before it scrolls. */
export interface BoardShape {
  cols: number;
  rows: number;
}

/**
 * Six cards 3 × 2, scrolling beyond; with fewer, each grows. Never fewer than
 * two columns, so a lone card's bubbles don't stretch into very long lines.
 */
export const boardShape = (count: number): BoardShape => {
  if (count <= 3) return { cols: Math.max(2, count), rows: 1 };
  return count === 4 ? { cols: 2, rows: 2 } : { cols: 3, rows: 2 };
};
