import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { NetworkAccessRequest, PermissionRequest } from '../../generated/bindings';
import type { PendingAskUser } from '../../assistant/sessionStore';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

import FleetRequestPanel from './FleetRequestPanel';
import { castFor } from './FleetFace';
import { emptyCard, type FleetRequest } from '../../fleet/fleetModel';
import { requestScene } from '../../fleet/fleetCrew';

const ask: PendingAskUser = {
  pendingId: 'ask-1',
  question: 'Where should I deploy?',
  extraContext: 'Staging was rebuilt yesterday.',
  options: [
    { label: 'Staging', description: 'Rebuilt yesterday' },
    { label: 'Canary', description: null },
    { label: 'Production', description: 'Freeze until Monday' },
  ],
};

const approval = {
  requestId: 'perm-1',
  workspaceId: 'ws-1',
  agentId: null,
  agentName: null,
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
  command: 'cd /srv/app && git push',
  rows: [{ text: 'git push', rule: 'git push' }],
} as unknown as PermissionRequest;

const submittedPermissions = () =>
  mockInvoke.mock.calls.filter(([cmd]) => cmd === 'submit_permission_decision').map(([, args]) => args);

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe('FleetRequestPanel', () => {
  it('keeps a long, many-line command out of its decisions; the bubble has it whole', () => {
    const note = '"Loki 8f42532a DONE: errors 348/338/133.\nConfirms alert-config blocker (model switch 10-21 14:11— 18:02) → 10 eval"';
    const long = { ...approval, command: note, rows: [{ text: note, rule: 'git push' }] } as PermissionRequest;
    const card = { ...emptyCard('ws-1'), requests: { 'approval:perm-1': { kind: 'approval', request: long, at: 1 } as unknown as FleetRequest } };
    render(<FleetRequestPanel scene={requestScene(card, { runs: {}, tasks: {} })!} faces={castFor({}, null)} loud={false} now={10} />);
    const buttons = screen.getAllByRole('button');
    expect(buttons.map((b) => b.textContent)).toEqual(['Once', 'Always', 'Deny']);
    expect(screen.getByLabelText('Command').textContent).toBe(note);
  });

  it('draws the request as the asking agent: Main for the workspace, the task\'s agent for a subtask', () => {
    const live = { runs: {}, tasks: { 'ws-1': { t1: { sessionId: 'sub', assignedAgentDefinitionId: 'd1', assignedToWorkspaceAgentId: 'a1', assignedAgentDisplayName: 'Researcher' } } } } as never;
    const panel = (...requests: FleetRequest[]) => {
      const card = { ...emptyCard('ws-1'), requests: Object.fromEntries(requests.map((r, i) => [`ask:${i}`, { ...r, at: i }])) };
      return <FleetRequestPanel scene={requestScene(card, live)!} faces={castFor({}, null)} loud={false} now={10} />;
    };
    const fromMain = { kind: 'ask', ask: { ...ask, sessionId: 'main' } } as unknown as FleetRequest;
    const { unmount } = render(panel(fromMain));
    const section = screen.getByRole('region', { name: 'Main needs you' });
    // The question is Main's bubble, and the answer is still the chat's own.
    expect(within(section).getByText('Main')).toBeInTheDocument();
    expect(within(section).getByText('Where should I deploy?')).toBeInTheDocument();
    expect(within(section).getByRole('dialog', { name: 'Agent question' })).toBeInTheDocument();
    expect(within(section).queryByText('AGENT IS ASKING')).not.toBeInTheDocument();
    expect(section).toHaveTextContent(/^Waiting on you/);
    unmount();

    const { unmount: unmountTwo } = render(panel(fromMain, fromMain));
    expect(within(screen.getByRole('region', { name: 'Main needs you' })).getByText('1 of 2')).toBeInTheDocument();
    unmountTwo();

    const fromTask = { kind: 'ask', ask: { ...ask, sessionId: 'sub' } } as unknown as FleetRequest;
    render(panel(fromTask));
    expect(within(screen.getByRole('region', { name: 'Researcher needs you' })).getByText('Researcher')).toBeInTheDocument();
  });

  it('says a command in monospace in the bubble, without the approval card\'s tag, and still decides it', async () => {
    const request = { kind: 'approval', request: { ...approval, agentId: 'wa-c' }, text: approval.command } as unknown as FleetRequest;
    const card = { ...emptyCard('ws-1'), agents: [{ id: 'wa-c', name: 'Committer', seed: 'def-c' }], requests: { 'approval:perm-1': { ...request, at: 1 } } };
    render(<FleetRequestPanel scene={requestScene(card, { runs: {}, tasks: {} })!} faces={castFor({}, null)} loud={false} now={10} />);
    const section = screen.getByRole('region', { name: 'Committer needs you' });
    expect(within(section).getByLabelText('Command').tagName).toBe('PRE');
    expect(within(section).getByLabelText('Command')).toHaveTextContent('cd /srv/app && git push');
    expect(within(section).queryByText('Permission requested')).not.toBeInTheDocument();
    // The Fleet card has no room for a reason: its Deny chip answers in one click, with none.
    expect(within(section).queryByRole('textbox')).not.toBeInTheDocument();
    await userEvent.click(within(section).getByRole('button', { name: 'Deny' }));
    expect(submittedPermissions()).toEqual([{ requestId: 'perm-1', decision: { kind: 'deny' } }]);
  });

  it('answers a network request with the network card, naming the workspace', async () => {
    const network: NetworkAccessRequest = {
      requestId: 'net-1',
      workspaceId: 'ws-1',
      agentId: 'wa-c',
      agentName: null,
      host: 'mirror.example.org',
      port: 443,
      reason: 'Download a fixture',
      runHosts: [],
      suggestedWildcard: '*.example.org',
      persistsToSharedAgent: false,
      alsoAffectsWorkspaces: [],
    };
    const card = {
      ...emptyCard('ws-1'),
      agents: [{ id: 'wa-c', name: 'Committer', seed: 'def-c' }],
      requests: { 'network:net-1': { kind: 'network', request: network, at: 1 } as unknown as FleetRequest },
    };
    render(
      <FleetRequestPanel
        scene={requestScene(card, { runs: {}, tasks: {} })!}
        faces={castFor({}, null)}
        loud={false}
        now={10}
        workspaceTitle="Alpha"
      />,
    );
    const section = screen.getByRole('region', { name: 'Committer needs you' });
    expect(within(section).getByText('mirror.example.org:443')).toBeInTheDocument();
    expect(within(section).getByText(/add their entry/)).toHaveTextContent(/allowed hosts in “Alpha” only\.$/);
    expect(within(section).getAllByRole('button').map((b) => b.textContent)).toEqual(['This run', 'Always', '*.example.org', 'Deny']);
    await userEvent.click(within(section).getByRole('button', { name: 'This run' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_network_access_decision', {
      requestId: 'net-1',
      decision: { kind: 'allowRun' },
    });
  });

  it('names the workspace in the Always allow line', () => {
    const request = { kind: 'approval', request: { ...approval, agentId: 'wa-c' }, text: approval.command } as unknown as FleetRequest;
    const card = { ...emptyCard('ws-1'), agents: [{ id: 'wa-c', name: 'Committer', seed: 'def-c' }], requests: { 'approval:perm-1': { ...request, at: 1 } } };
    render(
      <FleetRequestPanel
        scene={requestScene(card, { runs: {}, tasks: {} })!}
        faces={castFor({}, null)}
        loud={false}
        now={10}
        workspaceTitle="Alpha"
      />
    );
    expect(screen.getByText(/^Always allow removes/)).toHaveTextContent(
      /^Always allow removes git push from this agent's ask-first list in “Alpha” only\.$/
    );
  });

});
