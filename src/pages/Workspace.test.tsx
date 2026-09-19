import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Route, Routes } from 'react-router';

import Workspace from './Workspace';
import type { WorkspaceDetails, WorkspaceFileEntry } from '../generated/bindings';
import type { WorkspaceDetailsOptions } from '../workspace/client';

vi.mock('../workspace/client', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../workspace/client')>()),
  getWorkspaceDetails: vi.fn(),
  markWorkspaceOpened: vi.fn(async () => {}),
  getOrCreateWorkspaceSession: vi.fn(async () => ({ workspaceId: 'a', sessionId: 's1' })),
  listWorkspaceDir: vi.fn(async () => []),
}));

vi.mock('../assistant/client', () => ({
  loadSessionMessagesPage: vi.fn(async () => ({ messages: [], hasOlder: false, total: 0n })),
  listRuns: vi.fn(async () => []),
  cancelRun: vi.fn(async () => {}),
  sendMessage: vi.fn(async () => {}),
  deleteQueuedMessage: vi.fn(async () => {}),
  editQueuedMessage: vi.fn(async () => {}),
}));

const workspaceClient = await import('../workspace/client');
const getWorkspaceDetails = vi.mocked(workspaceClient.getWorkspaceDetails);

const MEMORY: WorkspaceFileEntry = {
  path: '.clai/memory/knowledge.md',
  relativePath: '.clai/memory/knowledge.md',
  name: 'knowledge.md',
  viewer: 'markdown',
  size: 128n,
  updatedAt: 1n,
  preview: null,
};

// Stands in for `workspace_get_details`, honouring its one flag: `details_files`
// short-circuits to no memories and a zero artifact count when the caller did
// not ask for files (src-tauri/src/commands/workspace.rs, `details_files`). So a
// poll that stops asking produces an empty workspace here exactly as it does in
// the app.
const detailsFor = (options: WorkspaceDetailsOptions): WorkspaceDetails => ({
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
  session: null,
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
});

const renderWorkspace = () =>
  render(
    <MemoryRouter initialEntries={['/workspace/a']}>
      <Routes>
        <Route path="/workspace/:workspaceId" element={<Workspace />} />
      </Routes>
    </MemoryRouter>
  );

beforeEach(() => {
  getWorkspaceDetails.mockImplementation(async (_workspaceId, options) => detailsFor(options));
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
  });
});
