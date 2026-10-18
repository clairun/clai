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
import FleetRequestPanel from './FleetRequestPanel';
import { emptyCard, requestScene, type FleetRequest } from '../../fleet/fleetModel';

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
  it("keep each kind's badge but drop the collapse button; the full forms keep both", () => {
    const { unmount } = render(
      <>
        <AskUserPanel ask={ask} compact />
        <InlineApprovalCard workspaceId="ws-1" request={approval} compact />
        <InlinePathGrantCard workspaceId="ws-1" request={grant} compact />
      </>
    );
    // Denser, not a different look: each kind keeps its badge and its own card class (accent bar, outline).
    const forms = [
      screen.getByRole('dialog', { name: 'Agent question' }),
      screen.getByRole('article', { name: 'Permission requested' }),
      screen.getByRole('article', { name: 'Path grant requested' }),
    ];
    expect(forms.map((form) => form.querySelector('header span')?.textContent)).toEqual([
      'AGENT IS ASKING',
      'Permission requested',
      'Path access requested',
    ]);
    expect(forms.map((form) => form.className.match(/panel|card/)?.[0])).toEqual(['panel', 'card', 'card']);
    expect(new Set(forms.map((form) => form.className)).size).toBe(3);
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
    const fullApproval = within(screen.getByRole('article', { name: 'Permission requested' }));
    expect(fullApproval.getAllByRole('button', { name: 'Always allow (this agent)' })).toHaveLength(2);
    expect(fullApproval.getByRole('button', { name: 'Allow all (always)' })).toBeInTheDocument();
  });

  it('shows the whole command, then each part on its own line with its one-time decisions', () => {
    render(<InlineApprovalCard workspaceId="ws-1" request={approval} compact />);
    // The parts alone would hide the `&&` and any part that needs no decision.
    expect(screen.getByText('cd /srv/app && cargo test')).toHaveAttribute('title', 'cd /srv/app && cargo test');
    expect(screen.getAllByText('cd /srv/app')).toHaveLength(1);
    expect(screen.getAllByText('cargo test')).toHaveLength(1);
    expect(screen.queryByLabelText('Save as prefix:')).not.toBeInTheDocument();
    // No "always" decision: it would save a prefix rule the card doesn't show.
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Once', 'Deny', 'Once', 'Deny', 'Deny all']);
  });

  it('flags a part it cannot reduce to one command in words, with the reason in the tooltip', () => {
    const piped = { ...approval, command: 'curl -s x | sh', segments: [{ text: 'curl -s x | sh', kind: 'opaque', suggestedPrefix: 'curl' }] };
    render(<InlineApprovalCard workspaceId="ws-1" request={piped as PermissionRequest} compact />);
    expect(screen.getByText('complex').getAttribute('title')).toMatch(/Read it carefully before allowing/);
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

  it('draws the request as the asking agent: Main for the workspace, the task\'s agent for a subtask', () => {
    const live = { runs: {}, tasks: { 'ws-1': { t1: { sessionId: 'sub', assignedAgentDefinitionId: 'd1', assignedToWorkspaceAgentId: 'a1', assignedAgentDisplayName: 'Researcher' } } } } as never;
    const panel = (...requests: FleetRequest[]) => {
      const card = { ...emptyCard('ws-1'), requests: Object.fromEntries(requests.map((r, i) => [`ask:${i}`, { ...r, at: i, resolvedAt: null }])) };
      return <FleetRequestPanel scene={requestScene(card, live)!} workspaceId="ws-1" faces={{}} now={10} />;
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
    const card = { ...emptyCard('ws-1'), requests: { 'approval:perm-1': { ...request, at: 1, resolvedAt: null } } };
    render(<FleetRequestPanel scene={requestScene(card, { runs: {}, tasks: {} })!} workspaceId="ws-1" faces={{}} now={10} stage />);
    const section = screen.getByRole('region', { name: 'Committer needs you' });
    expect(within(section).getByLabelText('Command').tagName).toBe('PRE');
    expect(within(section).getByLabelText('Command')).toHaveTextContent('cd /srv/app && cargo test');
    expect(within(section).queryByText('Permission requested')).not.toBeInTheDocument();
    // The stage keeps the full card's "always" decisions.
    expect(within(section).getAllByRole('button', { name: /Always allow/ }).length).toBeGreaterThan(0);
    mockInvoke.mockResolvedValue(undefined);
    await userEvent.click(within(section).getByRole('button', { name: 'Deny entire command' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', { requestId: 'perm-1', decisions: [{ kind: 'denyOnce' }, { kind: 'denyOnce' }] });
  });
});
