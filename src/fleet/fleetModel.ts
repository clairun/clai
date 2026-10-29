/**
 * The Fleet's state: one card per workspace, built by a pure reducer over
 * the app-global event streams (assistant, approvals, path grants) and a
 * startup snapshot. Streaming variants (deltas, tool calls) return the state
 * untouched, so they never reach React. Runs and tasks are not copied here:
 * they come from `fleetActivityStore` and `taskStore` (`FleetLive`).
 *
 * A card keeps the messages said in the workspace (`said`): what the user,
 * Main and the agents told each other. Main's words are heard a block of
 * text at a time, at each tool call (`assistant_message_updated`, at most
 * five a second) and when the message completes; deltas never reach the
 * Fleet. `cardFeed` turns them, with the tasks, into the miniature's
 * highlights, in the order they happened.
 */
import type {
  AgentRefSnapshot,
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantRun,
  AssistantUiEvent,
  FleetHighlight,
  FleetWorkspace,
  MessageOrigin,
  PathGrantRequest,
  PermissionRequest,
  TaskStatus,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import { imageParts, type ImagePart } from '../assistant/imageParts';
import { isRunMarkerText } from '../assistant/runMarker';
import type { PendingAskUser } from '../assistant/sessionStore';

/** A workspace with no event in this window and nothing live gets no card. */
export const RECENT_WINDOW_MS = 12 * 60 * 60 * 1000;
/** Messages a card keeps; a miniature shows fewer. */
export const SAID_LIMIT = 24;

/** A command approval or a path grant, as it arrives. */
export type FleetPendingRequest =
  | { kind: 'approval'; request: PermissionRequest }
  | { kind: 'grant'; request: PathGrantRequest };

export type FleetRequestKind = FleetPendingRequest['kind'];

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
} & ({ kind: 'ask'; ask: FleetAsk } | FleetPendingRequest);

/** How far an assistant message's text had got (`end`, in characters) at `at`. */
interface FleetMark {
  at: number;
  end: number;
}

/** A message said in the workspace, as the feed needs it. */
export interface FleetSaid {
  id: string;
  sessionId: string;
  role: 'user' | 'assistant';
  /** Who sent a user message; `null` for an assistant message. */
  origin: MessageOrigin | null;
  /** Whole as heard, without tool calls (`fullMessageText`), or the snapshot's preview ending in `…` until fetched whole (`applyWholeMessage`). */
  text: string;
  /** The snapshot's preview: the whole message has to be fetched. */
  truncated: boolean;
  /** A user message's pictures, which the chat draws too; none for an assistant message. */
  images: readonly ImagePart[];
  /** When last heard. */
  at: number;
  runId: string | null;
  /** An assistant message, block by block as it was heard, oldest first; empty for a user message. */
  marks: readonly FleetMark[];
}

export interface FleetCard {
  workspaceId: string;
  title: string;
  /** From the snapshot, or set when the user sends or clears from the terminal. */
  mainSessionId: string | null;
  /** The main session's newest run: from the snapshot, then its run events. */
  lastMainRun: AssistantRun | null;
  lastActivityAt: number;
  /** When the user last opened the workspace (here or on its page); 0 = never. */
  lastOpenedAt: number;
  /** Open requests, by `kind:id`. */
  requests: Record<string, FleetRequest>;
  /** Oldest first, at most `SAID_LIMIT`. */
  said: FleetSaid[];
  /** When questions were asked (`ask_user`), at most `SAID_LIMIT`: each closes Main's bubble. */
  asked: number[];
  /** The workspace's enabled members, Main excluded, in roster order (from the snapshot). */
  agents: FleetAgentRef[];
  /** The workspace's Main, for its face (from the snapshot); `null` until a snapshot names it. */
  main: FleetMainRef | null;
}

export interface FleetAgentRef {
  name: string;
  seed: string;
}

/** What Main's face is drawn from (`mainIdentity`). */
export interface FleetMainRef {
  id: string;
  avatar: AgentRefSnapshot['avatar'];
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
  lastMainRun: null,
  lastActivityAt: 0,
  lastOpenedAt: 0,
  requests: {},
  said: [],
  asked: [],
  agents: [],
  main: null,
});

const updateCard = (
  state: FleetState,
  workspaceId: string,
  update: (card: FleetCard) => FleetCard
): FleetState => {
  const current = state.cards[workspaceId] ?? emptyCard(workspaceId);
  const next = update(current);
  if (next === current) return state;
  return { cards: { ...state.cards, [workspaceId]: next } };
};

const bump = (card: FleetCard, at: number): FleetCard =>
  at > card.lastActivityAt ? { ...card, lastActivityAt: at } : card;


export const fullMessageText = (message: AssistantMessage): string => {
  const blocks: string[] = [];
  let current = '';
  for (const part of message.content) {
    if (part.type === 'text') {
      current += part.text;
    } else if (current) {
      blocks.push(current);
      current = '';
    }
  }
  if (current) blocks.push(current);
  return blocks.join(message.role === 'assistant' ? '\n\n' : '').trim();
};

/** What a bubble says of a message (`fullMessageText`), ending in `…` when the snapshot cut it. */
const saidText = (message: AssistantMessage, cut: boolean): string => `${fullMessageText(message)}${cut ? '…' : ''}`;

export const isTaskLive = (status: TaskStatus): boolean =>
  status === 'queued' || status === 'running' || status === 'stopping';

const userSaid = (message: AssistantMessage, at: number, runId: string | null, cut = false): FleetSaid => ({
  id: message.id,
  sessionId: message.sessionId,
  role: 'user',
  origin: message.origin,
  text: saidText(message, cut),
  truncated: cut,
  images: imageParts(message.content),
  at,
  runId,
  marks: [],
});

const assistantSaid = (message: AssistantMessage, text: string, runId: string | null, marks: readonly FleetMark[]): FleetSaid => ({
  id: message.id,
  sessionId: message.sessionId,
  role: 'assistant',
  origin: null,
  text,
  truncated: false,
  images: [],
  at: marks.at(-1)!.at,
  runId,
  marks,
});

/**
 * Keeps `said` in time order and at most `SAID_LIMIT` long; an event replaces
 * what a seed said. A message with neither words nor pictures, or an
 * automation's run marker, is never said.
 */
const addSaid = (card: FleetCard, said: readonly FleetSaid[], replace: boolean): FleetCard => {
  const held = new Set(card.said.map((s) => s.id));
  const incoming = said.filter(
    (s) => (s.text || s.images.length > 0) && !(s.role === 'user' && isRunMarkerText(s.text)) && (replace || !held.has(s.id))
  );
  if (incoming.length === 0) return card;
  const ids = new Set(incoming.map((s) => s.id));
  const next = [...card.said.filter((s) => !ids.has(s.id)), ...incoming].sort((a, b) => a.at - b.at);
  return { ...card, said: next.slice(-SAID_LIMIT) };
};

/**
 * Main's `message` as heard now: mid-run at a tool call, or complete. The
 * text past what was heard is the next block, heard at `at`; nothing new
 * changes nothing. Text that doesn't continue what was heard (an edit)
 * restates the message as one block.
 */
const hear = (card: FleetCard, message: AssistantMessage, runId: string | null, at: number): FleetCard => {
  if (message.role !== 'assistant') return card;
  const text = fullMessageText(message);
  const held = card.said.find((s) => s.id === message.id);
  if (!text || text === held?.text) return card;
  const marks =
    held && text.startsWith(held.text) ? [...held.marks, { at, end: text.length }] : [{ at: held?.marks[0]?.at ?? at, end: text.length }];
  return addSaid(card, [assistantSaid(message, text, runId ?? held?.runId ?? null, marks)], true);
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

/** The variants the Fleet reads. Everything else, deltas above all, is ignored. */
const FLEET_EVENTS: ReadonlySet<AssistantUiEvent['type']> = new Set([
  'message_created',
  'message_deleted',
  'run_queued',
  'run_started',
  'run_completed',
  'run_failed',
  'run_cancelled',
  'assistant_message_updated',
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
    if (event.type === 'assistant_message_updated') {
      // A tool call mid-run: news only when it closed a block of Main's text.
      if (current.mainSessionId && current.mainSessionId !== sessionId) return current;
      const heard = hear(current, event.payload.message, envelope.runId, at);
      return heard === current ? current : bump(heard, at);
    }
    const card = bump(current, at);
    switch (event.type) {
      case 'message_created': {
        // Every session's user messages: the user's, an agent's and a task's
        // report. An assistant message is heard as it is written.
        const { message } = event.payload;
        return message.role === 'user' ? addSaid(card, [userSaid(message, at, envelope.runId)], true) : card;
      }
      case 'message_deleted': {
        const said = card.said.filter((s) => s.id !== event.payload.message_id);
        return said.length === card.said.length ? card : { ...card, said };
      }
      case 'assistant_message_completed':
        // Main's replies, to the user. A task's own replies reach Main as its report;
        // before the main session is known, keep them and let `cardFeed` choose.
        if (card.mainSessionId && card.mainSessionId !== sessionId) return card;
        return hear(card, event.payload.message, envelope.runId, at);
      case 'run_queued':
      case 'run_started':
      case 'run_completed':
      case 'run_failed':
      case 'run_cancelled':
        // A session's runs arrive in order: the latest event is its newest run.
        return sessionId === card.mainSessionId ? { ...card, lastMainRun: event.payload.run } : card;
      case 'ask_user_requested':
        return {
          ...card,
          asked: [...card.asked, at].slice(-SAID_LIMIT),
          requests: {
            ...card.requests,
            [requestKey('ask', event.payload.pending_id)]: askRequest(
              {
                pendingId: event.payload.pending_id,
                question: event.payload.question,
                options: event.payload.options,
                multiSelect: event.payload.multi_select,
                extraContext: event.payload.extra_context,
                sessionId,
              },
              at
            ),
          },
        };
      case 'ask_user_resolved':
        return resolve(card, requestKey('ask', event.payload.pending_id));
      default:
        // `task_updated`: activity only; tasks come from the task store.
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
    const next = { ...bump(card, at), mainSessionId: sessionId,
      lastMainRun: card.mainSessionId === sessionId ? card.lastMainRun : null };
    return message ? addSaid(next, [userSaid(message, at, null)], true) : next;
  });

/** The user opened the workspace at `at`: the switcher's order knows it before the next snapshot. */
export const applyOpened = (state: FleetState, workspaceId: string, at: number): FleetState =>
  state.cards[workspaceId] ? updateCard(state, workspaceId, (card) => ({ ...card, lastOpenedAt: Math.max(card.lastOpenedAt, at) })) : state;

/** A snapshot's message, as if heard when it was created: an assistant one is a single block, its preview when cut. */
const seedSaid = ({ message, runId, truncated }: FleetHighlight): FleetSaid => {
  const at = Number(message.createdAt);
  if (message.role !== 'assistant') return userSaid(message, at, runId, truncated);
  const text = saidText(message, truncated);
  return { ...assistantSaid(message, text, runId, [{ at, end: text.length }]), truncated };
};

/** A message the snapshot cut, fetched whole (`loadWhole` in the store), in place of its preview. */
export const applyWholeMessage = (state: FleetState, workspaceId: string, message: AssistantMessage): FleetState =>
  updateCard(state, workspaceId, (card) => {
    const held = card.said.find((s) => s.id === message.id);
    return held?.truncated ? addSaid(card, [seedSaid({ message, runId: held.runId, truncated: false })], true) : card;
  });

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
    // Main's last reply, which the recent messages of every session may leave out.
    const last = ws.lastAssistantMessage;
    const seed = (last ? [...ws.recentMessages.filter((h) => h.message.id !== last.message.id), last] : ws.recentMessages).map(seedSaid);
    // Approvals and grants carry no timestamp; the workspace's activity stands in.
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
        mainSessionId: ws.mainSessionId ?? card.mainSessionId,
        lastMainRun: ws.lastMainRun,
        lastActivityAt: Math.max(card.lastActivityAt, activity),
        lastOpenedAt: Math.max(card.lastOpenedAt, Number(ws.lastOpenedAt)),
        requests,
        asked: [...new Set([...card.asked, ...ws.pendingAsks.map((a) => Number(a.requestedAt))])].sort((a, b) => a - b).slice(-SAID_LIMIT),
        agents: ws.agents.flatMap((a) => agentRef(a) ?? []),
        main: mainRef(ws.agents) ?? card.main,
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
  };
};

export const workspaceTasks = (card: FleetCard, live: FleetLive): WorkspaceTaskResponse[] =>
  Object.values(live.tasks[card.workspaceId] ?? {});

/** Working: any run in the workspace, main or not, or any live task. */
export const cardPhase = (card: FleetCard, live: FleetLive): FleetPhase => {
  if (cardAttention(card).total > 0) return 'waiting';
  if ((live.runs[card.workspaceId]?.length ?? 0) > 0) return 'working';
  return workspaceTasks(card, live).some((t) => isTaskLive(t.status)) ? 'working' : 'stopped';
};

/** Who a task was handed to, with the same face seed as its messages; `null` for the Main's own task. */
export const assigneeOf = (card: FleetCard, task: WorkspaceTaskResponse): FleetAgentRef | null =>
  card.main && task.assignedToWorkspaceAgentId === card.main.id
    ? null
    : { name: task.assignedAgentDisplayName, seed: task.assignedAgentDefinitionId || task.assignedToWorkspaceAgentId };

/** An agent as a message names it, with the same face seed as its tasks; `null` for the Main. */
export const agentRef = (agent: AgentRefSnapshot): FleetAgentRef | null =>
  agent.isDefault ? null : { name: agent.displayName, seed: agent.agentDefinitionId || agent.workspaceAgentId };

const mainRef = (agents: readonly AgentRefSnapshot[]): FleetMainRef | null => {
  const main = agents.find((a) => a.isDefault);
  return main ? { id: main.workspaceAgentId, avatar: main.avatar ?? null } : null;
};

/** Every distinct agent other than the Main with a task on the card, live ones first, then the most recent. */
export const cardAgents = (card: FleetCard, live: FleetLive): FleetAgentRef[] => {
  const tasks = workspaceTasks(card, live).sort(
    (a, b) => Number(isTaskLive(b.status)) - Number(isTaskLive(a.status)) || Number(b.updatedAt) - Number(a.updatedAt)
  );
  const agents = tasks.flatMap((t) => assigneeOf(card, t) ?? []);
  return [...new Map(agents.map((a) => [a.seed, a])).values()];
};

/** A run of the workspace that belongs to no task is its Main's. */
export const mainRunning = (card: FleetCard, live: FleetLive): boolean => {
  const taskRuns = new Set(workspaceTasks(card, live).map((t) => t.runId));
  return (live.runs[card.workspaceId] ?? []).some((id) => !taskRuns.has(id));
};
