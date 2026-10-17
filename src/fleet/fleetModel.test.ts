import { describe, expect, it } from 'vitest';
import type {
  AssistantEventEnvelope,
  AssistantMessage,
  AssistantRun,
  AssistantUiEvent,
  WorkspaceTaskResponse,
} from '../generated/bindings';
import {
  EMPTY_FLEET,
  RECENT_WINDOW_MS,
  applyAssistantEvent,
  applyAttentionCount,
  applyRequest,
  cardAttention,
  cardContent,
  cardItems,
  cardPhase,
  emptyCard,
  gridCapacity,
  placeCards,
  seedFleet,
  type FleetCard,
  type FleetSeedWorkspace,
  type FleetState,
} from './fleetModel';

const WS = 'ws-1';
const MAIN = 'main-session';

const env = (event: AssistantUiEvent, at: number, sessionId = MAIN, workspaceId = WS): AssistantEventEnvelope =>
  ({ sessionId, runId: null, workspaceId, timestamp: BigInt(at), event }) as AssistantEventEnvelope;

const message = (id: string, role: 'user' | 'assistant', text: string, origin: AssistantMessage['origin'] = { kind: 'user' }): AssistantMessage =>
  ({ id, sessionId: MAIN, role, content: [{ type: 'text', text }], createdAt: 0n, providerMetadata: null, origin }) as AssistantMessage;

const run = (id: string, trigger: AssistantRun['trigger'] = 'user_message', sessionId = MAIN): AssistantRun =>
  ({ id, sessionId, status: 'running', trigger, connectionId: 'c', protocolId: 'p', modelId: 'm', startedAt: 0n, completedAt: null, error: null }) as AssistantRun;

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

const apply = (state: FleetState, ...envelopes: AssistantEventEnvelope[]) =>
  envelopes.reduce(applyAssistantEvent, state);

const card = (state: FleetState, id = WS): FleetCard => state.cards[id]!;

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

  it('tracks main-session runs only; task runs never mark the main busy', () => {
    let s = apply(
      EMPTY_FLEET,
      env({ type: 'task_updated', payload: { task: task('t1', 'running', 1) } }, 1, 'task-session-t1'),
      env({ type: 'run_started', payload: { run: run('tr', 'workspace_task', 'task-session-t1') } }, 2, 'task-session-t1'),
    );
    expect(card(s).mainRuns).toEqual([]);
    expect(card(s).mainSessionId).toBeNull();
    s = apply(s, env({ type: 'run_started', payload: { run: run('r1') } }, 3));
    expect(card(s).mainSessionId).toBe(MAIN);
    expect(card(s).mainRuns).toEqual(['r1']);
    s = apply(s, env({ type: 'run_completed', payload: { run: run('r1') } }, 4));
    expect(card(s).mainRuns).toEqual([]);
  });

  it('keeps the newer copy of a task regardless of arrival order', () => {
    const s = apply(
      EMPTY_FLEET,
      env({ type: 'task_updated', payload: { task: task('t1', 'completed', 20) } }, 5),
      env({ type: 'task_updated', payload: { task: task('t1', 'running', 10) } }, 6),
    );
    expect(card(s).tasks.t1!.status).toBe('completed');
  });

  it('keeps finished subtasks and resolved requests as low-emphasis history', () => {
    let s = apply(
      EMPTY_FLEET,
      env({ type: 'task_updated', payload: { task: task('done', 'completed', 1) } }, 1),
      env({ type: 'task_updated', payload: { task: task('live', 'running', 2) } }, 2),
      env({ type: 'ask_user_requested', payload: { pending_id: 'q1', question: 'Ship it?', options: null, multi_select: false } } as AssistantUiEvent, 3),
      env({ type: 'ask_user_resolved', payload: { pending_id: 'q1' } } as AssistantUiEvent, 4),
    );
    s = applyRequest(s, 'approval', { requestId: 'a1', workspaceId: WS, agentId: null, agentName: 'Main', command: 'ls', segments: [] } as never, 5);
    const items = cardItems(card(s));
    expect(items.map((i) => [i.key, i.tone])).toEqual([
      ['approval:a1', 'attention'],
      ['task:live', 'live'],
      ['ask:q1', 'done'],
      ['task:done', 'done'],
    ]);
    s = applyAttentionCount(s, 'approval', WS, 0, 6);
    expect(cardItems(card(s)).find((i) => i.key === 'approval:a1')!.tone).toBe('done');
    expect(cardAttention(card(s)).total).toBe(0);
  });

  it('folds subtasks into their top-level task and stays live while any descendant runs', () => {
    const s = apply(
      EMPTY_FLEET,
      env({ type: 'task_updated', payload: { task: task('root', 'completed', 1) } }, 1),
      env({ type: 'task_updated', payload: { task: task('child', 'running', 2, { parentTaskId: 'root', assignedAgentDefinitionId: 'def-w', assignedAgentDisplayName: 'Writer' }) } }, 2),
    );
    const items = cardItems(card(s));
    expect(items).toHaveLength(1);
    expect(items[0]!.tone).toBe('live');
    expect(items[0]!.agents.map((a) => a.name)).toEqual(['Researcher', 'Writer']);
  });

  it('shows the last assistant message only once the workspace has stopped', () => {
    let s = apply(
      EMPTY_FLEET,
      env({ type: 'run_started', payload: { run: run('r1') } }, 1),
      env({ type: 'task_updated', payload: { task: task('t1', 'running', 2) } }, 2),
      env({ type: 'assistant_message_completed', payload: { message: message('a1', 'assistant', 'All done.') } }, 3),
    );
    expect(cardContent(card(s)).body).toBeNull();
    s = apply(s, env({ type: 'run_completed', payload: { run: run('r1') } }, 4));
    expect(cardPhase(card(s))).toBe('working');
    expect(cardContent(card(s)).body).toBeNull();
    s = apply(s, env({ type: 'task_updated', payload: { task: task('t1', 'completed', 5) } }, 5));
    expect(cardPhase(card(s))).toBe('stopped');
    expect(cardContent(card(s)).body).toBe('All done.');
  });

  it('ignores assistant messages from task sessions', () => {
    const s = apply(
      EMPTY_FLEET,
      env({ type: 'run_started', payload: { run: run('r1') } }, 1),
      env({ type: 'task_updated', payload: { task: task('t1', 'completed', 2) } }, 2, 'task-session-t1'),
      env({ type: 'assistant_message_completed', payload: { message: message('x', 'assistant', 'task text') } }, 3, 'task-session-t1'),
    );
    expect(card(s).lastMessage).toBeNull();
  });

  it("shows the user's own message until another message or event arrives", () => {
    let s = apply(EMPTY_FLEET, env({ type: 'message_created', payload: { message: message('u1', 'user', 'Please deploy') } }, 1));
    expect(card(s).userMessage?.text).toBe('Please deploy');
    expect(card(s).mainSessionId).toBe(MAIN);
    // Run lifecycle is not a displayed event: the message stays.
    s = apply(s, env({ type: 'run_started', payload: { run: run('r1') } }, 2));
    expect(card(s).userMessage?.text).toBe('Please deploy');
    s = apply(s, env({ type: 'task_updated', payload: { task: task('t1', 'running', 3) } }, 3));
    expect(card(s).userMessage).toBeNull();
    s = apply(s, env({ type: 'message_created', payload: { message: message('u2', 'user', 'Also this') } }, 4));
    s = apply(s, env({ type: 'assistant_message_completed', payload: { message: message('a1', 'assistant', 'On it') } }, 5));
    expect(card(s).userMessage).toBeNull();
  });

  it('does not take a task-result message for the user', () => {
    const s = apply(
      EMPTY_FLEET,
      env({ type: 'message_created', payload: { message: message('u1', 'user', 'go') } }, 1),
      env({ type: 'message_created', payload: { message: message('m2', 'user', 'result', { kind: 'task', taskId: 't', agent: { workspaceAgentId: 'a', displayName: 'R', isDefault: false }, title: 'T', cycleNo: 1n, status: 'completed' }) } }, 2),
    );
    expect(card(s).userMessage).toBeNull();
  });

  it('seed replaces live state, keeps history, and buffered events replay on top', () => {
    let s = apply(
      EMPTY_FLEET,
      env({ type: 'task_updated', payload: { task: task('old', 'completed', 1) } }, 1),
      env({ type: 'ask_user_requested', payload: { pending_id: 'stale', question: '?', options: null, multi_select: false } } as AssistantUiEvent, 2),
    );
    const seed: FleetSeedWorkspace = {
      workspaceId: WS,
      title: 'Infra',
      lastActivityAt: 10,
      mainSessionId: MAIN,
      activeRunIds: ['r9'],
      tasks: [task('t1', 'running', 9)],
      lastMessage: null,
      asks: [],
      requests: [],
      pendingApprovals: 0,
      pendingGrants: 0,
    };
    s = seedFleet(s, [seed], 50);
    expect(card(s).title).toBe('Infra');
    expect(card(s).mainRuns).toEqual(['r9']);
    expect(card(s).asks.stale!.resolvedAt).toBe(50);
    expect(Object.keys(card(s).tasks).sort()).toEqual(['old', 't1']);
    // A buffered completion replays after the seed and wins.
    s = apply(s, env({ type: 'run_completed', payload: { run: run('r9') } }, 11));
    expect(card(s).mainRuns).toEqual([]);
  });
});

describe('card placement', () => {
  const NOW = 1_000_000_000;
  const mk = (id: string, lastActivityAt: number, patch: Partial<FleetCard> = {}): FleetCard => ({
    ...emptyCard(id, id),
    lastActivityAt,
    ...patch,
  });
  const ids = (placed: { id: string }[]) => placed.map((p) => p.id);

  it('derives capacity from the screen size', () => {
    expect(gridCapacity(300, 150)).toEqual({ cols: 1, rows: 1 });
    expect(gridCapacity(1400, 700)).toEqual({ cols: 4, rows: 3 });
    expect(gridCapacity(900, 450)).toEqual({ cols: 2, rows: 2 });
  });

  it('never shows old idle workspaces, but always shows live ones', () => {
    const cards = [
      mk('recent', NOW - 1000),
      mk('old', NOW - RECENT_WINDOW_MS - 1),
      mk('old-but-running', NOW - RECENT_WINDOW_MS - 1, { mainRuns: ['r'] }),
    ];
    const placed = placeCards(cards, { now: NOW, capacity: { cols: 4, rows: 3 }, selectedId: null, prevOrder: [] });
    expect(ids(placed).sort()).toEqual(['old-but-running', 'recent']);
  });

  it('shows the most recent when more are active than fit', () => {
    const cards = [mk('a', NOW - 3), mk('b', NOW - 1), mk('c', NOW - 2)];
    const placed = placeCards(cards, { now: NOW, capacity: { cols: 2, rows: 1 }, selectedId: null, prevOrder: [] });
    expect(ids(placed)).toEqual(['b', 'c']);
  });

  it('keeps the selected card visible and larger', () => {
    const cards = [mk('a', NOW - 1), mk('b', NOW - 2), mk('sel', NOW - 50)];
    const placed = placeCards(cards, { now: NOW, capacity: { cols: 3, rows: 2 }, selectedId: 'sel', prevOrder: [] });
    expect(placed.find((p) => p.id === 'sel')).toEqual({ id: 'sel', cols: 2, rows: 2 });
    expect(placed).toHaveLength(3);
  });

  it('moves waiting cards first and grows them; others keep their order', () => {
    const cards = [
      mk('a', NOW - 1),
      mk('b', NOW - 2),
      mk('w', NOW - 3, { pendingApprovals: 1 }),
    ];
    const placed = placeCards(cards, { now: NOW, capacity: { cols: 3, rows: 2 }, selectedId: null, prevOrder: ['b', 'a', 'w'] });
    expect(placed).toEqual([
      { id: 'w', cols: 2, rows: 1 },
      { id: 'b', cols: 1, rows: 1 },
      { id: 'a', cols: 1, rows: 1 },
    ]);
  });
});
