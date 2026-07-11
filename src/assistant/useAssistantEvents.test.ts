import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import useAssistantStore from './sessionStore';
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

const SESSION = {
  id: 'sess-1',
  kind: 'interactive',
  title: 'T',
} as unknown as AssistantSession & { tabId?: string | null };
const fire = (event: unknown, runId: string | null = null) => {
  capturedHandler?.({
    payload: {
      sessionId: SESSION.id,
      runId,
      workspaceId: 'ws-1',
      timestamp: Date.now(),
      event,
    },
  });
};

beforeEach(() => {
  useAssistantStore.setState({ sessions: {}, activeSessionByTab: {} });
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
