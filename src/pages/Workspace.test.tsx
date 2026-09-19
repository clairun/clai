import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Route, Routes, useNavigate } from 'react-router';

import Workspace from './Workspace';
import styles from './Workspace.module.css';
import useAssistantStore from '../assistant/sessionStore';
import type {
  AssistantMessage,
  AssistantMessagePage,
  AssistantSession,
  WorkspaceDetails,
  WorkspaceDetailsOptions,
  WorkspaceFileEntry,
} from '../generated/bindings';

// The chat panel mounts the inline approval / path-grant cards, which
// subscribe to Tauri events on mount. Without a window bridge those
// subscriptions reject; stub the boundary so the page can render its
// conversation under jsdom.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock('../workspace/client', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../workspace/client')>()),
  getWorkspaceDetails: vi.fn(),
  markWorkspaceOpened: vi.fn(async () => {}),
  getOrCreateWorkspaceSession: vi.fn(),
  listWorkspaceDir: vi.fn(async () => []),
}));

vi.mock('../assistant/client', () => ({
  loadSessionMessagesPage: vi.fn(),
  listRuns: vi.fn(),
  cancelRun: vi.fn(async () => {}),
  sendMessage: vi.fn(async () => {}),
  deleteQueuedMessage: vi.fn(async () => {}),
  editQueuedMessage: vi.fn(async () => {}),
}));

const workspaceClient = await import('../workspace/client');
const getWorkspaceDetails = vi.mocked(workspaceClient.getWorkspaceDetails);
const getOrCreateWorkspaceSession = vi.mocked(workspaceClient.getOrCreateWorkspaceSession);
// Taken through `vi.mocked` on the real module, so the fixtures below are
// checked against the command's actual return types: a page missing
// `toolCalls`/`nextCursor`/`hasMore`/`totalCount` — all four read by
// `loadDetails` — stops compiling instead of feeding `undefined` into the store.
const assistantClient = await import('../assistant/client');
const loadSessionMessagesPage = vi.mocked(assistantClient.loadSessionMessagesPage);
const listRuns = vi.mocked(assistantClient.listRuns);

const MEMORY: WorkspaceFileEntry = {
  path: '.clai/memory/knowledge.md',
  relativePath: '.clai/memory/knowledge.md',
  name: 'knowledge.md',
  viewer: 'markdown',
  size: 128n,
  updatedAt: 1n,
  preview: null,
};

const sessionFor = (workspaceId: string, updatedAt: bigint): AssistantSession => ({
  id: `sess-${workspaceId}`,
  kind: 'interactive',
  title: null,
  context: {
    workspaceId,
    toolScopes: [],
    mcpServerIds: [],
    execution: {},
    cliSessionId: null,
    cliSessionProvider: null,
    automationId: null,
    agentWorkspaceId: null,
    automationName: null,
    interAgentCall: null,
    workspaceAgents: [],
  },
  createdAt: 0n,
  updatedAt,
});

const userMessage = (sessionId: string, id: string, text: string): AssistantMessage => ({
  id,
  sessionId,
  role: 'user',
  content: [{ type: 'text', text }],
  createdAt: 1n,
  providerMetadata: null,
});

// An assistant turn whose text has not been persisted yet: the backend
// writes the row when the turn opens and flushes its content at end of run,
// so mid-stream the page carries the message with empty content and the text
// on screen comes from the delta accumulator.
const streamingAssistantMessage = (sessionId: string, id: string): AssistantMessage => ({
  id,
  sessionId,
  role: 'assistant',
  content: [],
  createdAt: 2n,
  providerMetadata: null,
});

const messagePage = (messages: AssistantMessage[]): AssistantMessagePage => ({
  messages,
  toolCalls: [],
  nextCursor: null,
  hasMore: false,
  totalCount: messages.length,
});

// Stands in for `workspace_get_details`, honouring its one flag: `details_files`
// short-circuits to no memories and a zero artifact count when the caller did
// not ask for files (src-tauri/src/commands/workspace.rs, `details_files`). So a
// poll that stops asking produces an empty workspace here exactly as it does in
// the app.
const detailsFor = (
  options: WorkspaceDetailsOptions,
  overrides: Partial<WorkspaceDetails> = {}
): WorkspaceDetails => ({
  workspaceId: 'a',
  kind: 'general',
  title: 'Alpha',
  agentId: null,
  assignedAgents: [],
  tasks: [],
  defaultWorkspaceAgentId: null,
  rootPath: '/tmp/a',
  providerConnectionIds: [],
  providerConnectionNames: [],
  selectedMcpServerIds: [],
  disabledMcpServerIds: [],
  // A workspace with a conversation is the common case, and the session is
  // what makes `loadDetails` hydrate the store at all: a `null` here skips
  // the whole session half of the loader.
  session: sessionFor('a', 10n),
  runs: [],
  memories: options.includeFiles ? [MEMORY] : [],
  artifactCount: options.includeFiles ? 7n : 0n,
  artifactCountCapped: false,
  artifactLatestModifiedAt: 0n,
  queuedMessageIds: [],
  enabled: true,
  scheduleEnabled: false,
  schedulePaused: false,
  scheduleKind: null,
  nextRunInSeconds: null,
  ...overrides,
});

const GoToBeta = () => {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate('/workspace/b')}>
      go to beta
    </button>
  );
};

const renderWorkspace = () =>
  render(
    <MemoryRouter initialEntries={['/workspace/a']}>
      <Routes>
        <Route path="/workspace/:workspaceId" element={<Workspace />} />
      </Routes>
    </MemoryRouter>
  );

beforeEach(() => {
  // `streamingText` is reset alongside the sessions because the streaming
  // test below leaves deltas keyed by `sess-a`, and every other test here
  // hydrates that same session id — a leftover accumulator would render a
  // phantom assistant turn in an unrelated test.
  useAssistantStore.setState({ sessions: {}, activeSessionByTab: {}, streamingText: {} });
  getWorkspaceDetails.mockImplementation(async (_workspaceId, options) => detailsFor(options));
  getOrCreateWorkspaceSession.mockResolvedValue({
    session: sessionFor('a', 10n),
    providerConnectionId: 'conn-1',
  });
  loadSessionMessagesPage.mockResolvedValue(messagePage([]));
  listRuns.mockResolvedValue([]);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('Workspace details poll', () => {
  it('surfaces the workspace files the poll asked for', async () => {
    renderWorkspace();

    // Both counters come from the file walk, which the backend performs only
    // for a caller that asked for it — they read 0 for a populated workspace
    // if the poll stops requesting files.
    const memories = await screen.findByRole('button', { name: /memories/i });
    expect(memories).toHaveTextContent('1');
    expect(screen.getByRole('button', { name: /artifacts/i })).toHaveTextContent('7');

    await userEvent.click(memories);
    expect(await screen.findByText('knowledge.md')).toBeInTheDocument();
  });

  it('keeps asking for files on every poll tick', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderWorkspace();
    await waitFor(() => expect(getWorkspaceDetails).toHaveBeenCalled());

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    // The periodic tick is a separate call from the initial load, so it can
    // regress to `includeFiles: false` on its own — which would silently stop
    // surfacing files a running agent writes while the page stays open.
    expect(getWorkspaceDetails).toHaveBeenCalledTimes(2);
    for (const call of getWorkspaceDetails.mock.calls) {
      expect(call).toEqual(['a', { includeFiles: true }]);
    }
    // The walk's result is still rendered after the tick, not just requested.
    expect(screen.getByRole('button', { name: /artifacts/i })).toHaveTextContent('7');
    // The session is unchanged across both polls, so the page must hydrate it
    // exactly once. Re-hydrating per tick would re-read a 100-message page,
    // re-list the runs and hand the chat fresh message identities every five
    // seconds — the per-poll cost this command's options exist to avoid.
    expect(loadSessionMessagesPage).toHaveBeenCalledTimes(1);
    expect(listRuns).toHaveBeenCalledTimes(1);
  });

  it('clears a stale error banner once a poll recovers', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    getWorkspaceDetails.mockRejectedValueOnce(new Error('backend unavailable'));
    renderWorkspace();

    expect(await screen.findByText('backend unavailable')).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    // A transient failure's banner must not outlive the failure, or it sits
    // on the page for the rest of the session.
    await waitFor(() => expect(screen.queryByText('backend unavailable')).toBeNull());
  });
});

describe('Workspace session hydration', () => {
  it('seeds the queued chips from the details payload on first hydration', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { queuedMessageIds: ['m-2'] })
    );
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([
        userMessage('sess-a', 'm-1', 'first question'),
        userMessage('sess-a', 'm-2', 'queued follow-up'),
      ])
    );

    renderWorkspace();

    expect(await screen.findByText('first question')).toBeInTheDocument();
    // The queue is only re-announced by a live QueuedMessagesDelivered event,
    // so a hydration that drops it leaves a pending message indistinguishable
    // from a delivered one until the next run.
    expect(await screen.findByTitle('Waiting for the agent to pick this up')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Remove queued message' })).toBeInTheDocument();
  });

  it('invites a workspace with no conversation to start one', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { session: null })
    );

    renderWorkspace();

    // A freshly created workspace has no session, so there is nothing to
    // hydrate and nothing to wait for — the page must settle on the invite
    // rather than the hydration placeholder it shows while history lands.
    expect(await screen.findByText('Start a conversation')).toBeInTheDocument();
    expect(screen.queryByText('Loading conversation…')).toBeNull();
    // Nothing to hydrate: the loader must skip its session half rather than
    // dereference the absent session and turn a healthy empty workspace into
    // an error banner.
    expect(loadSessionMessagesPage).not.toHaveBeenCalled();
    expect(document.querySelector(`.${styles.errorBanner}`)).toBeNull();
  });

  it('re-hydrates when a background run has advanced the session', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([userMessage('sess-a', 'm-1', 'first question')])
    );
    renderWorkspace();
    expect(await screen.findByText('first question')).toBeInTheDocument();

    // A run that finished elsewhere (schedule, task, another tab) appends
    // messages and bumps the session's updatedAt; an open page that only
    // hydrates once would show a stale conversation until re-entry.
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { session: sessionFor('a', 20n) })
    );
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([
        userMessage('sess-a', 'm-1', 'first question'),
        userMessage('sess-a', 'm-2', 'answer from the background run'),
      ])
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(await screen.findByText('answer from the background run')).toBeInTheDocument();
  });

  it('leaves a streaming session alone when a poll sees it advance', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([
        userMessage('sess-a', 'm-1', 'first question'),
        streamingAssistantMessage('sess-a', 'm-2'),
      ])
    );
    renderWorkspace();
    expect(await screen.findByText('first question')).toBeInTheDocument();

    // The same call `useAssistantEvents` makes for an assistant delta, which
    // is what marks the session streaming in the store.
    act(() => {
      useAssistantStore.getState().appendDelta('sess-a', 'm-2', 'half an answer so far');
    });
    expect(await screen.findByText('half an answer so far')).toBeInTheDocument();

    // The run writing those deltas also bumps the session row, so a poll
    // landing mid-stream sees an advanced updatedAt every tick. Re-reading
    // the page now would swap the live conversation for the persisted one —
    // which does not yet contain the text being streamed — and hand the chat
    // fresh message identities while it renders.
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { session: sessionFor('a', 20n) })
    );
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([userMessage('sess-a', 'm-1', 'first question')])
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(loadSessionMessagesPage).toHaveBeenCalledTimes(1);
    expect(listRuns).toHaveBeenCalledTimes(1);
    expect(screen.getByText('half an answer so far')).toBeInTheDocument();
  });
});

describe('Workspace navigation', () => {
  it('drops the previous workspace conversation while switching workspaces', async () => {
    let releaseBeta: (details: WorkspaceDetails) => void = () => {};
    const betaDetails = new Promise<WorkspaceDetails>((resolve) => {
      releaseBeta = resolve;
    });

    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'b' ? betaDetails : detailsFor(options)
    );
    loadSessionMessagesPage.mockImplementation(async ({ sessionId }) =>
      messagePage([userMessage(sessionId, `${sessionId}-m1`, `conversation of ${sessionId}`)])
    );

    render(
      <MemoryRouter initialEntries={['/workspace/a']}>
        <GoToBeta />
        <Routes>
          <Route path="/workspace/:workspaceId" element={<Workspace />} />
        </Routes>
      </MemoryRouter>
    );

    expect(await screen.findByText('conversation of sess-a')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));

    // The page instance is reused across workspace→workspace navigation, so
    // `details` still holds workspace a until b's round trip lands. Showing
    // its conversation under b's header would be the wrong workspace's
    // history — and the input bar below it now posts to b.
    expect(await screen.findByText('Loading conversation…')).toBeInTheDocument();
    expect(screen.queryByText('conversation of sess-a')).toBeNull();

    await act(async () => {
      releaseBeta(
        detailsFor({ includeFiles: true }, { workspaceId: 'b', session: sessionFor('b', 10n) })
      );
      await betaDetails;
    });

    expect(await screen.findByText('conversation of sess-b')).toBeInTheDocument();
  });
});
