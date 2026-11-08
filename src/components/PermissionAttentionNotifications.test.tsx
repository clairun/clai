import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router';

const listeners = vi.hoisted(() => [] as { name: string; handler: (event: { payload: unknown }) => void }[]);
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.push({ name, handler });
    return Promise.resolve(() => {});
  }),
}));

import PermissionAttentionNotifications from './PermissionAttentionNotifications';

const fire = (name: string, payload: unknown) =>
  act(() => {
    for (const listener of listeners.filter((l) => l.name === name)) listener.handler({ payload });
  });

const network = {
  requestId: 'net-1',
  workspaceId: 'ws-2',
  agentId: 'wa-rust',
  agentName: 'Rust Architect',
  host: 'mirror.xn--bcher-kva.example',
  port: 443,
  reason: 'fixtures',
  runHosts: [],
  suggestedWildcard: null,
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
};
const permission = { requestId: 'perm-1', workspaceId: 'ws-2', agentName: 'Committer', command: 'git push', rows: [] };
const grant = { requestId: 'grant-1', workspaceId: 'ws-2', agentName: 'Reader', requestedPath: '/srv/data', requestedAccess: 'read_only' };

const KINDS = [
  ['permissions', permission, /Needs approval to run `git push`/],
  ['path-grants', grant, /Needs filesystem access to `\/srv\/data`/],
  ['network-access', network, /Needs network access to `mirror\.bücher\.example:443`/],
] as const;

const renderAt = async (path: string) => {
  render(
    <MemoryRouter initialEntries={[path]}>
      <PermissionAttentionNotifications />
    </MemoryRouter>,
  );
  await waitFor(() => expect(listeners.length).toBeGreaterThanOrEqual(6));
};

beforeEach(() => {
  listeners.length = 0;
});

describe('PermissionAttentionNotifications', () => {
  it.each(KINDS)('toasts a %s request from another workspace, and drops it once resolved', async (kind, req, body) => {
    await renderAt('/workspace/ws-1');
    fire(`${kind}://request`, req);
    expect(screen.getByText(body)).toBeInTheDocument();

    fire(`${kind}://resolved`, { requestId: req.requestId });
    expect(screen.queryByText(body)).not.toBeInTheDocument();
  });

  it('names a port of this computer as such', async () => {
    await renderAt('/workspace/ws-1');
    fire('network-access://request', { ...network, host: 'localhost', port: 5432 });
    expect(screen.getByText('Needs network access to port 5432 on this computer')).toBeInTheDocument();
  });

  it('toasts nothing for the workspace on screen', async () => {
    await renderAt('/workspace/ws-2');
    fire('network-access://request', network);
    expect(screen.queryByText(/Needs network access/)).not.toBeInTheDocument();
  });

  it('does not toast a request again when it is queued again, until it resolves', async () => {
    await renderAt('/workspace/ws-1');
    fire('network-access://request', network);
    act(() => screen.getByRole('button', { name: 'Dismiss notification' }).click());
    expect(screen.queryByText(/Needs network access/)).not.toBeInTheDocument();

    // A failed “Always” save queues the same request again.
    fire('network-access://request', network);
    expect(screen.queryByText(/Needs network access/)).not.toBeInTheDocument();

    fire('network-access://resolved', { requestId: 'net-1' });
    fire('network-access://request', network);
    expect(screen.getByText(/Needs network access/)).toBeInTheDocument();
  });

  it('drops only the toast whose request resolved', async () => {
    await renderAt('/workspace/ws-1');
    fire('network-access://request', network);
    fire('permissions://request', permission);
    fire('permissions://resolved', { requestId: 'net-1' });
    expect(screen.getAllByRole('button', { name: 'Dismiss notification' })).toHaveLength(2);
  });
});
