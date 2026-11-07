import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import InlineApprovalCard, { ApprovalCard } from './InlineApprovalCard';
import InlinePathGrantCard from './InlinePathGrantCard';
import InlineNetworkAccessCard from './InlineNetworkAccessCard';
import AskUserPanel from './AskUserPanel/AskUserPanel';
import AgentAvatar from './Agents/AgentAvatar';
import { identityFor } from './Agents/agentIdentity';
import { rosterAgent } from './AssistantChat/messageSender';
import type {
  NetworkAccessRequest,
  PathGrantRequest,
  PermissionRequest,
  WorkspaceAgentResponse,
  WorkspaceTaskResponse,
} from '../generated/bindings';

const agentRow = (over: Partial<WorkspaceAgentResponse>): WorkspaceAgentResponse => ({
  id: 'wa-x',
  workspaceId: 'ws-1',
  agentDefinitionId: 'def-x',
  displayName: 'X',
  enabled: true,
  isDefault: false,
  agentName: null,
  agentDescription: null,
  providerConnectionIds: [],
  skillIds: [],
  selectedMcpServerIds: [],
  execution: {},
  avatar: null,
  createdAt: 1n,
  updatedAt: 1n,
  ...over,
});

const MAIN = agentRow({
  id: 'wa-main',
  agentDefinitionId: 'def-main',
  displayName: 'Manager',
  isDefault: true,
});
const RUST = agentRow({ id: 'wa-rust', agentDefinitionId: 'def-rust', displayName: 'Rust Architect' });
const roster = [MAIN, RUST];

const permission = (agentId: string | null): PermissionRequest => ({
  requestId: `perm-${agentId}`,
  workspaceId: 'ws-1',
  agentId,
  agentName: null,
  command: 'cargo test',
  rows: [{ text: 'cargo test', rule: 'cargo test' }],
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
});

const grant = (agentId: string | null): PathGrantRequest => ({
  requestId: `grant-${agentId}`,
  workspaceId: 'ws-1',
  agentId,
  agentName: null,
  requestedPath: '/home/u/.config/gh',
  requestedAccess: 'read_only',
  reason: 'Read the gh token',
});

/** The face a card draws, hidden from assistive technology like the task cards' faces. */
const faceOf = (scope: HTMLElement) => scope.querySelector('[data-activity]');
const svgOf = (agent: Parameters<typeof identityFor>[0]) =>
  render(<AgentAvatar identity={identityFor(agent)} size={18} />).container.querySelector('span')?.innerHTML;

const network = (agentId: string | null): NetworkAccessRequest => ({
  requestId: `net-${agentId}`,
  workspaceId: 'ws-1',
  agentId,
  agentName: null,
  host: 'mirror.example.org',
  port: 443,
  reason: 'Download a fixture',
  runHosts: [],
  suggestedWildcard: null,
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
});

// Seed each card from the backend's pending list, as a reload would.
const pending = (requests: {
  permissions?: PermissionRequest[];
  grants?: PathGrantRequest[];
  network?: NetworkAccessRequest[];
}) =>
  mockInvoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === 'list_pending_permission_requests'
        ? (requests.permissions ?? [])
        : command === 'list_pending_path_grant_requests'
          ? (requests.grants ?? [])
          : command === 'list_pending_network_access_requests'
            ? (requests.network ?? [])
            : undefined
    )
  );

beforeEach(() => {
  mockInvoke.mockReset();
});

describe('a command approval names the agent that asked', () => {
  it("draws a teammate's face and name, not the Main's, for a request its task raised", async () => {
    pending({ permissions: [permission('wa-rust')] });
    render(<InlineApprovalCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', {
      name: 'Permission requested by Rust Architect',
    });
    expect(faceOf(card)).toHaveAttribute('aria-hidden', 'true');
    expect(faceOf(card)?.innerHTML).toBe(svgOf(RUST));
    expect(card).toHaveTextContent('Rust Architect wants to run:');
  });

  it('draws the Main as "Main" when it asked', async () => {
    pending({ permissions: [permission('wa-main')] });
    render(<InlineApprovalCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Permission requested by Main' });
    expect(faceOf(card)?.innerHTML).toBe(svgOf(MAIN));
    expect(card).toHaveTextContent('Main wants to run:');
  });

  it.each([
    ['no agent', null],
    ['an agent the roster does not know', 'wa-gone'],
  ])('falls back to the plain subtitle, with no face, for %s', async (_, agentId) => {
    pending({ permissions: [permission(agentId)] });
    render(<InlineApprovalCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Permission requested' });
    expect(faceOf(card)).toBeNull();
    expect(card).toHaveTextContent('An agent wants to run:');
  });

  it('finds an agent removed from the roster mid-task on its task', async () => {
    pending({ permissions: [permission('wa-gone')] });
    const tasks = [
      {
        assignedToWorkspaceAgentId: 'wa-gone',
        assignedAgentDefinitionId: 'def-gone',
        assignedAgentDisplayName: 'Scout',
      } as WorkspaceTaskResponse,
    ];
    render(<InlineApprovalCard workspaceId="ws-1" roster={roster} loadedTasks={() => tasks} />);

    const card = await screen.findByRole('article', { name: 'Permission requested by Scout' });
    expect(faceOf(card)?.innerHTML).toBe(svgOf({ id: 'wa-gone', agentDefinitionId: 'def-gone', isDefault: false }));
    expect(card).toHaveTextContent('Scout wants to run:');
  });

  it('leaves the face to the Fleet when the card is spoken', () => {
    render(
      <ApprovalCard
        req={permission('wa-rust')}
        requester={rosterAgent('wa-rust', roster)}
        speaker={(prompt) => prompt}
      />
    );
    expect(faceOf(document.body)).toBeNull();
    expect(screen.queryByText(/Rust Architect/)).not.toBeInTheDocument();
  });
});

describe('a path grant names the agent that asked', () => {
  it("draws the asking agent's face and name", async () => {
    pending({ grants: [grant('wa-rust')] });
    render(<InlinePathGrantCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', {
      name: 'Path grant requested by Rust Architect',
    });
    expect(faceOf(card)?.innerHTML).toBe(svgOf(RUST));
    expect(card).toHaveTextContent('Rust Architect wants to extend its filesystem grants:');
  });

  it('falls back to the plain subtitle, with no face, when the agent is unknown', async () => {
    pending({ grants: [grant(null)] });
    render(<InlinePathGrantCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Path grant requested' });
    expect(faceOf(card)).toBeNull();
    expect(card).toHaveTextContent('An agent wants to extend its filesystem grants:');
  });
});

describe('a network request names the agent that asked', () => {
  it("draws the asking agent's face and name, and says it in the announcement", async () => {
    pending({ network: [network('wa-rust')] });
    render(<InlineNetworkAccessCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Network access requested by Rust Architect' });
    expect(faceOf(card)?.innerHTML).toBe(svgOf(RUST));
    expect(card).toHaveTextContent('Rust Architect wants network access:');
    expect(screen.getByRole('status')).toHaveTextContent('Rust Architect wants network access to mirror.example.org:443.');
  });

  it('falls back to the plain subtitle, with no face, when the agent is unknown', async () => {
    pending({ network: [network(null)] });
    render(<InlineNetworkAccessCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Network access requested' });
    expect(faceOf(card)).toBeNull();
    expect(card).toHaveTextContent('An agent wants network access:');
  });
});

describe('a question names the agent that asked', () => {
  const ask = { pendingId: 'q-1', question: 'Ship it?', options: null, extraContext: null };

  it("draws the asking agent's face and name next to the tag", () => {
    render(<AskUserPanel ask={ask} asker={rosterAgent('wa-rust', roster)} />);

    const panel = screen.getByRole('dialog', { name: 'Question from Rust Architect' });
    expect(faceOf(panel)?.innerHTML).toBe(svgOf(RUST));
    expect(within(panel).getByText('Rust Architect')).toBeInTheDocument();
    expect(within(panel).getByText('AGENT IS ASKING')).toBeInTheDocument();
  });

  it('draws no face when nobody is known to have asked', () => {
    render(<AskUserPanel ask={ask} />);

    const panel = screen.getByRole('dialog', { name: 'Agent question' });
    expect(faceOf(panel)).toBeNull();
  });
});
