import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { PermissionRequest, PathGrantRequest } from '../../generated/bindings';
import type { PendingAskUser } from '../../assistant/sessionStore';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import AskUserPanel from '../AskUserPanel/AskUserPanel';
import InlineApprovalCard from '../InlineApprovalCard';
import InlinePathGrantCard from '../InlinePathGrantCard';
import FleetRequestRow from './FleetRequestRow';
import type { FleetRequest } from '../../fleet/fleetModel';

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

const grant = {
  requestId: 'grant-1',
  workspaceId: 'ws-1',
  agentId: null,
  agentName: null,
  requestedPath: '/home/me/.ssh',
  requestedAccess: 'read_only',
  reason: 'Read the deploy key fingerprint.',
} as unknown as PathGrantRequest;

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe('compact requests (small Fleet cards)', () => {
  it('drop the badge and the collapse button; the full forms keep them', () => {
    const { unmount } = render(
      <>
        <AskUserPanel ask={ask} compact />
        <InlineApprovalCard workspaceId="ws-1" request={approval} compact />
        <InlinePathGrantCard workspaceId="ws-1" request={grant} compact />
      </>
    );
    expect(screen.queryByText('AGENT IS ASKING')).not.toBeInTheDocument();
    expect(screen.queryByText('Permission requested')).not.toBeInTheDocument();
    expect(screen.queryByText('Path access requested')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /collapse/i })).not.toBeInTheDocument();
    unmount();

    render(
      <>
        <AskUserPanel ask={ask} />
        <InlineApprovalCard workspaceId="ws-1" request={approval} />
        <InlinePathGrantCard workspaceId="ws-1" request={grant} />
      </>
    );
    expect(screen.getByText('AGENT IS ASKING')).toBeInTheDocument();
    expect(screen.getByText('Permission requested')).toBeInTheDocument();
    expect(screen.getByText('Path access requested')).toBeInTheDocument();
    expect(screen.getAllByRole('button', { name: /collapse/i })).toHaveLength(3);
    // The full approval still shows the whole command and the prefix fields.
    expect(screen.getByText('cd /srv/app && cargo test')).toBeInTheDocument();
    expect(screen.getAllByLabelText('Save as prefix:')).toHaveLength(2);
  });

  it('shows each command part once, on its own line with its decisions', () => {
    render(<InlineApprovalCard workspaceId="ws-1" request={approval} compact />);
    expect(screen.queryByText('cd /srv/app && cargo test')).not.toBeInTheDocument();
    expect(screen.getAllByText('cd /srv/app')).toHaveLength(1);
    expect(screen.getAllByText('cargo test')).toHaveLength(1);
    expect(screen.queryByLabelText('Save as prefix:')).not.toBeInTheDocument();
    expect(screen.getAllByRole('button', { name: 'Once' })).toHaveLength(2);
    expect(screen.getAllByRole('button', { name: 'Never' })[0]).toHaveAttribute(
      'title',
      "Always deny (this agent). Saved on this workspace's own agent"
    );
  });

  it('keeps the path warnings, with the path and access on one line', () => {
    render(<InlinePathGrantCard workspaceId="ws-1" request={grant} compact />);
    expect(screen.getByText(/typically holds credentials or secrets/)).toBeInTheDocument();
    expect(screen.getByText('/home/me/.ssh')).toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Always' })).toBeInTheDocument();
  });

  it('answers a single-select question with one chip, its description in the tooltip', async () => {
    render(<AskUserPanel ask={ask} compact />);
    expect(screen.queryByRole('radio')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Send answer' })).not.toBeInTheDocument();
    const canary = screen.getByRole('button', { name: 'Canary' });
    expect(screen.getByRole('button', { name: 'Production' })).toHaveAttribute('title', 'Freeze until Monday');
    await userEvent.click(canary);
    expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
      request: { pendingId: 'ask-1', answer: 'Canary', selectedOptionIndex: 1, selectedOptionIndexes: null },
    });
  });

  it('toggles multi-select chips and sends them together', async () => {
    render(<AskUserPanel ask={{ ...ask, multiSelect: true }} compact />);
    await userEvent.click(screen.getByRole('button', { name: 'Staging' }));
    await userEvent.click(screen.getByRole('button', { name: 'Production' }));
    expect(screen.getByRole('button', { name: 'Staging' })).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(screen.getByRole('button', { name: 'Send answer' }));
    expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
      request: { pendingId: 'ask-1', answer: 'Staging, Production', selectedOptionIndex: null, selectedOptionIndexes: [0, 2] },
    });
  });

  it("shows who asks only for a subtask, not for the workspace's own session", () => {
    const request = { kind: 'ask', ask: { ...ask, sessionId: 'main' } } as unknown as FleetRequest;
    const live = { runs: {}, tasks: { 'ws-1': { t1: { sessionId: 'sub', assignedAgentDefinitionId: 'd1', assignedToWorkspaceAgentId: 'a1', assignedAgentDisplayName: 'Researcher' } } } } as never;
    const { unmount } = render(<FleetRequestRow requests={[request]} workspaceId="ws-1" live={live} faces={{}} />);
    const section = screen.getByRole('region', { name: 'Needs you' });
    expect(within(section).queryByText('Main')).not.toBeInTheDocument();
    unmount();

    // Several open: the count stays, still without the band.
    const { unmount: unmountTwo } = render(
      <FleetRequestRow requests={[request, request]} workspaceId="ws-1" live={live} faces={{}} />
    );
    const two = screen.getByRole('region', { name: 'Needs you' });
    expect(within(two).getByText('1 of 2')).toBeInTheDocument();
    expect(within(two).queryByText('Main')).not.toBeInTheDocument();
    unmountTwo();

    const fromTask = { kind: 'ask', ask: { ...ask, sessionId: 'sub' } } as unknown as FleetRequest;
    render(<FleetRequestRow requests={[fromTask]} workspaceId="ws-1" live={live} faces={{}} />);
    expect(within(screen.getByRole('region', { name: 'Needs you' })).getByText('Researcher')).toBeInTheDocument();
  });
});
