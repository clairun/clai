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
  FEED_LIMIT,
  FRESH_MS,
  SAID_LIMIT,
  MIN_BUBBLES,
  ROWS,
  STALE_MS,
  RECENT_WINDOW_MS,
  applyAssistantEvent,
  applyRequest,
  applyRequestResolved,
  applyUserSent,
  cardAttention,
  boardShape,
  cardFeed,
  captionText,
  fleetLoudness,
  shoutLevel,
  requestScene,
  firstSentence,
  plainText,
  openRequests,
  splitFeed,
  cardPhase,
  emptyCard,
  liveAgents,
  pickCards,
  seedFleet,
  type FleetCard,
  type FleetLine,
  type FleetLive,
  type FleetPanel,
  type FleetState,
} from './fleetModel';

const WS = 'ws-1';
const MAIN = 'main-session';
const NO_LIVE: FleetLive = { runs: {}, tasks: {} };
const NOW = Date.UTC(2026, 9, 18, 12);

const env = (event: AssistantUiEvent, at: number, sessionId = MAIN, workspaceId = WS): AssistantEventEnvelope =>
  ({ sessionId, runId: null, workspaceId, timestamp: BigInt(at), event }) as AssistantEventEnvelope;

const message = (id: string, role: 'user' | 'assistant', text: string, origin: AssistantMessage['origin'] = { kind: 'user' }, sessionId = MAIN): AssistantMessage =>
  ({ id, sessionId, role, content: [{ type: 'text', text }], createdAt: 0n, providerMetadata: null, origin }) as AssistantMessage;

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
    cycleNo: 1n,
    parentTaskId: null,
    assignedAgentDisplayName: 'Researcher',
    assignedAgentDefinitionId: 'def-r',
    assignedToWorkspaceAgentId: 'wa-r',
    createdAt: BigInt(updatedAt),
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

const MANAGER = { workspaceAgentId: 'wa-main', agentDefinitionId: 'def-main', displayName: 'Manager', isDefault: true };
const WRITER = { workspaceAgentId: 'wa-w', agentDefinitionId: 'def-w', displayName: 'Writer', isDefault: false };
const RESEARCHER = { workspaceAgentId: 'wa-r', agentDefinitionId: 'def-r', displayName: 'Researcher', isDefault: false };
const fromAgent = (agent = MANAGER): AssistantMessage['origin'] => ({ kind: 'agent', agent, sessionId: MAIN, runId: null, replyAddress: null });
const report = (taskId: string, status: WorkspaceTaskResponse['status'], agent = RESEARCHER): AssistantMessage['origin'] =>
  ({ kind: 'task', taskId, agent, title: `Task ${taskId}`, cycleNo: 1n, status }) as AssistantMessage['origin'];

const created = (m: AssistantMessage, at: number) => env({ type: 'message_created', payload: { message: m } } as AssistantUiEvent, at, m.sessionId);
const completed = (m: AssistantMessage, at: number) => env({ type: 'assistant_message_completed', payload: { message: m } } as AssistantUiEvent, at, m.sessionId);

/** A line as `from → to: headline [outcome|live]`, for readable expectations. */
const said = (l: FleetLine) => {
  const name = (s: FleetLine['from']) => (s.kind === 'agent' ? s.agent.name : s.kind === 'you' ? 'You' : 'Main');
  return `${name(l.from)} → ${name(l.to)}: ${l.headline}${l.outcome ? ` [${l.outcome}]` : ''}${l.live ? ` [${l.live}]` : ''}`;
};
const feedOf = (state: FleetState, live = NO_LIVE, now = NOW) => cardFeed(card(state), live, now);
const lines = (state: FleetState, live = NO_LIVE) => feedOf(state, live).lines.map(said);

describe('fleet reducer', () => {
  it('ignores streaming variants without producing a new state', () => {
    const base = apply(EMPTY_FLEET, env({ type: 'run_started', payload: { run: run('r1') } }, 1));
    const streaming: AssistantUiEvent[] = [
      { type: 'assistant_delta', payload: { message_id: 'm', text: 'x' } },
      { type: 'assistant_thinking_delta', payload: { message_id: 'm', text: 'x' } },
      { type: 'assistant_message_updated', payload: { message: message('m', 'assistant', 'x') } },
      { type: 'tool_call_started', payload: { invocation: {} } },
      { type: 'tool_call_completed', payload: { invocation: {} } },
    ] as AssistantUiEvent[];
    for (const event of streaming) {
      expect(applyAssistantEvent(base, env(event, 99))).toBe(base);
    }
  });

  it('keeps every message said, in time order, at most SAID_LIMIT; a deleted one leaves', () => {
    let s = withMain();
    for (let i = 0; i < SAID_LIMIT + 3; i++) s = apply(s, created(message(`u${i}`, 'user', `Message ${i}.`), 100 - i));
    expect(card(s).said).toHaveLength(SAID_LIMIT);
    expect(card(s).said[0]!.id).toBe(`u${SAID_LIMIT - 1}`);
    expect(card(s).said.at(-1)!.id).toBe('u0');
    s = apply(s, env({ type: 'message_deleted', payload: { message_id: 'u0' } } as AssistantUiEvent, 200));
    expect(card(s).said.map((m) => m.id)).not.toContain('u0');
  });

  it('keeps no assistant message of a task session, and nothing of a tool-only message', () => {
    let s = apply(withMain(), completed(message('t1', 'assistant', 'Working on it', { kind: 'user' }, 'task-session-t'), 1));
    s = apply(s, completed(message('a1', 'assistant', ''), 2));
    expect(card(s).said).toEqual([]);
  });

  it('resolves each request by its own resolved event; an answered one leaves', () => {
    let s = applyRequest(EMPTY_FLEET, { kind: 'approval', request: approval('a1') }, 1);
    s = applyRequest(s, { kind: 'approval', request: approval('a2') }, 2);
    s = applyRequest(s, { kind: 'grant', request: approval('a1') }, 3);
    expect(cardAttention(card(s))).toMatchObject({ approvals: 2, grants: 1, total: 3, since: 1 });
    s = applyRequestResolved(s, 'approval', 'a1');
    expect(cardAttention(card(s))).toMatchObject({ approvals: 1, grants: 1, total: 2, since: 2 });
    expect(card(s).requests['approval:a1']).toBeUndefined();
    expect(applyRequestResolved(s, 'approval', 'a1')).toBe(s);
    s = apply(s, askEvent('q1', 4), env({ type: 'ask_user_resolved', payload: { pending_id: 'q1' } } as AssistantUiEvent, 5));
    expect(card(s).requests['ask:q1']).toBeUndefined();
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
    s = applyRequestResolved(applyRequest(s, { kind: 'grant', request: approval('g1') }, 6), 'grant', 'g1');
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

  it('says a request through whoever asked: a subtask\'s agent, a named agent, or Main', () => {
    expect(requestScene(emptyCard(WS), NO_LIVE)).toBeNull();
    const live = withTasks(task('sub', 'running', NOW, { sessionId: 'sub-session', assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w' }));
    let s = apply(EMPTY_FLEET, askEvent('q1', NOW - 120_000));
    expect(requestScene(card(s), live)).toMatchObject({ count: 1, asker: null, caption: { kind: 'waiting', at: NOW - 120_000 } });
    s = apply(s, env({ type: 'ask_user_requested', payload: { pending_id: 'q2', question: 'Which?', options: null, multi_select: false } } as AssistantUiEvent, NOW - 200_000, 'sub-session'));
    expect(requestScene(card(s), live)).toMatchObject({ count: 2, asker: { name: 'Writer', seed: 'def-w' }, request: { text: 'Which?' } });
    const named = applyRequest(EMPTY_FLEET, { kind: 'approval', request: { ...(approval('a1') as object), agentName: 'Committer' } as never }, 5);
    expect(requestScene(card(named), NO_LIVE)?.asker).toEqual({ name: 'Committer', seed: 'Committer' });
  });

});

describe('the highlight feed', () => {
  const live = withTasks(
    task('t', 'completed', NOW - 50_000, { resultSummary: 'Found **3** leaks. Details follow.', completedAt: BigInt(NOW - 20_000) } as never),
    task('r', 'running', NOW - 40_000, { title: 'Draft the post', assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w', assignedToWorkspaceAgentId: 'wa-w' })
  );

  it('says every kind of highlight, in the order it happened', () => {
    const s = apply(
      withMain(),
      created(message('u1', 'user', 'Find the leaks. Then fix them.'), NOW - 60_000),
      created(message('m2t', 'user', 'Also check **staging**.', fromAgent(), 'task-session-t'), NOW - 45_000),
      created(message('w2t', 'user', 'Need the numbers.', fromAgent(WRITER), 'task-session-t'), NOW - 30_000),
      created(message('rep', 'user', 'Found 3 leaks.', report('t', 'completed')), NOW - 19_000),
      completed(message('a1', 'assistant', '## Summary\nThree leaks found.'), NOW - 10_000)
    );
    expect(lines(s, live)).toEqual([
      'You → Main: Find the leaks.',
      'Main → Researcher: Task t',
      'Main → Researcher: Also check staging.',
      'Main → Writer: Draft the post [running]',
      'Writer → Researcher: Need the numbers.',
      'Researcher → Main: Found 3 leaks. [done]',
      'Main → You: Summary',
    ]);
  });

  it('marks a live message cut to fit the card, so the zoom fetches it whole', () => {
    const long = `${'word '.repeat(200)}end`;
    const s = apply(withMain(), created(message('u1', 'user', 'Short.'), NOW - 2), completed(message('a1', 'assistant', long), NOW - 1));
    const [short, cut] = feedOf(s).lines;
    expect(short!.message).toEqual({ sessionId: MAIN, id: 'u1', truncated: false });
    expect(cut!.message).toEqual({ sessionId: MAIN, id: 'a1', truncated: true });
    expect(cut!.markdown.endsWith('…')).toBe(true);
  });

  it("says how a task ended from its status when no report came, and a subtask answers its parent's agent", () => {
    const tasks = withTasks(
      task('p', 'running', NOW - 9_000),
      task('f', 'failed', NOW - 8_000, { parentTaskId: 'p', assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w', error: 'Disk full. Retry later.', completedAt: BigInt(NOW - 5_000) } as never),
      task('x', 'stopped', NOW - 7_000, { completedAt: BigInt(NOW - 6_000) } as never),
      task('q', 'queued', NOW - 4_000)
    );
    expect(lines(withMain(), tasks)).toEqual([
      'Main → Researcher: Task p [running]',
      'Researcher → Writer: Task f',
      'Main → Researcher: Task x',
      'Researcher → Main: Task x [stopped]',
      'Writer → Researcher: Disk full. [failed]',
      'Main → Researcher: Task q [queued]',
    ]);
  });

  it("does not repeat a task's assignment, and keeps no message whose session is neither the main nor a task's", () => {
    const s = apply(
      withMain(),
      created(message('assign', 'user', 'You have been assigned a workspace-local task by a workspace agent.', fromAgent(), 'task-session-t'), NOW - 49_000),
      created(message('elsewhere', 'user', 'Hello', { kind: 'user' }, 'other-session'), NOW - 1)
    );
    expect(lines(s, live)).toEqual(['Main → Researcher: Task t', 'Main → Writer: Draft the post [running]', 'Researcher → Main: Found 3 leaks. [done]']);
  });

  it("keeps a running task's line live until it ends, and never says a tool call", () => {
    const s = apply(withMain(), env({ type: 'tool_call_started', payload: { invocation: {} } } as unknown as AssistantUiEvent, NOW - 1));
    const feed = feedOf(s, live);
    expect(feed.lines.find((l) => l.key === 'task:r')).toMatchObject({ live: 'running', to: { kind: 'agent', agent: { name: 'Writer' } } });
    expect(feed.lines.find((l) => l.key === 'task:t')?.live).toBeNull();
    expect(feed.lines).toHaveLength(3);
  });

  it('holds at most FEED_LIMIT lines, the newest', () => {
    let s = withMain();
    for (let i = 0; i < FEED_LIMIT + 4; i++) s = apply(s, created(message(`u${i}`, 'user', `Line ${i}`), NOW - 1000 + i));
    const shown = feedOf(s).lines;
    expect(shown).toHaveLength(FEED_LIMIT);
    expect(shown.at(-1)!.headline).toBe(`Line ${FEED_LIMIT + 3}`);
  });

  it('is working while a task or Main runs; just finished for FRESH_MS after a result; then idle or stale', () => {
    expect(feedOf(withMain(), live)).toMatchObject({ mood: 'working', caption: { kind: 'working', count: 1 } });
    expect(feedOf(withMain(), { runs: { [WS]: ['main-run'] }, tasks: {} })).toMatchObject({ mood: 'working', caption: { kind: 'working', count: 0 } });
    const ended = withTasks(task('t', 'completed', NOW - 50_000, { completedAt: BigInt(NOW - 20_000) } as never));
    expect(feedOf(withMain(), ended)).toMatchObject({ mood: 'fresh', caption: { kind: 'fresh', at: NOW - 20_000 } });
    expect(cardFeed(card(withMain()), ended, NOW - 20_000 + FRESH_MS)).toMatchObject({ mood: 'idle', caption: { kind: 'idle', at: NOW - 20_000 } });
    expect(cardFeed(card(withMain()), ended, NOW + STALE_MS)).toMatchObject({ mood: 'stale' });
    expect(captionText(feedOf(withMain()).caption, NOW)).toBe('Nothing yet');
  });

  it('changes only with what happened: the same tasks fetched again give the same feed', () => {
    const again = () => withTasks(task('t', 'completed', NOW - 10, { resultSummary: 'Done.' }), task('r', 'running', NOW));
    expect(cardFeed(emptyCard(WS), again(), NOW)).toEqual(cardFeed(emptyCard(WS), again(), NOW));
  });

  it('shouts a beat in full, then at half strength, then not at all, over FRESH_MS', () => {
    expect([0, FRESH_MS / 2 - 1, FRESH_MS / 2, FRESH_MS - 1, FRESH_MS].map((age) => shoutLevel(NOW - age, NOW))).toEqual(['full', 'full', 'half', 'half', null]);
  });

  it('draws loud every unselected card that needs the user, else only the newest good result; only the newest running task rushes in', () => {
    const panel = (id: string, tasks: WorkspaceTaskResponse[], { asking = false, selected = false, now = NOW } = {}): FleetPanel => {
      const c = asking ? { ...card(apply(EMPTY_FLEET, askEvent(`q-${id}`, 1))), workspaceId: id } : emptyCard(id);
      const live = { runs: {}, tasks: { [id]: Object.fromEntries(tasks.map((t) => [t.id, { ...t, workspaceId: id }])) } };
      return { card: c, feed: asking || selected ? null : cardFeed(c, live, now), selected };
    };
    const page = (asking: boolean) => [
      panel('r1', [task('run1', 'running', NOW - 3 * 60_000)]),
      panel('r2', [task('run2', 'running', NOW - 60_000), task('queued', 'queued', NOW)]),
      panel('f1', [task('done1', 'completed', NOW - 2 * 60_000)]),
      panel('f2', [task('done2', 'completed', NOW - 60_000), task('fail', 'failed', NOW - 10)]),
      ...(asking ? [panel('ask', [], { asking: true })] : []),
    ];
    const loud = (panels: FleetPanel[]) => {
      const { cards, rows } = fleetLoudness(panels);
      return { cards, rows: [...rows] };
    };
    expect(loud(page(true))).toEqual({ cards: { ask: 'needs' }, rows: ['task:run2'] });
    expect(loud(page(false))).toEqual({ cards: { f2: 'done' }, rows: ['task:run2', 'ended:done2'] });
    // Every request stays loud; a selected card never is, so the next result cheers.
    expect(loud([panel('a', [], { asking: true }), panel('b', [], { asking: true }), panel('c', [], { asking: true, selected: true })]).cards).toEqual({ a: 'needs', b: 'needs' });
    expect(loud([...page(false).slice(0, 3), { ...page(false)[3]!, feed: null, selected: true }]).cards).toEqual({ f1: 'done' });
    // A failure is not a cheer, and a result stops cheering after FRESH_MS.
    expect(loud([panel('x', [task('f', 'failed', NOW - 60_000)])]).cards).toEqual({});
    expect(loud([panel('x', [task('t', 'completed', NOW - 60_000)], { now: NOW + FRESH_MS })])).toEqual({ cards: {}, rows: [] });
  });

  it('words captions against the clock, so they age by the minute while the scene waits for events', () => {
    const idle = { kind: 'idle', at: NOW } as const;
    expect([0, 59_999, 60_000, 35 * 60_000, 3 * 3_600_000, 25 * 3_600_000, 50 * 3_600_000].map((age) => captionText(idle, NOW + age))).toEqual([
      'Last word · just now',
      'Last word · just now',
      'Last word · 1 min ago',
      'Last word · 35 min ago',
      'Last word · 3 h ago',
      'Last word · yesterday',
      'Last word · 2 days ago',
    ]);
    expect(captionText({ kind: 'waiting', at: NOW }, NOW + 12 * 60_000)).toBe('Waiting on you · 12 min');
    expect(captionText({ kind: 'waiting', at: NOW }, NOW + 10)).toBe('Waiting on you · under a minute');
    expect(captionText({ kind: 'fresh', at: NOW }, NOW + 10)).toBe('Just now');
    expect(captionText({ kind: 'fresh', at: NOW }, NOW + 3 * 60_000)).toBe('Just finished · 3 min ago');
  });
});

describe('splitFeed', () => {
  // Heights oldest first, each with its gap; rows 30 high, 3 for the rule under the last.
  it('draws every highlight as a bubble when they all fit', () => {
    expect(splitFeed([60, 60, 60], 200, 30, 3)).toEqual({ bubbles: 3, rows: 0 });
  });

  it('fills from the newest; up to two older ones collapse to rows and the oldest hide', () => {
    expect(splitFeed([60, 60, 60, 60, 60, 60, 60], 3 * 60 + 2 * 30 + 3, 30, 3)).toEqual({ bubbles: MIN_BUBBLES, rows: ROWS });
    // Room for a row and a half: one row.
    expect(splitFeed([60, 60, 60, 60, 60], 3 * 60 + 45, 30, 3)).toEqual({ bubbles: 3, rows: 1 });
  });

  it('keeps the newest a bubble on a tiny card, and nothing else', () => {
    expect(splitFeed([60, 60, 60, 60], 40, 30, 3)).toEqual({ bubbles: 1, rows: 0 });
    expect(splitFeed([], 40, 30, 3)).toEqual({ bubbles: 0, rows: 0 });
  });

  it('measures each bubble: a tall one leaves room for fewer', () => {
    expect(splitFeed([40, 40, 150, 40], 200, 30, 3)).toEqual({ bubbles: 2, rows: 0 });
    expect(splitFeed([40, 40, 40, 40, 150], 290, 30, 3)).toEqual({ bubbles: 3, rows: 1 });
  });

  it('past three bubbles, keeps room for a row of what came before', () => {
    // A fourth bubble would fit only by leaving no room for a row.
    expect(splitFeed([60, 60, 60, 60, 60], 4 * 60 + 10, 30, 3)).toEqual({ bubbles: 3, rows: 2 });
    // With nothing older, the fourth is a bubble.
    expect(splitFeed([60, 60, 60, 60], 4 * 60, 30, 3)).toEqual({ bubbles: 4, rows: 0 });
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
    lastOpenedAt: 0n,
    mainSessionId: MAIN,
    activeRunIds: [],
    tasks: [],
    pendingPermissions: [],
    pendingPathGrants: [],
    pendingAsks: [],
    lastAssistantMessage: null,
    recentMessages: [],
    ...patch,
  });

  it('takes the open requests of the snapshot, keeping the time of those already held', () => {
    let s = apply(EMPTY_FLEET, askEvent('stale', 2), askEvent('kept', 3));
    s = applyRequest(s, { kind: 'approval', request: approval('gone') }, 4);
    const pendingAsk = (pendingId: string) => ({ pendingId, workspaceId: WS, sessionId: MAIN, runId: 'r', question: '?', options: null, multiSelect: false, extraContext: null, requestedAt: 7n });
    s = seedFleet(s, [snapshotOf({ pendingAsks: [pendingAsk('kept'), pendingAsk('new')], pendingPathGrants: [approval('g1')] })]);
    const c = card(s);
    expect(c.title).toBe('Infra');
    expect(c.mainSessionId).toBe(MAIN);
    expect(Object.keys(c.requests).sort()).toEqual(['ask:kept', 'ask:new', 'grant:g1']);
    expect(c.requests['ask:kept']).toMatchObject({ at: 3 });
    expect(c.requests['ask:new']).toMatchObject({ kind: 'ask', ask: { pendingId: 'new', sessionId: MAIN }, text: '?', at: 7 });
    // A request has no timestamp of its own: it takes the workspace's activity.
    expect(c.requests['grant:g1']).toMatchObject({ at: 10 });
  });

  it('removes cards missing from the snapshot, e.g. deleted workspaces', () => {
    let s = apply(EMPTY_FLEET, askEvent('q', 1), env({ type: 'run_started', payload: { run: run('r') } }, 2, 's', 'deleted'));
    s = seedFleet(s, [snapshotOf()]);
    expect(Object.keys(s.cards)).toEqual([WS]);
  });

  const highlight = (messageId: string, at: number, patch: Partial<FleetWorkspace['recentMessages'][number]> = {}): FleetWorkspace['recentMessages'][number] => ({
    messageId,
    sessionId: MAIN,
    role: 'user',
    origin: { kind: 'user' },
    text: messageId,
    truncated: false,
    createdAt: BigInt(at),
    runId: null,
    ...patch,
  });

  it('seeds the feed from the recent messages and the last reply; an event a seed repeats keeps its own words', () => {
    let s = apply(withMain(), created(message('u1', 'user', 'Live words'), NOW - 50));
    s = seedFleet(s, [
      snapshotOf({
        recentMessages: [
          highlight('u1', NOW - 50, { text: 'Seed words' }),
          highlight('m2t', NOW - 40, { sessionId: 'task-session-t', origin: fromAgent() }),
          highlight('rep', NOW - 30, { origin: report('t', 'failed'), text: 'It broke', truncated: true }),
          highlight('a1', NOW - 20, { role: 'assistant', text: 'All done.' }),
        ],
        lastAssistantMessage: { messageId: 'a1', text: 'All done.', truncated: false, createdAt: BigInt(NOW - 20) },
      }),
    ]);
    expect(lines(s, withTasks(task('t', 'failed', NOW - 100)))).toEqual([
      'Main → Researcher: Task t',
      'You → Main: Live words',
      'Main → Researcher: m2t',
      'Researcher → Main: It broke… [failed]',
      'Main → You: All done.',
    ]);
    expect(card(s).said.filter((m) => m.id === 'a1')).toHaveLength(1);
  });

  it('keeps which message a line says and whether it was cut, so the zoom can fetch it whole', () => {
    const s = seedFleet(EMPTY_FLEET, [
      snapshotOf({
        recentMessages: [highlight('rep', NOW - 30, { origin: report('t', 'failed'), text: 'It **broke**', truncated: true })],
        lastAssistantMessage: { messageId: 'a1', text: 'All done.', truncated: false, createdAt: BigInt(NOW - 20) },
      }),
    ]);
    const feed = feedOf(s, withTasks(task('t', 'failed', NOW - 100)));
    expect(feed.lines.map((l) => [l.key, l.markdown, l.message])).toEqual([
      ['task:t', 'Task t', null],
      ['rep', 'It **broke**…', { sessionId: MAIN, id: 'rep', truncated: true }],
      ['a1', 'All done.', { sessionId: MAIN, id: 'a1', truncated: false }],
    ]);
  });

  it('seeds the last reply alone when the recent messages miss it', () => {
    const preview = { messageId: 'p', text: 'from snapshot', truncated: true, createdAt: 5n };
    const s = seedFleet(EMPTY_FLEET, [snapshotOf({ lastAssistantMessage: preview })]);
    expect(lines(s)).toEqual(['Main → You: from snapshot…']);
  });

  it("takes the snapshot's main session over the held one, and keeps the held one when the snapshot has none", () => {
    const held = applyUserSent(EMPTY_FLEET, WS, 'cleared-into', null, 1);
    expect(card(seedFleet(held, [snapshotOf()])).mainSessionId).toBe(MAIN);
    expect(card(seedFleet(held, [snapshotOf({ mainSessionId: null })])).mainSessionId).toBe('cleared-into');
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
        },
      },
    });
  const pick = (cards: FleetCard[], selectedId: string | null = null, prevOrder: string[] = [], live = NO_LIVE) =>
    pickCards(cards, { now: NOW, selectedId, prevOrder, live });

  it('never shows old idle workspaces, but always shows live and selected ones', () => {
    const old = NOW - RECENT_WINDOW_MS - 1;
    const cards = [mk('recent', NOW - 1000), mk('old', old), mk('old-but-running', old), waitingCard('old-waiting', old), mk('old-selected', old)];
    const picked = pick(cards, 'old-selected', [], { runs: { 'old-but-running': ['r'] }, tasks: {} });
    expect([...picked].sort()).toEqual(['old-but-running', 'old-selected', 'old-waiting', 'recent']);
  });

  it('shows every eligible card: waiting ones first, oldest first, then the most recent', () => {
    expect(pick(Array.from({ length: 12 }, (_, i) => mk(`c${i}`, NOW - i)))).toHaveLength(12);
    expect(pick([mk('a', NOW - 3), waitingCard('w2', NOW - 1), mk('b', NOW - 1), waitingCard('w1', NOW - 9)])).toEqual(['w1', 'w2', 'b', 'a']);
  });

  it('moves nothing on select or deselect; newcomers follow, and a leaving card leaves no gap', () => {
    const cards = [mk('s', NOW - 1), mk('a', NOW - 2), mk('b', NOW - 3)];
    const prev = ['s', 'a', 'b'];
    expect(pick(cards, 'b', prev)).toEqual(prev);
    expect(pick(cards, null, prev)).toEqual(prev);
    expect(pick([mk('a', NOW - 1), mk('b', NOW - 3), mk('new', NOW)], null, prev)).toEqual(['a', 'b', 'new']);
  });

  it('lays four miniatures 2 × 2 beside the stage and six 3 × 2 without one; fewer grow', () => {
    expect([0, 1, 2, 3, 4, 9].map((n) => boardShape(n, true))).toEqual([
      { cols: 1, rows: 1 },
      { cols: 1, rows: 1 },
      { cols: 1, rows: 2 },
      { cols: 2, rows: 2 },
      { cols: 2, rows: 2 },
      { cols: 2, rows: 2 },
    ]);
    expect([1, 2, 3, 4, 5, 6, 10].map((n) => boardShape(n, false))).toEqual([
      { cols: 1, rows: 1 },
      { cols: 2, rows: 1 },
      { cols: 3, rows: 1 },
      { cols: 2, rows: 2 },
      { cols: 3, rows: 2 },
      { cols: 3, rows: 2 },
      { cols: 3, rows: 2 },
    ]);
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
