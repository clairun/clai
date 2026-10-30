import { describe, expect, it } from 'vitest';
import type {
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantRun,
  AssistantUiEvent,
  FleetHighlight,
  FleetWorkspace,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import {
  EMPTY_FLEET,
  SAID_LIMIT,
  RECENT_WINDOW_MS,
  applyAssistantEvent,
  applyOpened,
  applyRequest,
  applyRequestResolved,
  applyUserSent,
  applyWholeMessage,
  cardAttention,
  openRequests,
  cardPhase,
  emptyCard,
  cardAgents,
  fullMessageText,
  seedFleet,
  type FleetCard,
  type FleetLive,
  type FleetState,
} from './fleetModel';
import {
  FEED_LIMIT,
  FRESH_MS,
  MIN_BUBBLES,
  ROWS,
  STALE_MS,
  cardFeed,
  captionText,
  firstSentence,
  plainText,
  splitFeed,
  type FleetLine,
} from './fleetFeed';
import {
  fleetLoudness,
  shoutLevel,
  requestScene,
  MAX_CREW,
  cardCrew,
  firstWords,
  type FleetPanel,
} from './fleetCrew';
import { boardShape, eligibleCards, recentlyOpened } from './boardSlots';

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

const text = (t: string) => ({ type: 'text', text: t }) as AssistantMessage['content'][number];
const tool = (id: string) => ({ type: 'tool_use', tool_call_id: id, tool_name: 'bash_exec', arguments: {} }) as AssistantMessage['content'][number];
const withParts = (id: string, ...content: AssistantMessage['content']): AssistantMessage => ({ ...message(id, 'assistant', ''), content });
/** Main's message as heard mid-run, at a tool call. */
const updated = (m: AssistantMessage, at: number, runId: string | null = 'r1') =>
  ({ ...env({ type: 'assistant_message_updated', payload: { message: m } } as AssistantUiEvent, at, m.sessionId), runId }) as AssistantEventEnvelope;
const created = (m: AssistantMessage, at: number) => env({ type: 'message_created', payload: { message: m } } as AssistantUiEvent, at, m.sessionId);
const completed = (m: AssistantMessage, at: number) => env({ type: 'assistant_message_completed', payload: { message: m } } as AssistantUiEvent, at, m.sessionId);

/** A line as `from → to: headline [outcome|live]`, for readable expectations. */
const said = (l: FleetLine) => {
  const name = (s: FleetLine['from']) => (s.kind === 'agent' ? s.agent.name : s.kind === 'you' ? 'You' : 'Main');
  return `${name(l.from)} → ${name(l.to)}: ${l.headline}${l.outcome ? ` [${l.outcome}]` : ''}${l.live ? ` [${l.live}]` : ''}`;
};
const feedOf = (state: FleetState, live = NO_LIVE, now = NOW) => cardFeed(card(state), live, now);
const lines = (state: FleetState, live = NO_LIVE) => feedOf(state, live).lines.map(said);

describe('main run failures', () => {
  it('retains a failed send for the card and clears it on the next run', () => {
    const failed = { ...run('r1'), status: 'failed' as const, startedAt: BigInt(NOW - 2000), completedAt: BigInt(NOW - 1000), error: 'Usage limit reached. Try again tomorrow.' };
    const user = message('u1', 'user', 'Please continue');
    const sent = applyUserSent(EMPTY_FLEET, WS, MAIN, user, NOW - 2000);
    const state = apply(sent, env({ type: 'run_failed', payload: { run: failed } }, NOW - 1000));
    expect(card(state).lastMainRun).toEqual(failed);
    expect(lines(state)).toEqual(['You → Main: Please continue']);
    const next = apply(state, env({ type: 'run_started', payload: { run: { ...run('r2'), startedAt: BigInt(NOW) } } }, NOW));
    expect(card(next).lastMainRun?.status).toBe('running');
    expect(lines(next)).toEqual(['You → Main: Please continue']);
  });

  it("keeps a member's run off the card when its run marker arrives before Main is known", () => {
    // A scheduled or manual automation run posts its marker into the member's own session as the user.
    const marker = message('mk', 'user', 'New scheduled run', { kind: 'user' }, 'member');
    const failed = { ...run('r1'), sessionId: 'member', status: 'failed' as const, error: 'Usage limit reached' };
    const state = apply(EMPTY_FLEET, created(marker, NOW - 1000), env({ type: 'run_failed', payload: { run: failed } }, NOW, 'member'));
    expect(card(state).mainSessionId).toBeNull();
    expect(card(state).lastMainRun).toBeNull();
    const seeded = seedFleet(state, [snapshotOf({ mainSessionId: MAIN, lastMainRun: null })]);
    expect(card(seeded).mainSessionId).toBe(MAIN);
    expect(card(seeded).lastMainRun).toBeNull();
  });

  it('forgets the old Main run when the user moves to a new Main session', () => {
    const failed = { ...run('r1'), status: 'failed' as const, error: 'Rate limit reached' };
    const state = apply(withMain(), env({ type: 'run_failed', payload: { run: failed } }, NOW));
    expect(card(applyUserSent(state, WS, MAIN, null, NOW + 1000)).lastMainRun).toEqual(failed);
    expect(card(applyUserSent(state, WS, 'new-main', null, NOW + 1000)).lastMainRun).toBeNull();
  });

  it('ignores task failures and records Main cancellation without an error', () => {
    const base = withMain();
    const other = apply(base, env({ type: 'run_failed', payload: { run: { ...run('task'), status: 'failed', error: 'Task failed' } } }, NOW, 'task-session'));
    const cancelled = apply(other, env({ type: 'run_cancelled', payload: { run: { ...run('r1'), status: 'cancelled', error: null } } }, NOW));
    expect(card(other).lastMainRun).toBeNull();
    expect(card(cancelled).lastMainRun?.status).toBe('cancelled');
    expect(lines(cancelled)).toEqual([]);
  });
});

describe('fleet reducer', () => {
  it('ignores deltas and tool calls, and an update with no new text, without producing a new state', () => {
    const base = apply(withMain(), env({ type: 'run_started', payload: { run: run('r1') } }, 1), updated(message('m', 'assistant', 'x'), 2));
    const streaming: AssistantUiEvent[] = [
      { type: 'assistant_delta', payload: { message_id: 'm', text: 'x' } },
      { type: 'assistant_thinking_delta', payload: { message_id: 'm', text: 'x' } },
      { type: 'assistant_message_updated', payload: { message: withParts('m', text('x'), tool('t1')) } },
      { type: 'assistant_message_updated', payload: { message: withParts('m2', tool('t2')) } },
      { type: 'tool_call_started', payload: { invocation: {} } },
      { type: 'tool_call_completed', payload: { invocation: {} } },
    ] as AssistantUiEvent[];
    for (const event of streaming) {
      expect(applyAssistantEvent(base, env(event, 99))).toBe(base);
    }
    // Nor does an update start a card for a workspace that has none.
    expect(applyAssistantEvent(base, env(streaming[3]!, 99, MAIN, 'elsewhere'))).toBe(base);
    // A task's words reach Main as its report.
    expect(applyAssistantEvent(base, updated(message('t', 'assistant', 'Working', { kind: 'user' }, 'task-session-t'), 99))).toBe(base);
  });

  it("moves the card's activity time on every kind of news, which brings a dismissed card back", () => {
    const news: ((s: FleetState, at: number) => FleetState)[] = [
      (s, at) => apply(s, created(message('a', 'user', 'From an agent', fromAgent(RESEARCHER)), at)),
      (s, at) => apply(s, completed(message('m', 'assistant', 'From Main'), at)),
      (s, at) => apply(s, env({ type: 'task_updated', payload: { task: task('t', 'running', at) } } as AssistantUiEvent, at)),
      (s, at) => apply(s, env({ type: 'run_started', payload: { run: run('r') } }, at, 'task-session-t')),
      (s, at) => apply(s, askEvent('q', at)),
      (s, at) => applyRequest(s, { kind: 'approval', request: approval('p') }, at),
      (s, at) => applyRequest(s, { kind: 'grant', request: { ...(approval('g') as object), requestedPath: '/x', requestedAccess: 'read_only' } as never }, at),
      (s, at) => applyUserSent(s, WS, MAIN, message('u', 'user', 'From you'), at),
    ];
    news.reduce((s, hear, i) => {
      const next = hear(s, 10 + i);
      expect(card(next).lastActivityAt).toBe(10 + i);
      return next;
    }, withMain());
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
    expect(cardAttention(card(s))).toMatchObject({ approvals: 2, grants: 1, total: 3 });
    s = applyRequestResolved(s, 'approval', 'a1');
    expect(cardAttention(card(s))).toMatchObject({ approvals: 1, grants: 1, total: 2 });
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

  it('says a request through whoever asked: a subtask\'s agent, a roster agent by name, or Main', () => {
    expect(requestScene(emptyCard(WS), NO_LIVE)).toBeNull();
    const live = withTasks(task('sub', 'running', NOW, { sessionId: 'sub-session', assignedAgentDisplayName: 'Writer', assignedAgentDefinitionId: 'def-w' }));
    let s = apply(EMPTY_FLEET, askEvent('q1', NOW - 120_000));
    expect(requestScene(card(s), live)).toMatchObject({ count: 1, asker: null, caption: { kind: 'waiting', at: NOW - 120_000 } });
    s = apply(s, env({ type: 'ask_user_requested', payload: { pending_id: 'q2', question: 'Which?', options: null, multi_select: false } } as AssistantUiEvent, NOW - 200_000, 'sub-session'));
    expect(requestScene(card(s), live)).toMatchObject({ count: 2, asker: { name: 'Writer', seed: 'def-w' }, request: { text: 'Which?' } });
    const roster = seedFleet(EMPTY_FLEET, [{ ...snapshotOf(), agents: [MANAGER, WRITER] }]);
    const by = (agentName: string) =>
      requestScene(card(applyRequest(roster, { kind: 'approval', request: { ...(approval('a1') as object), agentName } as never }, 5)), NO_LIVE)?.asker;
    // The same face as the crew strip draws for it.
    expect(by('Writer')).toEqual({ name: 'Writer', seed: 'def-w' });
    expect(by('Manager')).toBeNull();
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

  it("never says an automation's run marker, live or seeded", () => {
    const marker = '--- New scheduled run at 2026-10-18 12:00 ---\nRun the next scheduled pass.';
    let s = apply(withMain(), created(message('mark', 'user', marker), NOW - 30), created(message('u1', 'user', 'Real words.'), NOW - 20));
    s = seedFleet(s, [
      {
        workspaceId: WS,
        title: 'W',
        lastActivityAt: BigInt(NOW),
        lastOpenedAt: 0n,
        agents: [],
        mainSessionId: MAIN,
        lastMainRun: null,
        activeRunIds: [],
        tasks: [],
        pendingPermissions: [],
        pendingPathGrants: [],
        pendingAsks: [],
        lastAssistantMessage: null,
        recentMessages: [
          { message: { ...message('seed-mark', 'user', '--- Manual run at 12:01 ---'), createdAt: BigInt(NOW - 10) }, runId: null, truncated: false },
        ],
      },
    ]);
    expect(lines(s)).toEqual(['You → Main: Real words.']);
    expect(card(s).said.map((m) => m.id)).toEqual(['u1']);
  });

  it('keeps a live message whole, yours and Main\'s alike, without its tool calls', () => {
    const long = `${'word '.repeat(200)}end`;
    const s = apply(withMain(), created(message('u1', 'user', long), NOW - 2), completed(withParts('a1', text(long), tool('c1')), NOW - 1));
    expect(feedOf(s).lines.map((l) => [l.markdown, l.cut])).toEqual([
      [long, []],
      [long, []],
    ]);
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

describe("Main's bubbles", () => {
  const T = NOW - 60_000;
  /** Main's message `m1` in run `r1`: a block of text, a tool call, and another block. */
  const first = withParts('m1', text('Before.'), tool('c1'));
  const both = withParts('m1', text('Before.'), tool('c1'), text('After.'), tool('c2'));
  const bubbles = (state: FleetState, live = NO_LIVE) => feedOf(state, live).lines.filter((l) => l.from.kind === 'main' && l.to.kind === 'you');

  it('joins adjacent text parts without inventing a paragraph break', () => {
    const assistant = withParts('m', text('Hello'), text(' world'), tool('c1'), text('Next.'));
    expect(fullMessageText(assistant)).toBe('Hello world\n\nNext.');
    expect(fullMessageText({ ...assistant, role: 'user', content: [text('Hello'), text(' world')] })).toBe('Hello world');
  });

  it('adds each block of text as it closes, at a tool call, into the same bubble', () => {
    let s = apply(withMain(), updated(first, T));
    expect(bubbles(s).map((l) => l.markdown)).toEqual(['Before.']);
    s = apply(s, updated(both, T + 10));
    const [bubble] = bubbles(s);
    expect(bubbles(s)).toHaveLength(1);
    expect(bubble).toMatchObject({ key: 'm1', markdown: 'Before.\n\nAfter.', at: T + 10, blocks: [{ at: T, start: 0, end: 15 }] });
  });

  it('says nothing twice: completing a message adds only the text not yet heard', () => {
    let s = apply(withMain(), updated(first, T), updated(both, T + 10));
    const heard = s;
    s = apply(s, completed(both, T + 20));
    expect(card(s).said).toEqual(card(heard).said);
    s = apply(s, completed(withParts('m1', text('Before.'), tool('c1'), text('After.'), tool('c2'), text('Done.')), T + 30));
    expect(bubbles(s).map((l) => l.markdown)).toEqual(['Before.\n\nAfter.\n\nDone.']);
  });

  it('joins the blocks of one run across messages, but never two runs', () => {
    const next = { ...completed(message('m2', 'assistant', 'Second message.'), T + 10), runId: 'r1' } as AssistantEventEnvelope;
    let s = apply(withMain(), { ...completed(message('m1', 'assistant', 'First message.'), T), runId: 'r1' } as AssistantEventEnvelope, next);
    expect(bubbles(s).map((l) => l.markdown)).toEqual(['First message.\n\nSecond message.']);
    s = apply(s, { ...completed(message('m3', 'assistant', 'Next run.'), T + 20), runId: 'r2' } as AssistantEventEnvelope);
    expect(bubbles(s).map((l) => l.markdown)).toEqual(['First message.\n\nSecond message.', 'Next run.']);
  });

  const between = T + 5;
  it.each<[string, (s: FleetState) => FleetState, FleetLive, string]>([
    ['a message from you', (s) => apply(s, created(message('u', 'user', 'Wait.'), between)), NO_LIVE, 'You → Main: Wait.'],
    ['a task handed over', (s) => s, withTasks(task('t', 'running', between)), 'Main → Researcher: Task t [running]'],
    ["a task's report", (s) => apply(s, created(message('rep', 'user', 'Found it.', report('t', 'completed')), between)), NO_LIVE, 'Researcher → Main: Found it. [done]'],
    ['a message to an agent', (s) => apply(s, created(message('m2t', 'user', 'Also staging.', fromAgent(), 'task-session-t'), between)), withTasks(task('t', 'running', T - 1)), 'Main → Researcher: Also staging.'],
    ['a message between agents', (s) => apply(s, created(message('w2t', 'user', 'Numbers?', fromAgent(WRITER), 'task-session-t'), between)), withTasks(task('t', 'running', T - 1)), 'Writer → Researcher: Numbers?'],
  ])('closes the bubble at %s: later words start a new one after it', (_, say, live, line) => {
    const s = apply(say(apply(withMain(), updated(first, T))), updated(both, T + 10));
    expect(lines(s, live).slice(-3)).toEqual([
      'Main → You: Before.',
      line,
      'Main → You: After.',
    ]);
    // The new bubble is keyed by its own first block; the closed one keeps its key.
    expect(bubbles(s, live).map((l) => l.key)).toEqual(['m1', 'm1:1']);
  });

  it('closes the bubble at a question, answered or not, which says nothing itself', () => {
    const s = apply(withMain(), updated(first, T), askEvent('q1', between), env({ type: 'ask_user_resolved', payload: { pending_id: 'q1' } } as AssistantUiEvent, between + 1), updated(both, T + 10));
    expect(lines(s)).toEqual(['Main → You: Before.', 'Main → You: After.']);
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

const snapshotOf = (patch: Partial<FleetWorkspace> = {}): FleetWorkspace => ({
  workspaceId: WS,
  title: 'Infra',
  lastActivityAt: 10n,
  lastOpenedAt: 0n,
  agents: [],
  mainSessionId: MAIN,
  lastMainRun: null,
  activeRunIds: [],
  tasks: [],
  pendingPermissions: [],
  pendingPathGrants: [],
  pendingAsks: [],
  lastAssistantMessage: null,
  recentMessages: [],
  ...patch,
});

describe('snapshot', () => {
  it('restores the latest Main failure after Fleet reloads', () => {
    const failed = { ...run('r1'), status: 'failed' as const, startedAt: BigInt(NOW - 1000), completedAt: BigInt(NOW), error: 'Quota exceeded' };
    const seeded = seedFleet(EMPTY_FLEET, [snapshotOf({ lastMainRun: failed })]);
    expect(card(seeded).lastMainRun).toEqual(failed);
    const completed = { ...run('r2'), status: 'completed' as const, startedAt: BigInt(NOW + 1000), completedAt: BigInt(NOW + 2000) };
    expect(card(seedFleet(seeded, [snapshotOf({ lastMainRun: completed })])).lastMainRun).toEqual(completed);
    expect(card(seedFleet(seeded, [snapshotOf({ mainSessionId: 'new-main', lastMainRun: null })])).lastMainRun).toBeNull();
  });
  it('takes the open requests of the snapshot, keeping the time of those already held', () => {
    let s = apply(EMPTY_FLEET, askEvent('stale', 2), askEvent('kept', 3));
    s = applyRequest(s, { kind: 'approval', request: approval('gone') }, 4);
    const pendingAsk = (pendingId: string) => ({ pendingId, workspaceId: WS, sessionId: MAIN, question: '?', options: null, multiSelect: false, extraContext: null, requestedAt: 7n });
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

  const highlight = (m: AssistantMessage, at: number, runId: string | null = null, truncated = false): FleetHighlight => ({
    message: { ...m, createdAt: BigInt(at) },
    runId,
    truncated,
  });
  const broke = `It **broke**${' and broke'.repeat(60)}`;

  it('seeds the feed from the recent messages and the last reply; an event a seed repeats keeps its own words', () => {
    let s = apply(withMain(), created(message('u1', 'user', 'Live words'), NOW - 50));
    s = seedFleet(s, [
      snapshotOf({
        recentMessages: [
          highlight(message('u1', 'user', 'Seed words'), NOW - 50),
          highlight(message('m2t', 'user', 'm2t', fromAgent(), 'task-session-t'), NOW - 40),
          highlight(message('rep', 'user', 'It broke.', report('t', 'failed')), NOW - 30),
          highlight(message('a1', 'assistant', 'All done.'), NOW - 20),
        ],
        lastAssistantMessage: highlight(message('a1', 'assistant', 'All done.'), NOW - 20),
      }),
    ]);
    expect(lines(s, withTasks(task('t', 'failed', NOW - 100)))).toEqual([
      'Main → Researcher: Task t',
      'You → Main: Live words',
      'Main → Researcher: m2t',
      'Researcher → Main: It broke. [failed]',
      'Main → You: All done.',
    ]);
    expect(card(s).said.filter((m) => m.id === 'a1')).toHaveLength(1);
  });

  it('continues a tool-separated reply after a reload at the new update time', () => {
    const started = NOW - 1000;
    const seeded = seedFleet(EMPTY_FLEET, [snapshotOf({
      recentMessages: [highlight(withParts('m', text('Before.'), tool('c1'), text('After.')), started, 'r1')],
    })]);
    const continued = apply(seeded, updated(withParts('m', text('Before.'), tool('c1'), text('After.'), tool('c2'), text('More.')), NOW));
    expect(card(continued).said[0]!.marks).toEqual([{ at: started, end: 'Before.\n\nAfter.'.length }, { at: NOW, end: 'Before.\n\nAfter.\n\nMore.'.length }]);
    expect(feedOf(continued).lines.map((line) => [line.markdown, line.at])).toEqual([['Before.\n\nAfter.\n\nMore.', NOW]]);
  });

  it('lists each message a bubble joins that the snapshot cut, so each is fetched whole', () => {
    const s = seedFleet(EMPTY_FLEET, [
      snapshotOf({
        recentMessages: [
          highlight(message('a1', 'assistant', 'First of the'), NOW - 30, 'r1', true),
          highlight(message('a2', 'assistant', 'Whole.'), NOW - 25, 'r1'),
          highlight(message('a3', 'assistant', 'Then the'), NOW - 20, 'r1', true),
        ],
      }),
    ]);
    expect(feedOf(s).lines.map((l) => [l.markdown, l.cut])).toEqual([
      ['First of the…\n\nWhole.\n\nThen the…', [{ sessionId: MAIN, id: 'a1' }, { sessionId: MAIN, id: 'a3' }]],
    ]);
  });

  it('keeps which message a line says and whether the snapshot cut it, so it can be fetched whole', () => {
    const s = seedFleet(EMPTY_FLEET, [
      snapshotOf({
        recentMessages: [
          highlight(message('rep', 'user', broke, report('t', 'failed')), NOW - 30),
          // The snapshot cut these two short of the stored message.
          highlight(message('u2', 'user', 'Ship it'), NOW - 25, null, true),
          highlight(message('a0', 'assistant', 'Shipped to stag'), NOW - 22, null, true),
        ],
        lastAssistantMessage: highlight(message('a1', 'assistant', 'All done.'), NOW - 20),
      }),
    ]);
    const feed = feedOf(s, withTasks(task('t', 'failed', NOW - 100)));
    expect(feed.lines.map((l) => [l.key, l.markdown, l.cut])).toEqual([
      ['task:t', 'Task t', []],
      ['rep', broke, []],
      ['u2', 'Ship it…', [{ sessionId: MAIN, id: 'u2' }]],
      ['a0', 'Shipped to stag…', [{ sessionId: MAIN, id: 'a0' }]],
      ['a1', 'All done.', []],
    ]);
  });

  it('takes a message the snapshot cut whole once fetched, where and when it was said; nothing else', () => {
    const s = seedFleet(EMPTY_FLEET, [
      snapshotOf({
        recentMessages: [
          highlight(message('u1', 'user', 'Ship'), NOW - 30, null, true),
          highlight(withParts('a1', text('All do')), NOW - 20, 'r1', true),
        ],
      }),
    ]);
    const whole = applyWholeMessage(
      applyWholeMessage(s, WS, { ...message('u1', 'user', 'Ship it.'), createdAt: BigInt(NOW - 30) }),
      WS,
      { ...withParts('a1', text('All done.'), tool('c1'), text('Shipped.')), createdAt: BigInt(NOW - 20) }
    );
    expect(feedOf(whole).lines.map((l) => [l.markdown, l.at, l.cut])).toEqual([
      ['Ship it.', NOW - 30, []],
      ['All done.\n\nShipped.', NOW - 20, []],
    ]);
    expect(card(whole).said.map((m) => m.runId)).toEqual([null, 'r1']);
    // Already whole, or not on the card: nothing changes.
    expect(applyWholeMessage(whole, WS, message('u1', 'user', 'Other words.'))).toBe(whole);
    expect(applyWholeMessage(whole, 'elsewhere', message('u1', 'user', 'Other words.'))).toBe(whole);
  });

  it('seeds the last reply alone when the recent messages miss it', () => {
    const last = highlight(message('p', 'assistant', 'from snapshot'), 5);
    const s = seedFleet(EMPTY_FLEET, [snapshotOf({ lastAssistantMessage: last })]);
    expect(lines(s)).toEqual(['Main → You: from snapshot']);
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
  const pick = (cards: FleetCard[], selectedId: string | null = null, live = NO_LIVE) =>
    eligibleCards(cards, { now: NOW, selectedId, live });

  it('never shows old idle workspaces, but always shows live and selected ones', () => {
    const old = NOW - RECENT_WINDOW_MS - 1;
    const cards = [mk('recent', NOW - 1000), mk('old', old), mk('old-but-running', old), waitingCard('old-waiting', old), mk('old-selected', old)];
    const picked = pick(cards, 'old-selected', { runs: { 'old-but-running': ['r'] }, tasks: {} });
    expect([...picked].sort()).toEqual(['old-but-running', 'old-selected', 'old-waiting', 'recent']);
  });

  it('shows every eligible card', () => {
    expect(pick(Array.from({ length: 12 }, (_, i) => mk(`c${i}`, NOW - i)))).toHaveLength(12);
  });

  it('lays at most six cards 3 × 2; fewer grow, but never past half the width', () => {
    expect([1, 2, 3, 4, 5, 6, 10].map(boardShape)).toEqual([
      { cols: 2, rows: 1 },
      { cols: 2, rows: 1 },
      { cols: 3, rows: 1 },
      { cols: 2, rows: 2 },
      { cols: 3, rows: 2 },
      { cols: 3, rows: 2 },
      { cols: 3, rows: 2 },
    ]);
  });

  it('switches most recently opened first, the never opened by activity; opening one moves it first', () => {
    const cards = Object.fromEntries(
      [mk('busy', NOW - 1), mk('old', NOW - 9, { lastOpenedAt: NOW - 50 }), mk('last', NOW - 8, { lastOpenedAt: NOW - 5 }), mk('quiet', NOW - 7)].map(
        (c) => [c.workspaceId, c]
      )
    );
    const ids = ['busy', 'old', 'last', 'quiet'];
    expect(recentlyOpened(ids, cards)).toEqual(['last', 'old', 'busy', 'quiet']);
    const opened = applyOpened({ cards }, 'quiet', NOW);
    expect(recentlyOpened(ids, opened.cards)).toEqual(['quiet', 'last', 'old', 'busy']);
    // Agent activity alone reorders nothing among opened cards.
    expect(recentlyOpened(ids, { ...cards, old: { ...cards.old!, lastActivityAt: NOW } })).toEqual(['last', 'old', 'busy', 'quiet']);
    expect(applyOpened({ cards }, 'unknown', NOW).cards).toBe(cards);
  });
});

describe('the crew strip', () => {
  const crewSeed = (agents = [WRITER, RESEARCHER]) => seedFleet(EMPTY_FLEET, [{ ...snapshotOf(), agents: [MANAGER, ...agents] }]);
  const crewCard = (agents = [WRITER, RESEARCHER]) => card(crewSeed(agents));
  const live = (tasks: WorkspaceTaskResponse[], runs: string[] = []): FleetLive => ({
    runs: { [WS]: runs },
    tasks: { [WS]: Object.fromEntries(tasks.map((t) => [t.id, t])) },
  });
  const strip = (c: FleetCard, l: FleetLive = NO_LIVE, now = NOW) =>
    cardCrew(c, l, now).shown.map((m) => `${m.name}:${m.status}${m.words ? ` ${m.words}` : ''}${m.level ? ` ${m.level}` : ''}`);

  it('draws Main first, then the roster, idle', () => {
    expect(crewCard().agents.map((a) => a.seed)).toEqual(['def-w', 'def-r']);
    expect(strip(crewCard())).toEqual(['Main:idle', 'Writer:idle', 'Researcher:idle']);
    expect(strip(crewCard([]))).toEqual(['Main:idle']);
  });

  it("keeps the workspace's Main for its face, and the last one known when a snapshot names none", () => {
    expect(crewCard().main).toEqual({ id: 'wa-main', avatar: null });
    const picked = { ...MANAGER, avatar: { seed: 'picked', generatorVersion: 1 } };
    const seeded = seedFleet(EMPTY_FLEET, [{ ...snapshotOf(), agents: [picked] }]);
    expect(card(seeded).main).toEqual({ id: 'wa-main', avatar: { seed: 'picked', generatorVersion: 1 } });
    expect(card(seedFleet(seeded, [{ ...snapshotOf(), agents: [] }])).main?.avatar?.seed).toBe('picked');
  });

  it('gives a running member the first words of its task, the whole title for the zoom', () => {
    const t = task('r1', 'running', NOW, { title: 'Crawl the October feeds and drop duplicates' });
    expect(strip(crewCard(), live([t]))).toEqual(['Main:idle', 'Writer:idle', 'Researcher:running Crawl the October…']);
    expect(cardCrew(crewCard(), live([t]), NOW).shown[2]!.doing).toBe('Crawl the October feeds and drop duplicates');
    expect(firstWords('Short title')).toBe('Short title');
  });

  it('says Main answers when a run belongs to no task', () => {
    const c = card(apply(seedFleet(EMPTY_FLEET, [{ ...snapshotOf(), agents: [MANAGER] }]), created(message('u1', 'user', 'Ship the notes. Then rest.'), NOW)));
    const crew = cardCrew(c, live([], ['run-main']), NOW).shown;
    expect(crew.map((m) => [m.status, m.doing])).toEqual([['running', 'Answering: Ship the notes.']]);
  });

  it('marks who needs you: a task asking, an approval by name, Main for the rest', () => {
    const t = task('r1', 'running', NOW, { sessionId: 'task-r' });
    let s = seedFleet(EMPTY_FLEET, [{ ...snapshotOf(), agents: [MANAGER, WRITER, RESEARCHER] }]);
    s = apply(s, env({ type: 'ask_user_requested', payload: { pending_id: 'p1', question: 'Which feeds?', options: null, multi_select: false } } as AssistantUiEvent, NOW, 'task-r'));
    s = applyRequest(s, { kind: 'approval', request: { ...(approval('a1') as object), agentName: 'Writer' } as never }, NOW);
    s = applyRequest(s, { kind: 'approval', request: approval('a2') }, NOW);
    expect(strip(card(s), live([t]))).toEqual(['Main:needs', 'Writer:needs', 'Researcher:needs']);
  });

  it('ticks a finish, fading with the clock like the other beats', () => {
    const done = task('d1', 'completed', NOW, { completedAt: BigInt(NOW) } as Partial<WorkspaceTaskResponse>);
    expect(strip(crewCard(), live([done]), NOW + 1000)).toContain('Researcher:done full');
    expect(strip(crewCard(), live([done]), NOW + FRESH_MS * 0.75)).toContain('Researcher:done half');
    expect(strip(crewCard(), live([done]), NOW + FRESH_MS)).toContain('Researcher:idle');
  });

  it("draws the Main's own tasks, their reports and their questions as the Main's", () => {
    const self = { assignedAgentDisplayName: 'Manager', assignedAgentDefinitionId: 'def-main', assignedToWorkspaceAgentId: 'wa-main' };
    const mine = task('m1', 'running', NOW - 30_000, { ...self, title: 'Daily report', sessionId: 'task-m1' });
    const done = task('m0', 'completed', NOW - 60_000, { ...self, title: 'Loki report' });
    let s = apply(crewSeed(), created(message('rep', 'user', 'Loki is fine.', report('m0', 'completed', MANAGER)), NOW - 50_000));
    s = apply(s, env({ type: 'ask_user_requested', payload: { pending_id: 'p1', question: 'Which day?', options: null, multi_select: false } } as AssistantUiEvent, NOW, 'task-m1'));
    const l = live([mine, done]);
    expect(cardFeed(card(s), l, NOW).lines.map(said)).toEqual([
      'Main → Main: Loki report',
      'Main → Main: Loki is fine. [done]',
      'Main → Main: Daily report [running]',
    ]);
    expect(strip(card(s), l)).toEqual(['Main:needs', 'Writer:idle', 'Researcher:idle']);
    expect(requestScene(card(s), l)?.asker).toBeNull();
    expect(cardAgents(card(s), l)).toEqual([]);
  });

  it('keeps an agent the roster lost while it works', () => {
    const t = task('r1', 'running', NOW, { assignedAgentDisplayName: 'Ghost', assignedAgentDefinitionId: 'def-g' });
    expect(strip(crewCard([]), live([t]))).toEqual(['Main:idle', 'Ghost:running Task r1']);
  });

  it(`past ${MAX_CREW}, draws Main and the busy ones and counts the rest`, () => {
    const team = Array.from({ length: 7 }, (_, i) => ({ ...WRITER, workspaceAgentId: `wa-${i}`, agentDefinitionId: `def-${i}`, displayName: `A${i}` }));
    const busy = [task('r1', 'running', NOW, { assignedAgentDisplayName: 'A2', assignedAgentDefinitionId: 'def-2' }), task('q1', 'queued', NOW, { assignedAgentDisplayName: 'A5', assignedAgentDefinitionId: 'def-5' })];
    const crew = cardCrew(crewCard(team), live(busy), NOW);
    expect(crew.shown.map((m) => `${m.name}:${m.status}`)).toEqual(['Main:idle', 'A2:running', 'A5:queued']);
    expect(crew.more).toHaveLength(5);
    expect(cardCrew(crewCard(team.slice(0, 5)), NO_LIVE, NOW).more).toEqual([]);
  });
});
