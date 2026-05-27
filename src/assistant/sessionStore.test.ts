import { beforeEach, describe, expect, it } from 'vitest';
import useAssistantStore from './sessionStore';
import type { AssistantMessage, AssistantRun, AssistantSession } from '../generated/bindings';

// Minimal typed fixtures — the store only reads ids/status/content, so we
// cast partial shapes rather than spell out every required field.
const SESSION = {
  id: 'sess-1',
  kind: 'interactive',
  title: 'Test session',
  tabId: null,
} as unknown as AssistantSession & { tabId?: string | null };

const msg = (id: string): AssistantMessage =>
  ({ id, content: [] }) as unknown as AssistantMessage;
const run = (id: string, status: string): AssistantRun =>
  ({ id, status }) as unknown as AssistantRun;

const ASK_REQUEST = {
  pendingId: 'pending-abc',
  question: 'Pick one',
  options: [{ label: 'A' }, { label: 'B' }],
  extraContext: null,
};

beforeEach(() => {
  // Zustand exposes setState on the hook itself; reset the slice that
  // every test below mutates. Avoids cross-test bleed.
  useAssistantStore.setState({ sessions: {}, activeSessionByTab: {} });
});

describe('initSession', () => {
  it('creates a fresh entry when none exists', () => {
    useAssistantStore.getState().initSession(SESSION);
    const s = useAssistantStore.getState().sessions[SESSION.id];
    expect(s).toBeDefined();
    expect(s.messages).toEqual([]);
    expect(s.pendingAskUser).toBeNull();
  });

  it('is idempotent — does not clobber an existing entry', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setAskUserPending(SESSION.id, ASK_REQUEST);
    store.initSession(SESSION);
    expect(useAssistantStore.getState().sessions[SESSION.id].pendingAskUser).toEqual(
      ASK_REQUEST,
    );
  });
});

describe('setAskUserPending / clearAskUserPending', () => {
  it('writes the request onto the session', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setAskUserPending(SESSION.id, ASK_REQUEST);
    expect(useAssistantStore.getState().sessions[SESSION.id].pendingAskUser).toEqual(
      ASK_REQUEST,
    );
  });

  it('is a no-op when the session has not been initialized', () => {
    useAssistantStore.getState().setAskUserPending('unknown-session', ASK_REQUEST);
    expect(useAssistantStore.getState().sessions['unknown-session']).toBeUndefined();
  });

  it('clears the pending request when pendingId matches', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setAskUserPending(SESSION.id, ASK_REQUEST);
    store.clearAskUserPending(SESSION.id, ASK_REQUEST.pendingId);
    expect(useAssistantStore.getState().sessions[SESSION.id].pendingAskUser).toBeNull();
  });

  it('ignores a clear with a stale pendingId so a late resolve does not wipe a newer question', () => {
    // Documented invariant in sessionStore.js — a late-arriving
    // ask_user_resolved for a previous question must not unmount the
    // panel for the current question.
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setAskUserPending(SESSION.id, ASK_REQUEST);
    store.clearAskUserPending(SESSION.id, 'stale-pending-id');
    expect(useAssistantStore.getState().sessions[SESSION.id].pendingAskUser).toEqual(
      ASK_REQUEST,
    );
  });
});

describe('loadSessionData — snapshot refresh preserves in-flight FE state', () => {
  // Regression: ask_user panel was being unmounted within ~5s because
  // Workspace.jsx polls workspace_get_snapshot every 5s and the wholesale
  // replacement in loadSessionData was wiping pendingAskUser. Same race
  // existed (and is also fixed) for streamingTextByMessageId/isStreaming.

  it('preserves pendingAskUser across a snapshot refresh', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setAskUserPending(SESSION.id, ASK_REQUEST);
    store.loadSessionData(SESSION.id, SESSION, [], [], []);
    expect(useAssistantStore.getState().sessions[SESSION.id].pendingAskUser).toEqual(
      ASK_REQUEST,
    );
  });

  it('preserves streamingTextByMessageId across a snapshot refresh', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.appendDelta(SESSION.id, 'msg-1', 'Hello ');
    store.appendDelta(SESSION.id, 'msg-1', 'world');
    store.loadSessionData(SESSION.id, SESSION, [], [], []);
    expect(
      useAssistantStore.getState().sessions[SESSION.id].streamingTextByMessageId['msg-1'],
    ).toBe('Hello world');
    expect(useAssistantStore.getState().sessions[SESSION.id].isStreaming).toBe(true);
  });

  it('overwrites messages/runs/toolCalls with the snapshot payload', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.addMessage(SESSION.id, msg('old-msg'));
    store.loadSessionData(
      SESSION.id,
      SESSION,
      [msg('new-msg-1'), msg('new-msg-2')],
      [run('run-1', 'completed')],
      [],
    );
    const s = useAssistantStore.getState().sessions[SESSION.id];
    expect(s.messages.map((m) => m.id)).toEqual(['new-msg-1', 'new-msg-2']);
    expect(s.runs.map((r) => r.id)).toEqual(['run-1']);
  });
});

describe('addMessage', () => {
  it('deduplicates by id', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.addMessage(SESSION.id, msg('msg-1'));
    store.addMessage(SESSION.id, msg('msg-1'));
    expect(useAssistantStore.getState().sessions[SESSION.id].messages).toHaveLength(1);
  });
});

describe('setRunStatus', () => {
  it('clears streaming state on terminal status', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.appendDelta(SESSION.id, 'msg-1', 'partial');
    store.setRunStatus(SESSION.id, run('run-1', 'completed'));
    const s = useAssistantStore.getState().sessions[SESSION.id];
    expect(s.isStreaming).toBe(false);
    expect(s.streamingTextByMessageId).toEqual({});
  });

  it('sets streaming on queued/running/waiting_for_tool', () => {
    const store = useAssistantStore.getState();
    store.initSession(SESSION);
    store.setRunStatus(SESSION.id, run('run-1', 'queued'));
    expect(useAssistantStore.getState().sessions[SESSION.id].isStreaming).toBe(true);
  });
});
