import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Outlet, Route, Routes, useNavigate } from 'react-router';

import Workspace from './Workspace';
import styles from './Workspace.module.css';
import useAssistantStore from '../assistant/sessionStore';
import { useAssistantEvents } from '../assistant/useAssistantEvents';
import { useTaskStore } from '../workspace/taskStore';
import type {
  AssistantMessage,
  AssistantMessagePage,
  AssistantRun,
  AssistantSession,
  WorkspaceDetails,
  WorkspaceAgentResponse,
  WorkspaceDetailsOptions,
  WorkspaceFileEntry,
  WorkspaceListEntry,
  WorkspaceTaskResponse,
} from '../generated/bindings';

// The chat panel mounts the inline approval / path-grant cards, which
// subscribe to Tauri events on mount. Without a window bridge those
// subscriptions reject; stub the boundary so the page can render its
// conversation under jsdom.
// Handlers are kept per event name so a test can emit an `assistant://event`.
type EventHandler = (event: { payload: unknown }) => void;
const eventHandlers = vi.hoisted(() => new Map<string, Set<EventHandler>>());
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name: string, handler: EventHandler) => {
    const handlers = eventHandlers.get(name) ?? new Set<EventHandler>();
    handlers.add(handler);
    eventHandlers.set(name, handlers);
    return () => handlers.delete(handler);
  }),
}));

vi.mock('../workspace/client', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../workspace/client')>()),
  getWorkspaceDetails: vi.fn(),
  markWorkspaceOpened: vi.fn(async () => {}),
  getOrCreateWorkspaceSession: vi.fn(),
  listWorkspaceDir: vi.fn(async () => []),
  readWorkspaceFile: vi.fn(),
  runWorkspaceNow: vi.fn(async () => {}),
  setWorkspaceSchedulePaused: vi.fn(async () => {}),
  importWorkspaceFiles: vi.fn(async () => []),
  getWorkspaceTask: vi.fn(async () => null),
}));

vi.mock('../api/client', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../api/client')>()),
  workspaceDeleteAgent: vi.fn(async () => {}),
}));

vi.mock('../assistant/client', () => ({
  getSession: vi.fn(async () => null),
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
const readWorkspaceFile = vi.mocked(workspaceClient.readWorkspaceFile);
const runWorkspaceNow = vi.mocked(workspaceClient.runWorkspaceNow);
const setWorkspaceSchedulePaused = vi.mocked(workspaceClient.setWorkspaceSchedulePaused);
const importWorkspaceFiles = vi.mocked(workspaceClient.importWorkspaceFiles);
const workspaceDeleteAgent = vi.mocked((await import('../api/client')).workspaceDeleteAgent);
// Taken through `vi.mocked` on the real module, so the fixtures below are
// checked against the command's actual return types: a page missing
// `toolCalls`/`nextCursor`/`hasMore`/`totalCount` — all four read by
// `loadDetails` — stops compiling instead of feeding `undefined` into the store.
const assistantClient = await import('../assistant/client');
const loadSessionMessagesPage = vi.mocked(assistantClient.loadSessionMessagesPage);
const listRuns = vi.mocked(assistantClient.listRuns);
const cancelRun = vi.mocked(assistantClient.cancelRun);

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
  context: {
    cliSessionId: null,
    cliSessionProvider: null,
    automationId: null,
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
  origin: { kind: 'user' },
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
  origin: { kind: 'user' },
});

const runFor = (sessionId: string, status: AssistantRun['status']): AssistantRun => ({
  id: 'run-1',
  sessionId,
  status,
  trigger: 'user_message',
  connectionId: 'conn',
  protocolId: 'proto',
  modelId: 'model',
  startedAt: 1n,
  completedAt: null,
  error: null,
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

const GoToAlpha = () => {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate('/workspace/a')}>
      go to alpha
    </button>
  );
};

// The rail's workspace list, as FleetLayout hands it to the page.
const LISTED_WORKSPACES = [
  { id: 'a', title: 'Alpha' },
  { id: 'b', title: 'Beta' },
] as WorkspaceListEntry[];

const FleetOutlet = () => (
  <Outlet context={{ workspaces: LISTED_WORKSPACES, loadWorkspaces: async () => {} }} />
);

const renderSwitchableWorkspace = () =>
  render(
    <MemoryRouter initialEntries={['/workspace/a']}>
      <GoToAlpha />
      <GoToBeta />
      <Routes>
        <Route element={<FleetOutlet />}>
          <Route path="/workspace/:workspaceId" element={<Workspace />} />
        </Route>
      </Routes>
    </MemoryRouter>
  );

// Resolves `heldId`'s details only when the returned `release` is called;
// every other workspace answers at once through `resolveOther`.
const holdDetails = (
  heldId: string,
  heldValue: WorkspaceDetails,
  resolveOther: (workspaceId: string, options: WorkspaceDetailsOptions) => WorkspaceDetails = (
    _workspaceId,
    options
  ) => detailsFor(options)
) => {
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  getWorkspaceDetails.mockImplementation(async (workspaceId, options) => {
    if (workspaceId !== heldId) return resolveOther(workspaceId, options);
    await held;
    return heldValue;
  });
  return async () => {
    await act(async () => {
      release();
      await held;
    });
  };
};

const holdBetaDetails = (beta: WorkspaceDetails) => holdDetails('b', beta);

const FAILED_TASK: WorkspaceTaskResponse = {
  id: 'task-a',
  workspaceId: 'a',
  createdByWorkspaceAgentId: null,
  createdByDisplayName: null,
  assignedToWorkspaceAgentId: 'wa-1',
  assignedAgentDefinitionId: 'agent-1',
  assignedAgentDisplayName: 'Helper',
  title: 'Alpha task',
  instructions: 'Do the alpha thing',
  status: 'failed',
  resultSummary: null,
  error: 'Alpha broke',
  sessionId: null,
  runId: 'run-1',
  createdAt: 1n,
  updatedAt: 1n,
  completedAt: 1n,
  failureReason: null,
  parentSessionId: null,
  parentRunId: null,
  parentTaskId: null,
  depth: 0n,
  cycleNo: 0n,
  stopRequestedAt: null,
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
  useTaskStore.setState({ tasks: {}, seeded: {} });
  getWorkspaceDetails.mockImplementation(async (_workspaceId, options) => detailsFor(options));
  getOrCreateWorkspaceSession.mockResolvedValue({
    session: sessionFor('a', 10n),
    providerConnectionId: 'conn-1',
  });
  loadSessionMessagesPage.mockResolvedValue(messagePage([]));
  listRuns.mockResolvedValue([]);
  readWorkspaceFile.mockRejectedValue(new Error('File not found'));
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
    listRuns.mockResolvedValue([runFor('sess-a', 'running')]);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(loadSessionMessagesPage).toHaveBeenCalledTimes(1);
    expect(listRuns).toHaveBeenCalledTimes(2);
    expect(screen.getByText('half an answer so far')).toBeInTheDocument();
  });

  it('re-hydrates a streaming session whose run ended without a terminal event', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([userMessage('sess-a', 'm-1', 'first question')])
    );
    renderWorkspace();
    expect(await screen.findByText('first question')).toBeInTheDocument();

    act(() => {
      useAssistantStore.getState().setRunStatus('sess-a', runFor('sess-a', 'running'));
    });
    expect(useAssistantStore.getState().sessions['sess-a']!.runStartedAt).not.toBeNull();

    // The run finished but its run_completed event never reached the page.
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { session: sessionFor('a', 20n) })
    );
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([
        userMessage('sess-a', 'm-1', 'first question'),
        userMessage('sess-a', 'm-2', 'the finished answer'),
      ])
    );
    listRuns.mockResolvedValue([runFor('sess-a', 'completed')]);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(await screen.findByText('the finished answer')).toBeInTheDocument();
    const session = useAssistantStore.getState().sessions['sess-a']!;
    expect(session.isStreaming).toBe(false);
    expect(session.runStartedAt).toBeNull();
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

describe('Workspace switch chrome', () => {
  const beta = detailsFor(
    { includeFiles: true },
    { workspaceId: 'b', title: 'Beta', memories: [], artifactCount: 0n, session: sessionFor('b', 10n) }
  );

  it("keeps the previous workspace's title and run controls out of the header", async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, {
        runs: [
          {
            id: 'run-a',
            sessionId: 'sess-a',
            status: 'running',
            trigger: 'user_message',
            connectionId: 'conn-1',
            protocolId: 'p',
            modelId: 'm',
            startedAt: 1n,
            completedAt: null,
            error: null,
          },
        ],
      })
    );
    renderSwitchableWorkspace();
    expect(await screen.findByText('Alpha', { selector: 'h1' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop current run' })).toBeInTheDocument();

    const releaseBeta = holdBetaDetails(beta);
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));

    expect(await screen.findByText('Loading conversation…')).toBeInTheDocument();
    expect(screen.queryByText('Alpha', { selector: 'h1' })).toBeNull();
    // The rail already knows b's title, so the header names it at once.
    expect(screen.getByText('Beta', { selector: 'h1' })).not.toHaveAttribute('title');
    // Stop (and Ctrl+C) would cancel a's run from b's page.
    expect(screen.queryByRole('button', { name: 'Stop current run' })).toBeNull();

    await releaseBeta();
    expect(await screen.findByTitle('Click to rename')).toHaveTextContent('Beta');
  });

  it('does not show zero tasks before the new workspace task count loads', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [FAILED_TASK] })
    );
    renderSwitchableWorkspace();
    expect(await screen.findByRole('button', { name: /1 tasks/ })).toBeInTheDocument();

    const betaTasks = [
      { ...FAILED_TASK, id: 'task-b-1', workspaceId: 'b' },
      { ...FAILED_TASK, id: 'task-b-2', workspaceId: 'b' },
    ];
    const releaseBeta = holdBetaDetails({ ...beta, tasks: betaTasks });
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByText('Loading conversation…');

    expect(screen.getByRole('button', { name: /… tasks/ })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /0 tasks/ })).toBeNull();

    await releaseBeta();
    expect(await screen.findByRole('button', { name: /2 tasks/ })).toBeInTheDocument();
  });

  it("keeps the previous workspace's counts off the page", async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [FAILED_TASK] })
    );
    renderSwitchableWorkspace();
    expect(await screen.findByRole('button', { name: /1 tasks/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /1 memories/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /7 artifacts/ })).toBeInTheDocument();

    const releaseBeta = holdBetaDetails(beta);
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByText('Loading conversation…');

    expect(screen.getByRole('button', { name: /… tasks/ })).toBeInTheDocument();
    expect(screen.getByText('… msgs')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /… agents/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /… memories/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /… artifacts/ })).toBeInTheDocument();

    await releaseBeta();
    expect(await screen.findByTitle('Click to rename')).toHaveTextContent('Beta');
    expect(screen.getByText('0 msgs')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /0 agents/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /0 tasks/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /0 memories/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /0 artifacts/ })).toBeInTheDocument();
  });

  it('drops a late reply for the workspace the user already left', async () => {
    const releaseAlpha = holdDetails(
      'a',
      detailsFor({ includeFiles: true }, { tasks: [FAILED_TASK] }),
      () => beta
    );
    renderSwitchableWorkspace();
    await screen.findByText('Loading conversation…');

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    expect(await screen.findByTitle('Click to rename')).toHaveTextContent('Beta');
    expect(screen.queryByText('Loading conversation…')).toBeNull();

    await releaseAlpha();

    expect(screen.getByTitle('Click to rename')).toHaveTextContent('Beta');
    expect(screen.queryByText('Loading conversation…')).toBeNull();
    expect(loadSessionMessagesPage).not.toHaveBeenCalledWith(
      expect.objectContaining({ sessionId: 'sess-a' })
    );
  });

  it("holds a remembered drawer open on loading instead of the previous workspace's list", async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'b' ? beta : detailsFor(options)
    );
    renderSwitchableWorkspace();
    await screen.findByText('Alpha', { selector: 'h1' });

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByText('Beta', { selector: 'h1' });
    await userEvent.click(screen.getByRole('button', { name: /0 memories/ }));
    await userEvent.click(screen.getByRole('button', { name: 'go to alpha' }));
    await screen.findByText('Alpha', { selector: 'h1' });

    const releaseBeta = holdBetaDetails(beta);
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));

    const drawer = await screen.findByRole('complementary', { name: 'memories drawer' });
    expect(drawer).toHaveTextContent('Loading…');
    expect(screen.queryByText(MEMORY.name)).toBeNull();

    await releaseBeta();
    expect(await screen.findByText(/hasn't stored anything in memory yet/)).toBeInTheDocument();
  });
});

// A promise the test settles by hand, for holding one call in flight.
const deferred = <T,>() => {
  let resolve: (value: T) => void = () => {};
  let reject: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

const ACTIVE_RUN = {
  id: 'run-a',
  sessionId: 'sess-a',
  status: 'running',
  trigger: 'user_message',
  connectionId: 'conn-1',
  protocolId: 'p',
  modelId: 'm',
  startedAt: 1n,
  completedAt: null,
  error: null,
} as WorkspaceDetails['runs'][number];

describe('Workspace switch with a call in flight', () => {
  const scheduledDetails = (workspaceId: string, options: WorkspaceDetailsOptions) =>
    detailsFor(
      options,
      workspaceId === 'b'
        ? { workspaceId: 'b', title: 'Beta', session: sessionFor('b', 10n), scheduleEnabled: true }
        : { scheduleEnabled: true }
    );

  it("keeps the previous workspace's pending Run now and Pause off the new one", async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      scheduledDetails(workspaceId, options)
    );
    const alphaRun = deferred<void>();
    const alphaPause = deferred<void>();
    const betaRun = deferred<void>();
    const betaPause = deferred<void>();
    runWorkspaceNow.mockImplementation((workspaceId) =>
      workspaceId === 'a' ? alphaRun.promise : betaRun.promise
    );
    setWorkspaceSchedulePaused.mockImplementation((workspaceId) =>
      workspaceId === 'a' ? alphaPause.promise : betaPause.promise
    );
    renderSwitchableWorkspace();
    await userEvent.click(await screen.findByRole('button', { name: 'Run now' }));
    expect(screen.getByRole('button', { name: 'Run now' })).toBeDisabled();
    // The optimistic pause swaps a's Pause for Resume and hides Run now.
    await userEvent.click(screen.getByRole('button', { name: 'Pause schedule' }));
    expect(screen.getByRole('button', { name: 'Resume schedule' })).toBeDisabled();

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByTitle('Click to rename');

    const runNow = screen.getByRole('button', { name: 'Run now' });
    expect(runNow).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Pause schedule' })).toBeEnabled();
    await userEvent.click(runNow);
    expect(runWorkspaceNow).toHaveBeenCalledWith('b');

    // a's calls settling must not release b's own pending ones.
    await act(async () => {
      alphaRun.resolve();
    });
    expect(screen.getByRole('button', { name: 'Run now' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Pause schedule' }));
    await act(async () => {
      alphaPause.resolve();
    });
    expect(screen.getByRole('button', { name: 'Resume schedule' })).toBeDisabled();

    await act(async () => {
      betaRun.resolve();
      betaPause.resolve();
    });
  });

  it('keeps Run now enabled while a task only waits for its subtasks', async () => {
    const task = { ...FAILED_TASK, status: 'running' as const, completedAt: null, error: null };
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { scheduleEnabled: true, tasks: [{ ...task, runId: null }] })
    );
    const { unmount } = renderWorkspace();
    expect(await screen.findByRole('button', { name: 'Run now' })).toBeEnabled();
    unmount();

    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { scheduleEnabled: true, tasks: [task] })
    );
    renderWorkspace();
    expect(await screen.findByRole('button', { name: 'Run now' })).toBeDisabled();
  });

  it('still shows a pending Run now and Pause on returning to their workspace', async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      scheduledDetails(workspaceId, options)
    );
    const alphaRun = deferred<void>();
    const alphaPause = deferred<void>();
    const betaRun = deferred<void>();
    runWorkspaceNow.mockImplementation((workspaceId) =>
      workspaceId === 'a' ? alphaRun.promise : betaRun.promise
    );
    setWorkspaceSchedulePaused.mockImplementation(() => alphaPause.promise);
    renderSwitchableWorkspace();
    await userEvent.click(await screen.findByRole('button', { name: 'Run now' }));
    await userEvent.click(screen.getByRole('button', { name: 'Pause schedule' }));

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByText('Beta');
    await userEvent.click(screen.getByRole('button', { name: 'Run now' }));
    await userEvent.click(screen.getByRole('button', { name: 'go to alpha' }));
    await screen.findByText('Alpha');

    // a's reload reports the schedule unpaused, so both controls are back.
    expect(await screen.findByRole('button', { name: 'Run now' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Pause schedule' })).toBeDisabled();

    await act(async () => {
      alphaRun.resolve();
      alphaPause.resolve();
      betaRun.resolve();
    });
  });

  it("keeps the previous workspace's pending import off the new one", async () => {
    const alphaImport = deferred<string[]>();
    importWorkspaceFiles.mockImplementation((workspaceId) =>
      workspaceId === 'a' ? alphaImport.promise : Promise.resolve([])
    );
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'b'
        ? detailsFor(options, { workspaceId: 'b', title: 'Beta', session: sessionFor('b', 10n) })
        : detailsFor(options)
    );
    renderSwitchableWorkspace();
    const addFiles = 'Add files or folders to the workspace';
    await userEvent.click(await screen.findByRole('button', { name: /artifacts/i }));
    await userEvent.click(screen.getByRole('button', { name: addFiles }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Files…' }));
    expect(screen.getByRole('button', { name: addFiles })).toHaveAttribute('aria-disabled', 'true');

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByTitle('Click to rename');
    await userEvent.click(screen.getByRole('button', { name: /artifacts/i }));
    expect(screen.getByRole('button', { name: addFiles })).not.toHaveAttribute('aria-disabled');

    await act(async () => {
      alphaImport.reject(new Error('alpha import failed'));
    });

    expect(screen.queryByText('alpha import failed')).toBeNull();
    expect(document.querySelector(`.${styles.errorBanner}`)).toBeNull();
  });

  it("keeps the previous workspace's pending agent removal off the new one", async () => {
    const helper = {
      id: 'wa-1',
      agentDefinitionId: 'agent-1',
      displayName: 'Helper',
      enabled: true,
      isDefault: false,
      agentName: null,
      agentDescription: null,
    } as WorkspaceAgentResponse;
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      detailsFor(
        options,
        workspaceId === 'b'
          ? {
              workspaceId: 'b',
              title: 'Beta',
              session: sessionFor('b', 10n),
              assignedAgents: [{ ...helper, workspaceId: 'b' }],
            }
          : { assignedAgents: [{ ...helper, workspaceId: 'a' }] }
      )
    );
    const alphaRemove = deferred<void>();
    workspaceDeleteAgent.mockImplementation((workspaceId) =>
      workspaceId === 'a' ? alphaRemove.promise : Promise.resolve()
    );
    renderSwitchableWorkspace();
    const removeHelper = 'Remove Helper from the crew';
    await userEvent.click(await screen.findByRole('button', { name: /agents/i }));
    await userEvent.click(await screen.findByRole('button', { name: removeHelper }));
    expect(screen.getByRole('button', { name: removeHelper })).toBeDisabled();

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByTitle('Click to rename');
    await userEvent.click(screen.getByRole('button', { name: /agents/i }));
    expect(await screen.findByRole('button', { name: removeHelper })).toBeEnabled();

    await act(async () => {
      alphaRemove.reject(new Error('alpha remove failed'));
    });

    expect(screen.queryByText('alpha remove failed')).toBeNull();
  });

  it("clears the previous workspace's crew error on switch", async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'b'
        ? detailsFor(options, { workspaceId: 'b', title: 'Beta', session: sessionFor('b', 10n) })
        : detailsFor(options, {
            assignedAgents: [
              {
                id: 'wa-1',
                workspaceId: 'a',
                agentDefinitionId: 'agent-1',
                displayName: 'Helper',
                enabled: true,
                isDefault: false,
                agentName: null,
                agentDescription: null,
              } as WorkspaceAgentResponse,
            ],
          })
    );
    workspaceDeleteAgent.mockRejectedValueOnce(new Error('alpha remove failed'));
    renderSwitchableWorkspace();
    await userEvent.click(await screen.findByRole('button', { name: /agents/i }));
    await userEvent.click(
      await screen.findByRole('button', { name: 'Remove Helper from the crew' })
    );
    expect(await screen.findByText('alpha remove failed')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByTitle('Click to rename');
    await userEvent.click(screen.getByRole('button', { name: /agents/i }));

    expect(screen.queryByText('alpha remove failed')).toBeNull();
  });

  it("keeps a failed Stop from the previous workspace from releasing the new one's", async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'b'
        ? detailsFor(options, {
            workspaceId: 'b',
            title: 'Beta',
            session: sessionFor('b', 10n),
            runs: [{ ...ACTIVE_RUN, id: 'run-b', sessionId: 'sess-b' }],
          })
        : detailsFor(options, { runs: [ACTIVE_RUN] })
    );
    const alphaStop = deferred<never>();
    cancelRun.mockImplementation((_workspaceId, runId) =>
      runId === 'run-a' ? alphaStop.promise : new Promise<never>(() => {})
    );
    renderSwitchableWorkspace();
    await userEvent.click(await screen.findByRole('button', { name: 'Stop current run' }));
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByTitle('Click to rename');
    await userEvent.click(await screen.findByRole('button', { name: 'Stop current run' }));
    expect(screen.getByRole('button', { name: 'Stop current run' })).toBeDisabled();

    await act(async () => {
      alphaStop.reject(new Error('alpha stop failed'));
    });

    expect(screen.getByRole('button', { name: 'Stop current run' })).toBeDisabled();
  });

  it.each([
    {
      action: 'Run now',
      hold: (call: Promise<never>) => runWorkspaceNow.mockImplementation(() => call),
    },
    {
      action: 'Pause schedule',
      hold: (call: Promise<never>) => setWorkspaceSchedulePaused.mockImplementation(() => call),
    },
    {
      action: 'Stop current run',
      hold: (call: Promise<never>) => cancelRun.mockImplementation(() => call),
    },
  ])(
    "keeps a failed $action from the previous workspace out of the new one's banner",
    async ({ action, hold }) => {
      getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
        workspaceId === 'b'
          ? scheduledDetails(workspaceId, options)
          : detailsFor(options, {
              scheduleEnabled: true,
              runs: action === 'Stop current run' ? [ACTIVE_RUN] : [],
            })
      );
      const alphaCall = deferred<never>();
      hold(alphaCall.promise);
      renderSwitchableWorkspace();
      await userEvent.click(await screen.findByRole('button', { name: action }));
      await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
      await screen.findByTitle('Click to rename');

      await act(async () => {
        alphaCall.reject(new Error('alpha call failed'));
      });

      expect(screen.queryByText('alpha call failed')).toBeNull();
      expect(document.querySelector(`.${styles.errorBanner}`)).toBeNull();
    }
  );

  it.each([
    {
      action: 'Run now',
      hold: (call: Promise<void>) => runWorkspaceNow.mockImplementation(() => call),
    },
    {
      action: 'Pause schedule',
      hold: (call: Promise<void>) => setWorkspaceSchedulePaused.mockImplementation(() => call),
    },
    {
      action: 'Stop current run',
      hold: (call: Promise<void>) =>
        cancelRun.mockImplementation(() => call.then(() => ACTIVE_RUN)),
    },
  ])(
    "keeps a late successful $action from clearing the new workspace's error",
    async ({ action, hold }) => {
      getWorkspaceDetails.mockImplementation(async (workspaceId, options) => {
        if (workspaceId === 'b') throw new Error('beta unavailable');
        return detailsFor(options, {
          scheduleEnabled: true,
          runs: action === 'Stop current run' ? [ACTIVE_RUN] : [],
        });
      });
      const alphaCall = deferred<void>();
      hold(alphaCall.promise);
      renderSwitchableWorkspace();
      await userEvent.click(await screen.findByRole('button', { name: action }));
      await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
      expect(await screen.findByText('beta unavailable')).toBeInTheDocument();

      await act(async () => {
        alphaCall.resolve();
      });

      expect(screen.getByText('beta unavailable')).toBeInTheDocument();
    }
  );

  it("keeps the new workspace's own error when a call from the previous one fails", async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) => {
      if (workspaceId === 'b') throw new Error('beta unavailable');
      return detailsFor(options, { scheduleEnabled: true });
    });
    const alphaRun = deferred<never>();
    runWorkspaceNow.mockImplementation(() => alphaRun.promise);
    renderSwitchableWorkspace();
    await userEvent.click(await screen.findByRole('button', { name: 'Run now' }));
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    expect(await screen.findByText('beta unavailable')).toBeInTheDocument();

    await act(async () => {
      alphaRun.reject(new Error('alpha run failed'));
    });

    expect(screen.getByText('beta unavailable')).toBeInTheDocument();
    expect(screen.queryByText('alpha run failed')).toBeNull();
  });

  it('still shows a pending Stop on returning to its workspace', async () => {
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'b'
        ? detailsFor(options, { workspaceId: 'b', title: 'Beta', session: sessionFor('b', 10n) })
        : detailsFor(options, { runs: [ACTIVE_RUN] })
    );
    cancelRun.mockImplementation(() => new Promise<never>(() => {}));
    renderSwitchableWorkspace();
    await userEvent.click(await screen.findByRole('button', { name: 'Stop current run' }));
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByText('Beta');
    await userEvent.click(screen.getByRole('button', { name: 'go to alpha' }));
    await screen.findByText('Alpha');

    expect(await screen.findByRole('button', { name: 'Stop current run' })).toBeDisabled();
  });

  it("clears the previous workspace's error banner on switch", async () => {
    getWorkspaceDetails.mockRejectedValueOnce(new Error('backend unavailable'));
    renderSwitchableWorkspace();
    expect(await screen.findByText('backend unavailable')).toBeInTheDocument();

    const releaseBeta = holdBetaDetails(
      detailsFor(
        { includeFiles: true },
        { workspaceId: 'b', title: 'Beta', session: sessionFor('b', 10n) }
      )
    );
    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByText('Loading conversation…');

    expect(screen.queryByText('backend unavailable')).toBeNull();

    await releaseBeta();
  });

  it('ignores a failed details load for the workspace the user already left', async () => {
    const alphaDetails = deferred<WorkspaceDetails>();
    getWorkspaceDetails.mockImplementation(async (workspaceId, options) =>
      workspaceId === 'a'
        ? alphaDetails.promise
        : detailsFor(options, { workspaceId: 'b', title: 'Beta', session: sessionFor('b', 10n) })
    );
    // Hold b's history so b is still hydrating when a's load fails.
    const betaMessages = deferred<AssistantMessagePage>();
    loadSessionMessagesPage.mockImplementation(() => betaMessages.promise);
    renderSwitchableWorkspace();
    await screen.findByText('Loading conversation…');

    await userEvent.click(screen.getByRole('button', { name: 'go to beta' }));
    await screen.findByTitle('Click to rename');
    expect(screen.getByText('Loading conversation…')).toBeInTheDocument();

    await act(async () => {
      alphaDetails.reject(new Error('alpha load failed'));
    });

    expect(screen.queryByText('alpha load failed')).toBeNull();
    // b's first load is still in flight: the placeholder must hold rather
    // than flip to the empty-conversation invite.
    expect(screen.getByText('Loading conversation…')).toBeInTheDocument();
    expect(screen.queryByText('Start a conversation')).toBeNull();

    await act(async () => {
      betaMessages.resolve(messagePage([]));
    });
  });
});

describe('Workspace file preview', () => {
  it('opens a link to an unlisted memory file as a memory, without the delete button', async () => {
    readWorkspaceFile.mockImplementation(async (_workspaceId, path) =>
      path === MEMORY.path
        ? { path, viewer: 'markdown', content: '[notes](notes.md)' }
        : { path, viewer: 'markdown', content: '# Notes' }
    );
    renderWorkspace();

    await userEvent.click(await screen.findByRole('button', { name: /1 memories/ }));
    await userEvent.click(await screen.findByText(MEMORY.name));
    await userEvent.click(await screen.findByRole('link', { name: 'notes' }));

    expect(await screen.findByRole('region', { name: 'Memory: notes.md' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Delete file' })).toBeNull();
  });

  it('opens a workspace file linked from chat in the artifact preview', async () => {
    loadSessionMessagesPage.mockResolvedValue(
      messagePage([
        {
          ...streamingAssistantMessage('sess-a', 'm1'),
          content: [{ type: 'text', text: 'Run [bisect.sh](/tmp/a/scratch/bisect.sh)' }],
        },
      ])
    );
    readWorkspaceFile.mockImplementation(async (_workspaceId, path) => ({
      path,
      viewer: 'code',
      content: 'echo bisect',
    }));
    renderWorkspace();

    await userEvent.click(await screen.findByRole('link', { name: 'bisect.sh' }));

    expect(await screen.findByRole('region', { name: 'Artifact: bisect.sh' })).toBeInTheDocument();
    expect(readWorkspaceFile.mock.calls.map((call) => call[1])).toContain('scratch/bisect.sh');
  });
});

describe('Workspace task transcript', () => {
  it('follows the polled task status after the panel opened', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const running: WorkspaceTaskResponse = {
      ...FAILED_TASK,
      status: 'running',
      error: null,
      completedAt: null,
      sessionId: 'sess-task-a',
    };
    let task = running;
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [task] })
    );
    renderWorkspace();

    await userEvent.click(await screen.findByRole('button', { name: /1 tasks/ }));
    await userEvent.click(
      await screen.findByRole('button', { name: `Open the log of "${running.title}"` })
    );
    const panel = await screen.findByRole('region', { name: `Transcript for ${running.title}` });
    expect(panel).toHaveTextContent('Running');

    task = { ...running, status: 'completed', completedAt: 2n, updatedAt: 2n };
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });

    await waitFor(() => expect(panel).toHaveTextContent('Completed'));
    expect(panel).not.toHaveTextContent('Running');
  });

  it('opens the task a route arrives with', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [FAILED_TASK] })
    );
    vi.mocked(workspaceClient.getWorkspaceTask).mockResolvedValueOnce(FAILED_TASK);
    render(
      <MemoryRouter initialEntries={[{ pathname: '/workspace/a', state: { openTaskId: FAILED_TASK.id } }]}>
        <Routes>
          <Route path="/workspace/:workspaceId" element={<Workspace />} />
        </Routes>
      </MemoryRouter>
    );
    await screen.findByRole('region', { name: `Transcript for ${FAILED_TASK.title}` });
  });
});

const AssistantEvents = () => {
  useAssistantEvents();
  return null;
};

describe('Workspace task store', () => {
  const RUNNING: WorkspaceTaskResponse = {
    ...FAILED_TASK,
    status: 'running',
    error: null,
    completedAt: null,
    sessionId: 'sess-task-a',
  };

  beforeEach(() => eventHandlers.clear());

  it('open panel status follows event without poll', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [RUNNING] })
    );
    render(
      <MemoryRouter initialEntries={['/workspace/a']}>
        <AssistantEvents />
        <Routes>
          <Route path="/workspace/:workspaceId" element={<Workspace />} />
        </Routes>
      </MemoryRouter>
    );

    await userEvent.click(await screen.findByRole('button', { name: /1 tasks/ }));
    await userEvent.click(
      await screen.findByRole('button', { name: `Open the log of "${RUNNING.title}"` })
    );
    const panel = await screen.findByRole('region', { name: `Transcript for ${RUNNING.title}` });
    expect(panel).toHaveTextContent('Running');
    const polls = getWorkspaceDetails.mock.calls.length;

    act(() => {
      for (const handler of eventHandlers.get('assistant://event') ?? []) {
        handler({
          payload: {
            sessionId: 'sess-task-a',
            runId: null,
            workspaceId: 'a',
            timestamp: 0,
            event: {
              type: 'task_updated',
              payload: { task: { ...RUNNING, status: 'completed', updatedAt: 2n } },
            },
          },
        });
      }
    });

    expect(panel).toHaveTextContent('Completed');
    expect(getWorkspaceDetails.mock.calls.length).toBe(polls);
  });

  it('first details load shows restart-stopped task without event', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [{ ...RUNNING, status: 'stopped' }] })
    );
    renderWorkspace();

    await userEvent.click(await screen.findByRole('button', { name: /1 tasks/ }));
    expect(await screen.findByText('Stopped')).toBeInTheDocument();
    expect(useTaskStore.getState().tasks.a?.[RUNNING.id]?.status).toBe('stopped');
  });
});

describe('Workspace nested task logs', () => {
  const PARENT: WorkspaceTaskResponse = {
    ...FAILED_TASK,
    id: 'task-parent',
    title: 'Review PR',
    status: 'running',
    error: null,
    sessionId: 'sess-parent',
  };
  const CHILD: WorkspaceTaskResponse = {
    ...PARENT,
    id: 'task-child',
    title: 'Fix parse',
    status: 'completed',
    sessionId: 'sess-child',
    parentTaskId: PARENT.id,
    depth: 1n,
  };
  const childResult: AssistantMessage = {
    id: 'result-1',
    sessionId: 'sess-parent',
    role: 'user',
    content: [{ type: 'text', text: 'parse() fixed' }],
    createdAt: 1n,
    providerMetadata: null,
    origin: {
      kind: 'task',
      taskId: CHILD.id,
      agent: { workspaceAgentId: 'wa-1', displayName: 'Helper', isDefault: false },
      title: CHILD.title,
      cycleNo: 1n,
      status: 'completed',
    },
  };
  const seedLog = (sessionId: string, messages: AssistantMessage[]) =>
    useAssistantStore.setState((state) => ({
      sessions: {
        ...state.sessions,
        [sessionId]: { messages, toolCalls: [], isStreaming: false, runStartedAt: null },
      },
    }) as never);

  it('nested card in task log opens child and back returns', async () => {
    getWorkspaceDetails.mockImplementation(async (_workspaceId, options) =>
      detailsFor(options, { tasks: [PARENT, CHILD] })
    );
    seedLog('sess-parent', [childResult]);
    seedLog('sess-child', [
      {
        id: 'child-1',
        sessionId: 'sess-child',
        role: 'assistant',
        content: [{ type: 'text', text: 'child log' }],
        createdAt: 1n,
        providerMetadata: null,
        origin: { kind: 'user' },
      },
    ]);
    renderWorkspace();

    await userEvent.click(await screen.findByRole('button', { name: /2 tasks/ }));
    await userEvent.click(
      await screen.findByRole('button', { name: `Open the log of "${PARENT.title}"` })
    );
    const parentLog = await screen.findByRole('region', { name: `Transcript for ${PARENT.title}` });
    expect(screen.queryByRole('button', { name: 'Back' })).toBeNull();

    await userEvent.click(within(parentLog).getByRole('button', { name: `Result from Helper: ${CHILD.title}, Completed` }));
    const childLog = await screen.findByRole('region', { name: `Transcript for ${CHILD.title}` });
    expect(childLog).toHaveTextContent('child log');
    expect(
      within(childLog).getByRole('navigation', { name: 'Task ancestry' })
    ).toHaveTextContent(PARENT.title);

    await userEvent.click(within(childLog).getByRole('button', { name: 'Back' }));
    const returnedLog = await screen.findByRole('region', { name: `Transcript for ${PARENT.title}` });
    expect(
      within(returnedLog).getByRole('button', { name: `Result from Helper: ${CHILD.title}, Completed` })
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Back' })).toBeNull();
  });


});
