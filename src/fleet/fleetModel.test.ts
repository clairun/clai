import { describe, expect, it } from 'vitest';
import type {
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantRun,
  AssistantUiEvent,
  FleetWorkspace,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import {
  EMPTY_FLEET,
  RECENT_WINDOW_MS,
  applyAssistantEvent,
  applyRequest,
  applyRequestResolved,
  applyUserSent,
  cardAttention,
  cardItems,
  cardPhase,
  emptyCard,
  gridCapacity,
  placeCards,
  seedFleet,
  visibleLastMessage,
  type FleetCard,
  type FleetLive,
  type FleetState,
  type PlacedCard,
} from './fleetModel';

const WS = 'ws-1';
const MAIN = 'main-session';
const NO_LIVE: FleetLive = { runs: {}, tasks: {} };

const env = (event: AssistantUiEvent, at: number, sessionId = MAIN, workspaceId = WS): AssistantEventEnvelope =>
  ({ sessionId, runId: null, workspaceId, timestamp: BigInt(at), event }) as AssistantEventEnvelope;

const message = (id: string, role: 'user' | 'assistant', text: string, origin: AssistantMessage['origin'] = { kind: 'user' }): AssistantMessage =>
  ({ id, sessionId: MAIN, role, content: [{ type: 'text', text }], createdAt: 0n, providerMetadata: null, origin }) as AssistantMessage;

const run = (id: string): AssistantRun =>
  ({ id, sessionId: MAIN, status: 'running', trigger: 'user_message', connectionId: 'c', protocolId: 'p', modelId: 'm', startedAt: 0n, completedAt: null, error: null }) as AssistantRun;

const task = (id: string, status: WorkspaceTaskResponse['status'], updatedAt: number, extra: Partial<WorkspaceTaskResponse> = {}): WorkspaceTaskResponse =>
  ({
    id,
    workspaceId: WS,
    title: `Task ${id}`,
    status,
    failureReason: null,
    runId: null,
    sessionId: `task-session-${id}`,
    parentTaskId: null,
    assignedAgentDisplayName: 'Researcher',
    assignedAgentDefinitionId: 'def-r',
    assignedToWorkspaceAgentId: 'wa-r',
    updatedAt: BigInt(updatedAt),
    ...extra,
  }) as unknown as WorkspaceTaskResponse;

const withTasks = (...tasks: WorkspaceTaskResponse[]): FleetLive => ({
  runs: {},
  tasks: { [WS]: Object.fromEntries(tasks.map((t) => [t.id, t])) },
});

const askEvent = (pendingId: string, at: number) =>
  env({ type: 'ask_user_requested', payload: { pending_id: pendingId, question: 'Ship it?', options: null, multi_select: false } } as AssistantUiEvent, at);

const approval = (requestId: string) =>
  ({ requestId, workspaceId: WS, agentId: null, agentName: 'Main', command: 'ls', segments: [] }) as never;

const apply = (state: FleetState, ...envelopes: AssistantEventEnvelope[]) =>
  envelopes.reduce(applyAssistantEvent, state);

const card = (state: FleetState, id = WS): FleetCard => state.cards[id]!;

/** A workspace whose main session is `MAIN`, as the snapshot or a send would set it. */
const withMain = (state: FleetState = EMPTY_FLEET) => applyUserSent(state, WS, MAIN, null, 0);

describe('fleet reducer', () => {
  it('ignores streaming variants without producing a new state', () => {
    const base = apply(EMPTY_FLEET, env({ type: 'run_started', payload: { run: run('r1') } }, 1));
    const streaming: AssistantUiEvent[] = [
      { type: 'assistant_delta', payload: { message_id: 'm', text: 'x' } },
      { type: 'assistant_thinking_delta', payload: { message_id: 'm', text: 'x' } },
      { type: 'assistant_message_updated', payload: { message: message('m', 'assistant', 'x') } },
    ] as AssistantUiEvent[];
    for (const event of streaming) {
      expect(applyAssistantEvent(base, env(event, 99))).toBe(base);
    }
  });

  it("shows the user's own message until another message or event arrives", () => {
    let s = apply(withMain(), env({ type: 'message_created', payload: { message: message('u1', 'user', 'Please deploy') } }, 1));
    expect(card(s).userMessage?.text).toBe('Please deploy');
    // Run lifecycle is not a displayed event: the message stays, but counts as activity.
    s = apply(s, env({ type: 'run_started', payload: { run: run('r1') } }, 2));
    expect(card(s).userMessage?.text).toBe('Please deploy');
    expect(card(s).lastActivityAt).toBe(2);
    s = apply(s, env({ type: 'task_updated', payload: { task: task('t1', 'running', 3) } }, 3));
    expect(card(s).userMessage).toBeNull();
    s = apply(s, env({ type: 'message_created', payload: { message: message('u2', 'user', 'Also this') } }, 4));
    s = apply(s, env({ type: 'assistant_message_completed', payload: { message: message('a1', 'assistant', 'On it') } }, 5));
    expect(card(s).userMessage).toBeNull();
    expect(card(s).lastMessage?.text).toBe('On it');
  });

  it('does not take a task-result message for the user', () => {
    const s = apply(
      withMain(),
      env({ type: 'message_created', payload: { message: message('u1', 'user', 'go') } }, 1),
      env({ type: 'message_created', payload: { message: message('m2', 'user', 'result', { kind: 'task', taskId: 't', agent: { workspaceAgentId: 'a', displayName: 'R', isDefault: false }, title: 'T', cycleNo: 1n, status: 'completed' }) } }, 2),
    );
    expect(card(s).userMessage).toBeNull();
  });

  it('reads messages from the main session only, and never guesses which one that is', () => {
    const fromTask = [
      env({ type: 'message_created', payload: { message: message('u1', 'user', 'from the human') } }, 1, 'task-session'),
      env({ type: 'assistant_message_completed', payload: { message: message('x', 'assistant', 'task text') } }, 2, 'task-session'),
    ];
    // No main known yet: nothing is adopted, whatever the origin.
    let s = apply(EMPTY_FLEET, ...fromTask);
    expect(card(s).mainSessionId).toBeNull();
    expect(card(s).userMessage).toBeNull();
    expect(card(s).lastMessage).toBeNull();
    expect(card(s).lastActivityAt).toBe(2);
    s = apply(withMain(), ...fromTask);
    expect(card(s).mainSessionId).toBe(MAIN);
    expect(card(s).userMessage).toBeNull();
    expect(card(s).lastMessage).toBeNull();
  });

  it('/clear moves the main to the fresh session', () => {
    let s = apply(withMain(), env({ type: 'message_created', payload: { message: message('u1', 'user', 'old') } }, 1));
    s = applyUserSent(s, WS, 'fresh', null, 2);
    expect(card(s).mainSessionId).toBe('fresh');
    expect(card(s).userMessage).toBeNull();
    s = apply(s, env({ type: 'assistant_message_completed', payload: { message: message('a', 'assistant', 'Hi again') } }, 3, 'fresh'));
    expect(card(s).lastMessage?.text).toBe('Hi again');
  });

  it('drops a deleted user message', () => {
    let s = apply(withMain(), env({ type: 'message_created', payload: { message: message('u1', 'user', 'oops') } }, 1));
    s = apply(s, env({ type: 'message_deleted', payload: { message_id: 'other' } } as AssistantUiEvent, 2));
    expect(card(s).userMessage?.id).toBe('u1');
    s = apply(s, env({ type: 'message_deleted', payload: { message_id: 'u1' } } as AssistantUiEvent, 3));
    expect(card(s).userMessage).toBeNull();
  });

  it('resolves each request by its own resolved event, user decisions included', () => {
    let s = applyRequest(EMPTY_FLEET, 'approval', approval('a1'), 1);
    s = applyRequest(s, 'approval', approval('a2'), 2);
    s = applyRequest(s, 'grant', approval('a1'), 3);
    expect(cardAttention(card(s))).toMatchObject({ approvals: 2, grants: 1, total: 3, since: 1 });
    s = applyRequestResolved(s, 'approval', 'a1', 4);
    expect(cardAttention(card(s))).toMatchObject({ approvals: 1, grants: 1, total: 2, since: 2 });
    expect(card(s).requests['grant:a1']!.resolvedAt).toBeNull();
    // A duplicate resolve keeps the first time.
    expect(applyRequestResolved(s, 'approval', 'a1', 9)).toBe(s);
  });

  it('keeps finished subtasks and resolved requests as low-emphasis history', () => {
    const live = withTasks(task('done', 'completed', 1), task('live', 'running', 2));
    let s = apply(EMPTY_FLEET, askEvent('q1', 3), env({ type: 'ask_user_resolved', payload: { pending_id: 'q1' } } as AssistantUiEvent, 4));
    s = applyRequest(s, 'approval', approval('a1'), 5);
    expect(cardItems(card(s), live).map((i) => [i.key, i.tone])).toEqual([
      ['approval:a1', 'attention'],
      ['task:live', 'live'],
      ['ask:q1', 'done'],
      ['task:done', 'done'],
    ]);
    s = applyRequestResolved(s, 'approval', 'a1', 6);
    expect(cardItems(card(s), live)[0]).toMatchObject({ key: 'task:live' });
  });

  it('folds subtasks into their top-level task and stays live while any descendant runs', () => {
    const live = withTasks(
      task('root', 'completed', 1),
      task('child', 'running', 2, { parentTaskId: 'root', assignedAgentDefinitionId: 'def-w', assignedAgentDisplayName: 'Writer' }),
    );
    const items = cardItems(emptyCard(WS), live);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ tone: 'live', status: 'Subtasks running' });
    expect(items[0]!.agents.map((a) => a.name)).toEqual(['Researcher', 'Writer']);
  });

  it('labels tasks the way the rest of the app does', () => {
    const live = withTasks(
      task('done', 'completed', 3),
      task('lost', 'failed', 2, { failureReason: 'restart' }),
      task('stuck', 'failed', 1, { failureReason: 'blocked' }),
    );
    expect(cardItems(emptyCard(WS), live).map((i) => [i.status, i.statusKey])).toEqual([
      ['Completed', 'completed'],
      ['Failed · interrupted', 'failed'],
      ['Blocked', 'blocked'],
    ]);
  });

  it('is working while any run of the workspace or any task is live, and waiting while anything is open', () => {
    const c = emptyCard(WS);
    expect(cardPhase(c, NO_LIVE)).toBe('stopped');
    expect(cardPhase(c, { runs: { [WS]: ['task-run'] }, tasks: {} })).toBe('working');
    expect(cardPhase(c, { runs: { other: ['r'] }, tasks: {} })).toBe('stopped');
    expect(cardPhase(c, withTasks(task('t', 'queued', 1)))).toBe('working');
    expect(cardPhase(c, withTasks(task('t', 'failed', 1)))).toBe('stopped');
    const asking = card(apply(EMPTY_FLEET, askEvent('q', 1)));
    expect(cardPhase(asking, { runs: { [WS]: ['r'] }, tasks: {} })).toBe('waiting');
  });

  it('shows the last assistant message only once the workspace has stopped', () => {
    const s = apply(withMain(), env({ type: 'assistant_message_completed', payload: { message: message('a1', 'assistant', 'All done.') } }, 3));
    expect(visibleLastMessage(card(s), { runs: { [WS]: ['r'] }, tasks: {} })).toBeNull();
    expect(visibleLastMessage(card(s), withTasks(task('t1', 'running', 2)))).toBeNull();
    expect(visibleLastMessage(card(s), withTasks(task('t1', 'completed', 5)))?.text).toBe('All done.');
  });
});

describe('snapshot', () => {
  const snapshotOf = (patch: Partial<FleetWorkspace> = {}): FleetWorkspace => ({
    workspaceId: WS,
    title: 'Infra',
    starred: false,
    lastActivityAt: 10n,
    mainSessionId: MAIN,
    activeRunIds: [],
    tasks: [],
    pendingPermissions: [],
    pendingPathGrants: [],
    pendingAsks: [],
    lastAssistantMessage: null,
    ...patch,
  });

  it('keeps history, resolves what is no longer pending, and adds what is', () => {
    let s = apply(EMPTY_FLEET, askEvent('stale', 2), askEvent('kept', 3));
    s = applyRequest(s, 'approval', approval('gone'), 4);
    const pendingAsk = (pendingId: string) => ({ pendingId, workspaceId: WS, sessionId: MAIN, runId: 'r', question: '?', options: null, multiSelect: false, extraContext: null, requestedAt: 7n });
    s = seedFleet(s, [snapshotOf({ pendingAsks: [pendingAsk('kept'), pendingAsk('new')], pendingPathGrants: [approval('g1')] })], 50);
    const c = card(s);
    expect(c.title).toBe('Infra');
    expect(c.mainSessionId).toBe(MAIN);
    expect(c.requests['ask:stale']!.resolvedAt).toBe(50);
    expect(c.requests['ask:kept']).toMatchObject({ at: 3, resolvedAt: null });
    expect(c.requests['ask:new']).toMatchObject({ kind: 'ask', text: '?', at: 7, resolvedAt: null });
    expect(c.requests['approval:gone']!.resolvedAt).toBe(50);
    // A request has no timestamp of its own: it takes the workspace's activity.
    expect(c.requests['grant:g1']).toMatchObject({ at: 10, resolvedAt: null });
  });

  it('removes cards missing from the snapshot, e.g. deleted workspaces', () => {
    let s = apply(EMPTY_FLEET, askEvent('q', 1), env({ type: 'run_started', payload: { run: run('r') } }, 2, 's', 'deleted'));
    s = seedFleet(s, [snapshotOf()], 50);
    expect(Object.keys(s.cards)).toEqual([WS]);
  });

  it('keeps the newer of the held and the snapshot last message', () => {
    const preview = { messageId: 'p', text: 'from snapshot', truncated: true, createdAt: 5n };
    let s = seedFleet(EMPTY_FLEET, [snapshotOf({ lastAssistantMessage: preview })], 50);
    expect(card(s).lastMessage?.text).toBe('from snapshot…');
    s = apply(s, env({ type: 'assistant_message_completed', payload: { message: message('a', 'assistant', 'live') } }, 9));
    s = seedFleet(s, [snapshotOf({ lastAssistantMessage: preview })], 60);
    expect(card(s).lastMessage?.text).toBe('live');
  });

  it("takes the snapshot's main session over the held one, and keeps the held one when the snapshot has none", () => {
    const held = applyUserSent(EMPTY_FLEET, WS, 'cleared-into', null, 1);
    expect(card(seedFleet(held, [snapshotOf()], 50)).mainSessionId).toBe(MAIN);
    expect(card(seedFleet(held, [snapshotOf({ mainSessionId: null })], 50)).mainSessionId).toBe('cleared-into');
  });
});

describe('card placement', () => {
  const NOW = 1_000_000_000;
  const mk = (id: string, lastActivityAt: number, patch: Partial<FleetCard> = {}): FleetCard => ({
    ...emptyCard(id, id),
    lastActivityAt,
    ...patch,
  });
  const waitingCard = (id: string, lastActivityAt: number, since = lastActivityAt) =>
    mk(id, lastActivityAt, { requests: { 'ask:q': { kind: 'ask', text: '?', agentName: null, at: since, resolvedAt: null } } });
  const ids = (placed: { id: string }[]) => placed.map((p) => p.id);
  const place = (cards: FleetCard[], cols: number, rows: number, selectedId: string | null = null, prevOrder: string[] = [], live = NO_LIVE) =>
    placeCards(cards, { now: NOW, capacity: { cols, rows }, selectedId, prevOrder, live });

  /** What CSS grid `row dense` auto-placement does with the output: rows used. */
  const rowsUsed = (placed: PlacedCard[], cols: number) => {
    const taken: boolean[][] = [];
    const free = (r: number, c: number, w: number, h: number) => {
      if (c + w > cols) return false;
      for (let i = r; i < r + h; i += 1) for (let j = c; j < c + w; j += 1) if (taken[i]?.[j]) return false;
      return true;
    };
    let used = 0;
    for (const p of placed) {
      let r = 0;
      let c = 0;
      while (!free(r, c, p.cols, p.rows)) {
        c += 1;
        if (c >= cols) [r, c] = [r + 1, 0];
      }
      for (let i = r; i < r + p.rows; i += 1) for (let j = c; j < c + p.cols; j += 1) (taken[i] ??= [])[j] = true;
      used = Math.max(used, r + p.rows);
    }
    return used;
  };

  it('derives capacity from the screen size', () => {
    expect(gridCapacity(300, 150)).toEqual({ cols: 1, rows: 1 });
    expect(gridCapacity(1400, 700)).toEqual({ cols: 4, rows: 3 });
    expect(gridCapacity(900, 450)).toEqual({ cols: 2, rows: 2 });
  });

  it('never shows old idle workspaces, but always shows live and selected ones', () => {
    const old = NOW - RECENT_WINDOW_MS - 1;
    const cards = [mk('recent', NOW - 1000), mk('old', old), mk('old-but-running', old), waitingCard('old-waiting', old), mk('old-selected', old)];
    const placed = place(cards, 4, 3, 'old-selected', [], { runs: { 'old-but-running': ['r'] }, tasks: {} });
    expect(ids(placed).sort()).toEqual(['old-but-running', 'old-selected', 'old-waiting', 'recent']);
  });

  it('shows the most recent when more are active than fit', () => {
    const placed = place([mk('a', NOW - 3), mk('b', NOW - 1), mk('c', NOW - 2)], 2, 1);
    expect(ids(placed)).toEqual(['b', 'c']);
  });

  it('puts the selected card first and larger', () => {
    const placed = place([mk('a', NOW - 1), mk('b', NOW - 2), mk('sel', NOW - 50)], 3, 2, 'sel');
    expect(placed).toEqual([
      { id: 'sel', cols: 2, rows: 2 },
      { id: 'a', cols: 1, rows: 1 },
      { id: 'b', cols: 1, rows: 1 },
    ]);
  });

  it('keeps the selected card inside a 3×2 grid when another card is waiting', () => {
    const placed = place([waitingCard('w', NOW - 1), mk('sel', NOW - 2)], 3, 2, 'sel');
    expect(placed).toEqual([
      { id: 'sel', cols: 2, rows: 2 },
      { id: 'w', cols: 1, rows: 1 },
    ]);
    expect(rowsUsed(placed, 3)).toBe(2);
  });

  it('moves waiting cards up (oldest first) and grows them; others keep their order', () => {
    const cards = [mk('a', NOW - 1), mk('b', NOW - 2), waitingCard('w2', NOW - 3, 20), waitingCard('w1', NOW - 4, 10)];
    expect(place(cards, 4, 2, null, ['b', 'a'])).toEqual([
      { id: 'w1', cols: 2, rows: 1 },
      { id: 'w2', cols: 2, rows: 1 },
      { id: 'b', cols: 1, rows: 1 },
      { id: 'a', cols: 1, rows: 1 },
    ]);
  });

  it("doesn't grow a waiting card when that would push a later card out", () => {
    const placed = place([waitingCard('w', NOW - 3), mk('a', NOW - 1), mk('b', NOW - 2)], 3, 1);
    expect(placed).toEqual([
      { id: 'w', cols: 1, rows: 1 },
      { id: 'a', cols: 1, rows: 1 },
      { id: 'b', cols: 1, rows: 1 },
    ]);
  });

  it('never spills below the grid, whatever the mix', () => {
    for (let cols = 1; cols <= 4; cols += 1) {
      for (let rows = 1; rows <= 3; rows += 1) {
        for (let waiting = 0; waiting <= 5; waiting += 1) {
          for (let idle = 0; idle <= 5; idle += 1) {
            for (const selected of [null, 'i0', 'w0']) {
              const cards = [
                ...Array.from({ length: waiting }, (_, i) => waitingCard(`w${i}`, NOW - i - 1)),
                ...Array.from({ length: idle }, (_, i) => mk(`i${i}`, NOW - i - 1)),
              ];
              const sel = cards.some((c) => c.workspaceId === selected) ? selected : null;
              const placed = place(cards, cols, rows, sel);
              expect(rowsUsed(placed, cols)).toBeLessThanOrEqual(rows);
              expect(placed.length).toBe(Math.min(cards.length, cols * rows - (sel ? Math.min(2, cols) * Math.min(2, rows) - 1 : 0)));
              if (sel) expect(placed[0]!.id).toBe(sel);
            }
          }
        }
      }
    }
  });
});
