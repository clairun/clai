import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import InlineApprovalCard, { ApprovalCard } from './InlineApprovalCard';
import InlinePathGrantCard from './InlinePathGrantCard';
import AskUserPanel from './AskUserPanel/AskUserPanel';
import { rosterAgent } from './AssistantChat/messageSender';
import type {
  PathGrantRequest,
  PermissionRequest,
  WorkspaceAgentResponse,
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

const roster = [
  agentRow({
    id: 'wa-main',
    agentDefinitionId: 'def-main',
    displayName: 'Manager',
    isDefault: true,
  }),
  agentRow({ id: 'wa-rust', agentDefinitionId: 'def-rust', displayName: 'Rust Architect' }),
];

const permission = (agentId: string | null): PermissionRequest => ({
  requestId: `perm-${agentId}`,
  workspaceId: 'ws-1',
  agentId,
  agentName: null,
  command: 'cargo test',
  segments: [{ text: 'cargo test', kind: 'simple', suggestedPrefix: 'cargo test' }],
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

// Seed each card from the backend's pending list, as a reload would.
const pending = (requests: { permissions?: PermissionRequest[]; grants?: PathGrantRequest[] }) =>
  mockInvoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === 'list_pending_permission_requests'
        ? (requests.permissions ?? [])
        : command === 'list_pending_path_grant_requests'
          ? (requests.grants ?? [])
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
    expect(within(card).getByRole('img', { name: 'Rust Architect' })).toBeInTheDocument();
    expect(within(card).queryByRole('img', { name: 'Main' })).not.toBeInTheDocument();
    expect(card).toHaveTextContent('Rust Architect wants to run:');
  });

  it('draws the Main as "Main" when it asked', async () => {
    pending({ permissions: [permission('wa-main')] });
    render(<InlineApprovalCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Permission requested by Main' });
    expect(within(card).getByRole('img', { name: 'Main' })).toBeInTheDocument();
  });

  it.each([
    ['no agent', null],
    ['an agent the roster does not know', 'wa-gone'],
  ])('falls back to the plain subtitle, with no face, for %s', async (_, agentId) => {
    pending({ permissions: [permission(agentId)] });
    render(<InlineApprovalCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Permission requested' });
    expect(within(card).queryByRole('img')).not.toBeInTheDocument();
    expect(card).toHaveTextContent('An agent wants to run:');
  });

  it('leaves the face to the Fleet when the card is spoken', () => {
    render(
      <ApprovalCard
        req={permission('wa-rust')}
        requester={rosterAgent('wa-rust', roster)}
        speaker={(prompt) => prompt}
      />
    );
    expect(screen.queryByRole('img', { name: 'Rust Architect' })).not.toBeInTheDocument();
  });
});

describe('a path grant names the agent that asked', () => {
  it("draws the asking agent's face and name", async () => {
    pending({ grants: [grant('wa-rust')] });
    render(<InlinePathGrantCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', {
      name: 'Path grant requested by Rust Architect',
    });
    expect(within(card).getByRole('img', { name: 'Rust Architect' })).toBeInTheDocument();
    expect(card).toHaveTextContent('Rust Architect wants to extend its filesystem grants:');
  });

  it('falls back to the plain subtitle, with no face, when the agent is unknown', async () => {
    pending({ grants: [grant(null)] });
    render(<InlinePathGrantCard workspaceId="ws-1" roster={roster} />);

    const card = await screen.findByRole('article', { name: 'Path grant requested' });
    expect(within(card).queryByRole('img')).not.toBeInTheDocument();
    expect(card).toHaveTextContent('An agent wants to extend its filesystem grants:');
  });
});

describe('a question names the agent that asked', () => {
  const ask = { pendingId: 'q-1', question: 'Ship it?', options: null, extraContext: null };

  it("draws the asking agent's face and name next to the tag", () => {
    render(<AskUserPanel ask={ask} asker={rosterAgent('wa-rust', roster)} />);

    const panel = screen.getByRole('dialog', { name: 'Question from Rust Architect' });
    expect(within(panel).getByRole('img', { name: 'Rust Architect' })).toBeInTheDocument();
    expect(within(panel).getByText('AGENT IS ASKING')).toBeInTheDocument();
  });

  it('draws no face when nobody is known to have asked', () => {
    render(<AskUserPanel ask={ask} />);

    const panel = screen.getByRole('dialog', { name: 'Agent question' });
    expect(within(panel).queryByRole('img')).not.toBeInTheDocument();
  });
});
