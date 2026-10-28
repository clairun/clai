import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { PermissionRequest } from '../../generated/bindings';
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
  command: 'cd /srv/app && cargo test',
  segments: [
    { text: 'cd /srv/app', kind: 'simple', suggestedPrefix: 'cd' },
    { text: 'cargo test', kind: 'simple', suggestedPrefix: 'cargo test' },
  ],
} as unknown as PermissionRequest;

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe('FleetRequestPanel', () => {
  it('keeps a long, many-line command out of its decisions; the bubble has it whole', () => {
    const note = '"Loki 8f42532a DONE: errors 348/338/133.\nConfirms alert-config blocker (model switch 10-21 14:11— 18:02) → 10 eval"';
    const long = { ...approval, command: note, segments: [{ text: note, kind: 'simple', suggestedPrefix: note }] } as PermissionRequest;
    const card = { ...emptyCard('ws-1'), requests: { 'approval:perm-1': { kind: 'approval', request: long, at: 1 } as unknown as FleetRequest } };
    render(<FleetRequestPanel scene={requestScene(card, { runs: {}, tasks: {} })!} faces={castFor({}, null)} loud={false} now={10} />);
    const buttons = screen.getAllByRole('button');
    // A chip names the prefix it saves by its first words; the tooltip has it whole.
    expect(buttons.map((b) => b.textContent)).toEqual(['Once', 'Always · "Loki 8f42532a …', 'Never · "Loki 8f42532a …', 'Deny']);
    expect(buttons[1]!.title).toContain('Confirms alert-config blocker');
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
    const request = { kind: 'approval', request: { ...approval, agentName: 'Committer' }, agentName: 'Committer', text: approval.command } as unknown as FleetRequest;
    const card = { ...emptyCard('ws-1'), agents: [{ name: 'Committer', seed: 'def-c' }], requests: { 'approval:perm-1': { ...request, at: 1 } } };
    render(<FleetRequestPanel scene={requestScene(card, { runs: {}, tasks: {} })!} faces={castFor({}, null)} loud={false} now={10} />);
    const section = screen.getByRole('region', { name: 'Committer needs you' });
    expect(within(section).getByLabelText('Command').tagName).toBe('PRE');
    expect(within(section).getByLabelText('Command')).toHaveTextContent('cd /srv/app && cargo test');
    expect(within(section).queryByText('Permission requested')).not.toBeInTheDocument();
    await userEvent.click(within(section).getByRole('button', { name: /Deny all/ }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', { requestId: 'perm-1', decisions: [{ kind: 'denyOnce' }, { kind: 'denyOnce' }] });
  });
});
