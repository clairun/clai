import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import useAssistantStore from './sessionStore';
import { optimisticRunNowId, useFleetActivityStore } from '../stores/fleetActivityStore';
import { useTaskStore } from '../workspace/taskStore';
import type { AssistantSession } from '../generated/bindings';

type EventHandler = (event: { payload: unknown }) => void;

// Tauri's `listen` returns a Promise<UnlistenFn>; we mock it so the hook
// receives our synthetic envelopes. Each test calls the captured handler
// directly — no real event bus required.
let capturedHandler: EventHandler | null;
let unlisten: () => void;
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((_name, handler) => {
    capturedHandler = handler;
    return Promise.resolve(() => {
      unlisten();
    });
  }),
}));

// Imported AFTER vi.mock so the hook picks up the mocked listen().
const { useAssistantEvents } = await import('./useAssistantEvents');

const SESSION = { id: 'sess-1' } as unknown as AssistantSession;
const fire = (
  event: unknown,
  runId: string | null = null,
  workspaceId = 'ws-1',
  timestamp = Date.now(),
  sessionId = SESSION.id
) => {
  capturedHandler?.({
    payload: {
      sessionId,
      runId,
      workspaceId,
      timestamp,
      event,
    },
  });
};

beforeEach(() => {
  useAssistantStore.setState({ sessions: {}, activeSessionByTab: {} });
  useFleetActivityStore.setState({
    activeRunsByWorkspace: {},
    runNowSkipped: [],
    skippedRunByWorkspace: {},
    latestAcceptedRunBySession: {},
  });
  capturedHandler = null;
  unlisten = vi.fn();
});

const mount = () => renderHook(() => useAssistantEvents());

describe('useAssistantEvents — ask_user envelope', () => {
  // Regression seed: this is the round-trip that ferries the BE event
  // through Tauri's event bus and into the zustand store. The field
  // naming convention (snake_case from Rust serde) is load-bearing —
  // a `payload.pendingId` typo here was the exact kind of bug TS would
  // catch but JS doesn't; until we have TS, this test pins the contract.

  it('ask_user_requested writes pendingAskUser onto the session', async () => {
    useAssistantStore.getState().initSession(SESSION);
    mount();
    // Wait one microtask for the listen() Promise.resolve() to register.
    await Promise.resolve();
    fire({
      type: 'ask_user_requested',
      payload: {
        pending_id: 'p-1',
        question: 'Do the thing?',
        options: [{ label: 'yes' }],
        extra_context: 'because reasons',
      },
    });
    const pending = useAssistantStore.getState().sessions[SESSION.id]!.pendingAskUser;
    expect(pending).toEqual({
      pendingId: 'p-1',
      question: 'Do the thing?',
      options: [{ label: 'yes' }],
      extraContext: 'because reasons',
    });
  });

  it('ask_user_resolved clears the matching pending request', async () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setAskUserPending(SESSION.id, {
      pendingId: 'p-1',
      question: 'q',
      options: null,
      extraContext: null,
    });
    mount();
    await Promise.resolve();
    fire({ type: 'ask_user_resolved', payload: { pending_id: 'p-1' } });
    expect(useAssistantStore.getState().sessions[SESSION.id]!.pendingAskUser).toBeNull();
  });
});

describe('useAssistantEvents — run lifecycle', () => {
  it('run_completed updates the run record and clears streaming', async () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.appendDelta(SESSION.id, 'msg-1', 'partial text');
    mount();
    await Promise.resolve();
    fire({
      type: 'run_completed',
      payload: { run: { id: 'run-1', status: 'completed' } },
    });
    const s = useAssistantStore.getState().sessions[SESSION.id]!;
    expect(s.runs).toEqual([{ id: 'run-1', status: 'completed' }]);
    expect(s.isStreaming).toBe(false);
  });

  // R3.2 regression, at the level the user actually experiences it. Pressing
  // Stop mid-answer used to leave the assistant row as the empty placeholder
  // it was created with, so the visible text lived only in `streamingText` —
  // and `run_cancelled` wipes that accumulator wholesale. The answer vanished
  // the instant the run went terminal. The backend now persists whatever was
  // streamed and emits `assistant_message_completed` *before* `run_cancelled`;
  // this pins that the text survives that pair, in that order.
  it('keeps the partial answer when a run is cancelled mid-stream', async () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    mount();
    await Promise.resolve();

    fire({
      type: 'message_created',
      payload: {
        message: { id: 'msg-1', role: 'assistant', content: [{ type: 'text', text: '' }] },
      },
    });
    fire({ type: 'assistant_delta', payload: { message_id: 'msg-1', text: 'Half a th' } });
    expect(useAssistantStore.getState().streamingText[SESSION.id]!['msg-1']).toBe('Half a th');

    // The backend finalizes the row from what it streamed, then marks the run
    // cancelled. Order matters: the second event clears the accumulator.
    fire({
      type: 'assistant_message_completed',
      payload: {
        message: {
          id: 'msg-1',
          role: 'assistant',
          content: [{ type: 'text', text: 'Half a th' }],
        },
      },
    });
    fire({
      type: 'run_cancelled',
      payload: { run: { id: 'run-1', status: 'cancelled' } },
    });

    const s = useAssistantStore.getState().sessions[SESSION.id]!;
    expect(s.messages).toHaveLength(1);
    expect(s.messages[0]!.content).toEqual([{ type: 'text', text: 'Half a th' }]);
    expect(s.isStreaming).toBe(false);
    // The accumulator is gone, so the surviving text comes from the persisted
    // message and nothing renders twice.
    expect(useAssistantStore.getState().streamingText[SESSION.id]).toBeUndefined();
  });

  it('assistant_delta accumulates streaming text under message_id', async () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    mount();
    await Promise.resolve();
    fire({
      type: 'assistant_delta',
      payload: { message_id: 'msg-1', text: 'Hello ' },
    });
    fire({
      type: 'assistant_delta',
      payload: { message_id: 'msg-1', text: 'world' },
    });
    expect(useAssistantStore.getState().streamingText[SESSION.id]!['msg-1']).toBe('Hello world');
  });
});

describe('useAssistantEvents — run_skipped', () => {
  const RUN_SKIPPED = {
    type: 'run_skipped',
    payload: { reason: 'session_busy', activeRunId: 'run-busy' },
  };

  it('run_skipped records inline feedback and clears optimistic run', async () => {
    useFleetActivityStore.getState().markRunStarted('ws-1', optimisticRunNowId('ws-1'));
    mount();
    await Promise.resolve();

    act(() => fire(RUN_SKIPPED));

    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-1']);
    expect(useFleetActivityStore.getState().activeRunsByWorkspace).toEqual({});
  });

  it('clears skipped feedback across windows only when a later run starts', async () => {
    mount();
    await Promise.resolve();
    const skippedAt = Date.now();

    fire(RUN_SKIPPED, null, 'ws-1', skippedAt);
    fire(RUN_SKIPPED, null, 'ws-2', skippedAt);
    // The active run's start can arrive late; it did not accept the retry.
    fire(
      {
        type: 'run_started',
        payload: { run: { id: 'run-busy', startedAt: BigInt(skippedAt - 1) } },
      },
      'run-busy',
      'ws-1',
      skippedAt + 1
    );
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-2', 'ws-1']);
    fire(
      {
        type: 'run_queued',
        payload: { run: { id: 'run-retry', startedAt: BigInt(skippedAt + 1) } },
      },
      'run-retry',
      'ws-1',
      skippedAt + 2
    );
    fire(
      {
        type: 'run_started',
        payload: { run: { id: 'other-session', startedAt: BigInt(skippedAt + 1) } },
      },
      'other-session',
      'ws-1',
      skippedAt + 2,
      'sess-other'
    );
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-2', 'ws-1']);

    // This event is broadcast to every window after the retry creates a new run.
    fire(
      {
        type: 'run_started',
        payload: { run: { id: 'run-retry', startedAt: BigInt(skippedAt + 1) } },
      },
      'run-retry',
      'ws-1',
      skippedAt + 2
    );
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-2']);

    // A later busy retry produces fresh feedback, which remains visible.
    fire(RUN_SKIPPED, null, 'ws-1', skippedAt + 3);
    fire(
      {
        type: 'run_started',
        payload: { run: { id: 'older-run', startedAt: BigInt(skippedAt - 1) } },
      },
      'older-run',
      'ws-1',
      skippedAt + 4
    );
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-1', 'ws-2']);
  });

  it('clears skipped feedback when an accepted retry fails before starting', async () => {
    mount();
    await Promise.resolve();
    const skippedAt = Date.now();
    const failedRun = (id: string, startedAt: number) => ({
      id,
      sessionId: SESSION.id,
      status: 'failed',
      trigger: 'scheduled',
      connectionId: 'connection',
      protocolId: 'provider',
      modelId: 'model',
      startedAt: BigInt(startedAt),
      completedAt: BigInt(startedAt + 1),
      error: 'provider not configured',
    });

    fire(RUN_SKIPPED, null, 'ws-1', skippedAt);
    fire({ type: 'run_failed', payload: { run: failedRun('run-busy', skippedAt - 1) } }, 'run-busy', 'ws-1', skippedAt + 1);
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-1']);

    fire({ type: 'run_failed', payload: { run: failedRun('run-retry', skippedAt + 2) } }, 'run-retry', 'ws-1', skippedAt + 3);
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual([]);

    fire(RUN_SKIPPED, null, 'ws-1', skippedAt);
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual([]);
    fire(RUN_SKIPPED, null, 'ws-1', skippedAt + 4);
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual(['ws-1']);
  });

  it('ignores a delayed skip after its retry already started', async () => {
    mount();
    await Promise.resolve();
    const skippedAt = Date.now();

    fire(
      {
        type: 'run_started',
        payload: { run: { id: 'run-retry', startedAt: BigInt(skippedAt + 1) } },
      },
      'run-retry',
      'ws-1',
      skippedAt + 2
    );
    fire(RUN_SKIPPED, null, 'ws-1', skippedAt);
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual([]);
  });

  it('keeps the newer skip when an older skip and start arrive late', async () => {
    mount();
    await Promise.resolve();
    const at = Date.now();
    const skip = (activeRunId: string) => ({
      type: 'run_skipped',
      payload: { reason: 'session_busy', activeRunId },
    });

    // B is already busy when its skip is delivered. A's older skip arrives afterward.
    fire(skip('run-b'), null, 'ws-1', at + 20);
    useFleetActivityStore.getState().markRunStarted('ws-1', optimisticRunNowId('ws-1'));
    fire(skip('run-a'), null, 'ws-1', at + 10);
    fire(
      { type: 'run_started', payload: { run: { id: 'run-b', startedAt: BigInt(at + 15) } } },
      'run-b',
      'ws-1',
      at + 21
    );

    const state = useFleetActivityStore.getState();
    expect(state.runNowSkipped).toEqual(['ws-1']);
    expect(state.skippedRunByWorkspace['ws-1']?.activeRunId).toBe('run-b');
    expect(state.activeRunsByWorkspace['ws-1']).toContain(optimisticRunNowId('ws-1'));
  });

  it('run_skipped for another workspace keys feedback by workspaceId', async () => {
    const fleet = useFleetActivityStore.getState();
    fleet.markRunStarted('ws-1', optimisticRunNowId('ws-1'));
    fleet.markRunStarted('ws-2', optimisticRunNowId('ws-2'));
    mount();
    await Promise.resolve();

    fire(RUN_SKIPPED, null, 'ws-2');
    fire(RUN_SKIPPED, null, 'ws-2');

    const state = useFleetActivityStore.getState();
    expect(state.runNowSkipped).toEqual(['ws-2']);
    expect(state.activeRunsByWorkspace).toEqual({ 'ws-1': [optimisticRunNowId('ws-1')] });
  });
});

describe('useAssistantEvents — task_updated', () => {
  // As the Rust side serializes it: camelCase inside `task`, bigints as numbers.
  const wireTask = {
    id: 'task-1',
    workspaceId: 'ws-1',
    createdByWorkspaceAgentId: 'wa-main',
    createdByDisplayName: 'Main',
    assignedToWorkspaceAgentId: 'wa-coder',
    assignedAgentDefinitionId: 'def-coder',
    assignedAgentDisplayName: 'Coder',
    title: 'Fix flaky test',
    instructions: 'fix it',
    status: 'failed',
    failureReason: 'blocked',
    resultSummary: null,
    error: 'Need repo access',
    sessionId: 'sess-1',
    runId: 'run-1',
    parentSessionId: 'sess-main',
    parentRunId: 'run-main',
    parentTaskId: null,
    depth: 0,
    cycleNo: 2,
    createdAt: 1,
    updatedAt: 9,
    completedAt: 9,
    attentionAcknowledgedAt: null,
  };

  beforeEach(() => useTaskStore.setState({ tasks: {}, seeded: {} }));

  it('task_updated upserts store', async () => {
    mount();
    await Promise.resolve();
    fire({ type: 'task_updated', payload: { task: { ...wireTask, status: 'running' } } });
    expect(useTaskStore.getState().tasks['ws-1']?.['task-1']?.status).toBe('running');
    fire({ type: 'task_updated', payload: { task: { ...wireTask, updatedAt: 10 } } });
    expect(useTaskStore.getState().tasks['ws-1']?.['task-1']?.status).toBe('failed');
  });

  it('task_updated payload parsed camelCase', async () => {
    mount();
    await Promise.resolve();
    fire({ type: 'task_updated', payload: { task: wireTask } });
    const task = useTaskStore.getState().tasks['ws-1']?.['task-1'];
    expect(task).toMatchObject({
      workspaceId: 'ws-1',
      failureReason: 'blocked',
      parentSessionId: 'sess-main',
      cycleNo: 2,
    });
  });
});
