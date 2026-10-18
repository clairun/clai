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
  EARLIER_LIMIT,
  FRESH_MS,
  STALE_MS,
  TALK_LIMIT,
  RECENT_WINDOW_MS,
  applyAssistantEvent,
  applyRequest,
  applyRequestResolved,
  applyUserSent,
  cardAttention,
  cardScene,
  firstSentence,
  plainText,
  shortTime,
  openRequests,
  cardPhase,
  emptyCard,
  gridCapacity,
  gridShape,
  liveAgents,
  pickCards,
  seedFleet,
  type FleetCard,
  type FleetLive,
  type FleetState,
} from './fleetModel';

const WS = 'ws-1';
const MAIN = 'main-session';
const NO_LIVE: FleetLive = { runs: {}, tasks: {} };
const NOW = Date.UTC(2026, 9, 18, 12);

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
    let s = applyRequest(EMPTY_FLEET, { kind: 'approval', request: approval('a1') }, 1);
    s = applyRequest(s, { kind: 'approval', request: approval('a2') }, 2);
    s = applyRequest(s, { kind: 'grant', request: approval('a1') }, 3);
    expect(cardAttention(card(s))).toMatchObject({ approvals: 2, grants: 1, total: 3, since: 1 });
    s = applyRequestResolved(s, 'approval', 'a1', 4);
    expect(cardAttention(card(s))).toMatchObject({ approvals: 1, grants: 1, total: 2, since: 2 });
    expect(card(s).requests['grant:a1']!.resolvedAt).toBeNull();
    // A duplicate resolve keeps the first time.
    expect(applyRequestResolved(s, 'approval', 'a1', 9)).toBe(s);
  });

  it('keeps what answering needs on each open request, oldest first', () => {
    const ask = env(
      {
        type: 'ask_user_requested',
        payload: { pending_id: 'q1', question: 'Ship?', options: [{ label: 'Yes', description: null }], multi_select: false },
      } as AssistantUiEvent,
      3,
      'task-session'
    );
    let s = applyRequest(apply(EMPTY_FLEET, ask), { kind: 'approval', request: approval('a1') }, 5);
    s = applyRequestResolved(applyRequest(s, { kind: 'grant', request: approval('g1') }, 6), 'grant', 'g1', 7);
    expect(openRequests(card(s))).toMatchObject([
      {
        kind: 'ask',
        ask: { pendingId: 'q1', question: 'Ship?', options: [{ label: 'Yes' }], multiSelect: false, sessionId: 'task-session' },
      },
      { kind: 'approval', request: { requestId: 'a1', command: 'ls' } },
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

  it('turns finished tasks and answered questions into Earlier, newest first; answered approvals leave; open requests stay out', () => {
    const live = withTasks(task('done', 'completed', 1), task('live', 'running', 2));
    let s = apply(EMPTY_FLEET, askEvent('q1', 3), env({ type: 'ask_user_resolved', payload: { pending_id: 'q1' } } as AssistantUiEvent, 4));
    s = applyRequest(s, { kind: 'approval', request: approval('a1') }, 5);
    s = applyRequestResolved(s, 'approval', 'a1', 6);
    s = applyRequestResolved(applyRequest(s, { kind: 'grant', request: approval('g1') }, 7), 'grant', 'g1', 8);
    const scene = cardScene(card(s), live, NOW);
    expect(scene.earlier.map((e) => [e.key, e.outcome, e.agent?.name ?? null])).toEqual([
      ['ask:q1', 'answered', null],
      ['task:done', 'done', 'Researcher'],
    ]);
    expect(scene.running.map((r) => r.key)).toEqual(['task:live']);
  });

  it('draws a live task as one running talk, its subtasks as helpers', () => {
    const live = withTasks(
      task('root', 'completed', 1),
      task('child', 'running', 2, { parentTaskId: 'root', assignedAgentDefinitionId: 'def-w', assignedAgentDisplayName: 'Writer' }),
      task('next', 'queued', 3),
    );
    expect(cardScene(emptyCard(WS), live, NOW).running).toEqual([
      { key: 'task:root', title: 'Task root', agent: { name: 'Researcher', seed: 'def-r' }, queued: false, helpers: 1 },
      { key: 'task:next', title: 'Task next', agent: { name: 'Researcher', seed: 'def-r' }, queued: true, helpers: 0 },
    ]);
  });

  it('flips a just-finished task to its result for FRESH_MS, then moves it to Earlier', () => {
    const at = NOW - 60_000;
    const live = withTasks(task('t', 'completed', at, { resultSummary: 'Shipped **v2** to `main`. Tests pass.' }));
    expect(cardScene(emptyCard(WS), live, NOW)).toMatchObject({
      mood: 'fresh',
      caption: `Just now · ${shortTime(at, NOW)}`,
      finished: [{ asked: 'Task t', result: 'Shipped v2 to main.', failed: false, at }],
      earlier: [],
    });
    const later = cardScene(emptyCard(WS), live, at + FRESH_MS + 1);
    expect(later.finished).toEqual([]);
    expect(later.earlier).toMatchObject([{ key: 'task:t', title: 'Task t', outcome: 'done' }]);
  });

  it('says why a task failed, falling back to its label, and the title when a result is empty', () => {
    const live = withTasks(
      task('a', 'failed', NOW - 3, { error: 'Cargo ran out of disk. Free 2 GB.' }),
      task('b', 'failed', NOW - 2, { failureReason: 'restart' }),
      task('c', 'completed', NOW - 1, { resultSummary: '   ' }),
    );
    expect(cardScene(emptyCard(WS), live, NOW).finished.map((f) => [f.result, f.failed])).toEqual([
      ['Task c', false],
      ['Failed · interrupted', true],
      ['Cargo ran out of disk.', true],
    ]);
  });

  it('draws at most TALK_LIMIT talks, running first; results without room go to Earlier, at most EARLIER_LIMIT', () => {
    const many = Array.from({ length: TALK_LIMIT + 1 }, (_, i) => task(`r${i}`, 'running', NOW));
    const running = cardScene(emptyCard(WS), withTasks(...many), NOW);
    expect(running.running).toHaveLength(TALK_LIMIT);
    expect(running.caption).toBe(`Now · ${TALK_LIMIT + 1} at once`);

    const done = (id: string, ago: number) => task(id, 'completed', NOW - ago);
    const mixed = cardScene(emptyCard(WS), withTasks(task('r', 'running', NOW), done('f1', 1), done('f2', 2), done('f3', 3), done('old', FRESH_MS + 5)), NOW);
    expect(mixed.finished.map((f) => f.key)).toEqual(['task:f1', 'task:f2']);
    expect(mixed.earlier.map((e) => e.key)).toEqual(['task:f3', 'task:old']);
    expect(EARLIER_LIMIT).toBe(2);
  });

  it('keeps Main’s last word, with its first sentence, while the workspace works; a run of no task is Main’s', () => {
    const s = apply(withMain(), env({ type: 'assistant_message_completed', payload: { message: message('a1', 'assistant', '## Done\nAll **green**. Next: deploy.') } }, NOW - 5));
    const scene = cardScene(card(s), { runs: { [WS]: ['main-run'] }, tasks: {} }, NOW);
    expect(scene).toMatchObject({ mood: 'working', caption: 'Now', mainBusy: true, toYou: { text: '## Done\nAll **green**. Next: deploy.', headline: 'Done' } });
    expect(cardScene(card(s), withTasks(task('t', 'running', 1, { runId: 'task-run' } as never)), NOW).mainBusy).toBe(false);
  });

  it('captions an idle card with its last word, and a stale one in days', () => {
    const at = (ago: number) => card(apply(withMain(), env({ type: 'assistant_message_completed', payload: { message: message('a1', 'assistant', 'Hi.') } }, NOW - ago)));
    expect(cardScene(at(60_000), NO_LIVE, NOW)).toMatchObject({ mood: 'idle', caption: `Last word · ${shortTime(NOW - 60_000, NOW)}` });
    expect(cardScene(at(STALE_MS), NO_LIVE, NOW)).toMatchObject({ mood: 'stale', caption: 'Yesterday' });
    expect(cardScene(at(3 * STALE_MS + 5), NO_LIVE, NOW)).toMatchObject({ mood: 'stale', caption: '3 days ago' });
    expect(cardScene(emptyCard(WS), NO_LIVE, NOW)).toMatchObject({ mood: 'idle', caption: 'Nothing yet', toYou: null });
  });

  it('changes only with what happened: the same tasks fetched again give the same scene', () => {
    const live = () => withTasks(task('t', 'completed', NOW - 10, { resultSummary: 'Done.' }), task('r', 'running', NOW));
    expect(cardScene(emptyCard(WS), live(), NOW)).toEqual(cardScene(emptyCard(WS), live(), NOW));
  });
});

describe('firstSentence', () => {
  it.each([
    ['Shipped **v2**. Then more.', 'Shipped v2.'],
    ['\n\n# Heading\nbody', 'Heading'],
    ['- [the report](reports/x.md) is ready', 'the report is ready'],
    ['Version 2.1 is out', 'Version 2.1 is out'],
    ['keep snake_case names', 'keep snake_case names'],
    ['Done!\nMore', 'Done!'],
    [null, ''],
  ])('%j → %j', (input, expected) => {
    expect(firstSentence(input)).toBe(expected);
  });

  it('reads a whole message as one plain paragraph for a clamped bubble', () => {
    expect(plainText('## Done\n\n- **one** [link](x)\n- `two`')).toBe('Done one link two');
  });

  it('cuts a sentence with no end at 200 characters', () => {
    expect(firstSentence('a'.repeat(300))).toBe(`${'a'.repeat(199)}…`);
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
    s = applyRequest(s, { kind: 'approval', request: approval('gone') }, 4);
    const pendingAsk = (pendingId: string) => ({ pendingId, workspaceId: WS, sessionId: MAIN, runId: 'r', question: '?', options: null, multiSelect: false, extraContext: null, requestedAt: 7n });
    s = seedFleet(s, [snapshotOf({ pendingAsks: [pendingAsk('kept'), pendingAsk('new')], pendingPathGrants: [approval('g1')] })], 50);
    const c = card(s);
    expect(c.title).toBe('Infra');
    expect(c.mainSessionId).toBe(MAIN);
    expect(c.requests['ask:stale']!.resolvedAt).toBe(50);
    expect(c.requests['ask:kept']).toMatchObject({ at: 3, resolvedAt: null });
    expect(c.requests['ask:new']).toMatchObject({ kind: 'ask', ask: { pendingId: 'new', sessionId: MAIN }, text: '?', at: 7, resolvedAt: null });
    expect(c.requests['approval:gone']).toBeUndefined();
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

describe('card picking', () => {
  const NOW = 1_000_000_000;
  const mk = (id: string, lastActivityAt: number, patch: Partial<FleetCard> = {}): FleetCard => ({
    ...emptyCard(id, id),
    lastActivityAt,
    ...patch,
  });
  const waitingCard = (id: string, lastActivityAt: number, since = lastActivityAt) =>
    mk(id, lastActivityAt, {
      requests: {
        'ask:q': {
          kind: 'ask',
          ask: { pendingId: 'q', question: '?', options: null, extraContext: null, sessionId: MAIN },
          text: '?',
          agentName: null,
          at: since,
          resolvedAt: null,
        },
      },
    });
  const pick = (cards: FleetCard[], cols: number, rows: number, selectedId: string | null = null, prevOrder: string[] = [], live = NO_LIVE) =>
    pickCards(cards, { now: NOW, capacity: { cols, rows }, selectedId, prevOrder, live });

  it('derives capacity from the screen size', () => {
    expect(gridCapacity(300, 150)).toEqual({ cols: 1, rows: 1 });
    expect(gridCapacity(1400, 700)).toEqual({ cols: 4, rows: 3 });
    expect(gridCapacity(900, 450)).toEqual({ cols: 2, rows: 2 });
  });

  it('sizes the grid by the cards it holds, so a short Fleet still reaches the terminal', () => {
    const fourByThree = { cols: 4, rows: 3 };
    expect(gridShape(fourByThree, 1)).toEqual({ cols: 2, rows: 2 });
    expect(gridShape(fourByThree, 2)).toEqual({ cols: 3, rows: 2 });
    expect(gridShape(fourByThree, 3)).toEqual({ cols: 3, rows: 2 });
    expect(gridShape(fourByThree, 5)).toEqual({ cols: 4, rows: 2 });
    expect(gridShape(fourByThree, 6)).toEqual({ cols: 4, rows: 3 });
    expect(gridShape(fourByThree, 9)).toEqual({ cols: 4, rows: 3 });
    expect(gridShape({ cols: 2, rows: 3 }, 2)).toEqual({ cols: 2, rows: 3 });
    expect(gridShape({ cols: 1, rows: 1 }, 1)).toEqual({ cols: 1, rows: 1 });
  });

  it('never shows old idle workspaces, but always shows live and selected ones', () => {
    const old = NOW - RECENT_WINDOW_MS - 1;
    const cards = [mk('recent', NOW - 1000), mk('old', old), mk('old-but-running', old), waitingCard('old-waiting', old), mk('old-selected', old)];
    const picked = pick(cards, 4, 3, 'old-selected', [], { runs: { 'old-but-running': ['r'] }, tasks: {} });
    expect([...picked].sort()).toEqual(['old-but-running', 'old-selected', 'old-waiting', 'recent']);
  });

  it('counts the stage as four cells and shows the most recent when more are active than fit, but the selected and waiting ones first', () => {
    const many = Array.from({ length: 12 }, (_, i) => mk(`c${i}`, NOW - i));
    expect(pick(many, 4, 3)).toHaveLength(9);
    expect(pick(many, 2, 2)).toEqual(['c0']);
    expect(pick([mk('a', NOW - 3), mk('b', NOW - 1), mk('c', NOW - 2)], 3, 1)).toEqual(['b', 'c']);
    expect(pick([mk('a', NOW - 3), waitingCard('w', NOW - 9), mk('b', NOW - 1), mk('sel', NOW - 8)], 4, 1, 'sel')).toEqual(['sel', 'b', 'w']);
  });

  it('puts the most recent card on the stage at first, unselected, and the rest in rank order', () => {
    expect(pick([waitingCard('w', NOW - 9), mk('a', NOW - 2), mk('b', NOW - 1)], 4, 3)).toEqual(['b', 'w', 'a']);
  });

  const cards = [mk('s', NOW - 1), mk('a', NOW - 2), mk('b', NOW - 3), mk('c', NOW - 4), mk('d', NOW - 5)];
  const prev = ['s', 'a', 'b', 'c', 'd'];

  it('swaps the selected card with the stage card, and every other card keeps its slot', () => {
    expect(pick(cards, 4, 3, 'c', prev)).toEqual(['c', 'a', 'b', 's', 'd']);
    // From there, selecting another swaps only those two again.
    expect(pick(cards, 4, 3, 'a', ['c', 'a', 'b', 's', 'd'])).toEqual(['a', 'c', 'b', 's', 'd']);
  });

  it('moves nothing when the selected card is already on the stage, or on deselect', () => {
    expect(pick(cards, 4, 3, 's', prev)).toEqual(prev);
    const afterSwap = ['c', 'a', 'b', 's', 'd'];
    expect(pick(cards, 4, 3, null, afterSwap)).toEqual(afterSwap);
  });

  it('gives the stage to the most recent remaining card when the stage card leaves; only that card moves', () => {
    const rest = cards.filter((c) => c.workspaceId !== 's');
    expect(pick(rest, 4, 3, null, prev)).toEqual(['a', 'b', 'c', 'd']);
    expect(pick([mk('a', NOW - 3), mk('c', NOW - 4), mk('b', NOW)], 4, 3, null, ['s', 'a', 'c', 'b'])).toEqual(['b', 'a', 'c']);
  });

  it('gives newcomers the next slots, never the stage, and closes the gap a leaving card left', () => {
    expect(pick([mk('a', NOW - 1), mk('c', NOW - 3), mk('new', NOW)], 4, 3, null, ['a', 'b', 'c'])).toEqual(['a', 'c', 'new']);
  });
});

describe('live agents', () => {
  const card = emptyCard(WS, 'W');
  const agent = (n: string) => ({
    assignedAgentDisplayName: `Agent ${n}`,
    assignedAgentDefinitionId: `def-${n}`,
  });
  const liveWith = (tasks: WorkspaceTaskResponse[], runs: string[] = []): FleetLive => ({
    runs: { [WS]: runs },
    tasks: { [WS]: Object.fromEntries(tasks.map((t) => [t.id, t])) },
  });

  it('lists each agent once, running before queued', () => {
    const agents = liveAgents(
      card,
      liveWith([
        task('q1', 'queued', 1, agent('a')),
        task('q2', 'queued', 2, agent('a')),
        task('r1', 'running', 3, agent('b')),
        task('r2', 'running', 4, { ...agent('b'), title: 'Second' }),
        task('d1', 'completed', 5, agent('c')),
      ])
    );
    expect(agents.map((a) => [a.seed, a.running, a.doing])).toEqual([
      ['def-b', true, 'running: 2 tasks'],
      ['def-a', false, 'queued: 2 tasks'],
    ]);
  });

  it("adds Main when a run belongs to no task, but not for a task's own run", () => {
    const tasks = [task('r1', 'running', 1, { ...agent('b'), runId: 'run-task' })];
    expect(liveAgents(card, liveWith(tasks, ['run-task'])).map((a) => a.seed)).toEqual(['def-b']);
    expect(liveAgents(card, liveWith(tasks, ['run-task', 'run-main'])).map((a) => a.name)).toEqual([
      'Main',
      'Agent b',
    ]);
  });

  it('is empty when nothing is live', () => {
    expect(liveAgents(card, liveWith([task('d1', 'completed', 1)]))).toEqual([]);
    expect(liveAgents(card, NO_LIVE)).toEqual([]);
  });
});
