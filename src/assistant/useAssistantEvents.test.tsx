import { beforeEach, describe, expect, it, vi } from 'vitest';
import React from 'react';
import { act, render, renderHook, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import useAssistantStore from './sessionStore';
import { optimisticRunNowId, useFleetActivityStore } from '../stores/fleetActivityStore';
import WorkspaceTaskNotifications from '../components/WorkspaceTaskNotifications';
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
const fire = (event: unknown, runId: string | null = null, workspaceId = 'ws-1') => {
  capturedHandler?.({
    payload: {
      sessionId: SESSION.id,
      runId,
      workspaceId,
      timestamp: Date.now(),
      event,
    },
  });
};

beforeEach(() => {
  useAssistantStore.setState({ sessions: {}, activeSessionByTab: {} });
  useFleetActivityStore.setState({ activeRunsByWorkspace: {}, runNowSkipped: [] });
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
    expect(
      useAssistantStore.getState().streamingText[SESSION.id]!['msg-1'],
    ).toBe('Hello world');
  });
});

describe('useAssistantEvents — run_skipped', () => {
  const RUN_SKIPPED = {
    type: 'run_skipped',
    payload: { reason: 'session_busy', activeRunId: 'run-busy' },
  };

  it('run_skipped shows busy toast and clears optimistic run', async () => {
    useFleetActivityStore.getState().markRunStarted('ws-1', optimisticRunNowId('ws-1'));
    // Rendered before the hook so the hook's listener is the captured one.
    render(
      <MemoryRouter>
        <WorkspaceTaskNotifications />
      </MemoryRouter>,
    );
    mount();
    await Promise.resolve();

    act(() => fire(RUN_SKIPPED));

    expect(screen.getByRole('status')).toHaveTextContent('Run now skipped: session busy');
    expect(useFleetActivityStore.getState().activeRunsByWorkspace).toEqual({});
  });

  it('run_skipped for another workspace keys toast by workspaceId', async () => {
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
