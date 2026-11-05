import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { NetworkAccessRequest, WorkspaceAgentResponse } from '../generated/bindings';
import { rosterAgent } from './AssistantChat/messageSender';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import InlineNetworkAccessCard, { NetworkAccessCard } from './InlineNetworkAccessCard';

const request = (overrides: Partial<NetworkAccessRequest> = {}): NetworkAccessRequest => ({
  requestId: 'net-1',
  workspaceId: 'ws-1',
  agentId: 'agent-1',
  agentName: null,
  host: 'mirror.example.org',
  port: 443,
  reason: 'Download a test fixture.',
  runHosts: [
    { host: 'github.com', port: 443, allowed: true },
    { host: 'mirror.example.org', port: 443, allowed: false },
  ],
  suggestedWildcard: '*.example.org',
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
  ...overrides,
});

const requester = rosterAgent('wa-rust', [
  {
    id: 'wa-rust',
    workspaceId: 'ws-1',
    agentDefinitionId: 'def-rust',
    displayName: 'Rust Architect',
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
  } as unknown as WorkspaceAgentResponse,
]);

afterEach(() => {
  mockInvoke.mockReset();
});

const submitted = () =>
  mockInvoke.mock.calls.filter(([cmd]) => cmd === 'submit_network_access_decision').map(([, args]) => args);

describe('NetworkAccessCard', () => {
  it('shows the host, the reason and what this run reached', () => {
    render(<NetworkAccessCard req={request()} />);
    expect(screen.getByText('Network access requested')).toBeInTheDocument();
    expect(screen.getByText('mirror.example.org:443')).toBeInTheDocument();
    expect(screen.getByText('not on allowlist')).toBeInTheDocument();
    expect(screen.getByText('Download a test fixture.')).toBeInTheDocument();
    const runLine = screen.getByText(/^This run:/);
    expect(runLine).toHaveTextContent('This run: github.com ✓ · mirror.example.org ✕');
    expect(within(runLine).getByRole('img', { name: 'allowed' })).toBeInTheDocument();
    expect(within(runLine).getByRole('img', { name: 'refused' })).toBeInTheDocument();
  });

  it('sends each choice with its entry', async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    for (const [label, decision] of [
      ['Allow this run', { kind: 'allowRun' }],
      ['Always for Rust Architect', { kind: 'allowAlways', entry: 'mirror.example.org' }],
      ['Allow *.example.org', { kind: 'allowAlways', entry: '*.example.org' }],
    ] as const) {
      const { unmount } = render(<NetworkAccessCard req={request()} requester={requester} />);
      await user.click(screen.getByRole('button', { name: label }));
      expect(submitted().at(-1)).toEqual({ requestId: 'net-1', decision });
      unmount();
    }
  });

  it('offers no wildcard when the host has no parent domain to widen to', () => {
    render(<NetworkAccessCard req={request({ host: 'example.org', suggestedWildcard: null })} />);
    expect(screen.queryByRole('button', { name: /^Allow \*\./ })).not.toBeInTheDocument();
    expect(screen.getByText(/adds/)).toHaveTextContent(/Always. adds example\.org to this agent.s allowlist in this workspace only\./);
  });

  it('denies with the reason the user typed, or none', async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    const { unmount } = render(<NetworkAccessCard req={request()} />);
    await user.click(screen.getByRole('button', { name: 'Deny' }));
    await user.type(screen.getByLabelText('Reason for denying (optional)'), '  use the internal mirror ');
    await user.click(screen.getByRole('button', { name: 'Deny access' }));
    expect(submitted().at(-1)).toEqual({ requestId: 'net-1', decision: { kind: 'deny', reason: 'use the internal mirror' } });
    unmount();

    render(<NetworkAccessCard req={request()} />);
    await user.click(screen.getByRole('button', { name: 'Deny' }));
    await user.click(screen.getByRole('button', { name: 'Deny access' }));
    expect(submitted().at(-1)).toEqual({ requestId: 'net-1', decision: { kind: 'deny' } });
  });

  it('says where Always saves', () => {
    const { unmount } = render(<NetworkAccessCard req={request()} workspaceName="clai" />);
    expect(screen.getByText(/add their entry/)).toHaveTextContent(/allowlist in .clai. only\.$/);
    unmount();

    render(
      <NetworkAccessCard
        req={request({ persistsToSharedAgent: true, alsoAffectsWorkspaces: ['docs-site'] })}
        workspaceName="clai"
      />,
    );
    expect(screen.getByText(/add their entry/)).toHaveTextContent(
      /shared agent.s allowlist in every workspace that has it, now and later\. Today that is .clai. and .docs-site.\./,
    );
  });

  it('keeps a refused decision on the card so the user can answer again', async () => {
    mockInvoke.mockRejectedValueOnce('not a narrow entry').mockResolvedValue(undefined);
    const onDecided = vi.fn();
    const user = userEvent.setup();
    render(<NetworkAccessCard req={request()} onDecided={onDecided} />);
    await user.click(screen.getByRole('button', { name: 'Allow *.example.org' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('not a narrow entry');
    expect(onDecided).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Allow this run' }));
    expect(onDecided).toHaveBeenCalledWith('net-1');
  });

  it('speaks on the Fleet with short chips', () => {
    render(<NetworkAccessCard req={request()} speaker={(prompt: ReactNode) => <div>{prompt}</div>} />);
    expect(screen.queryByText('Network access requested')).not.toBeInTheDocument();
    expect(screen.getByText('mirror.example.org:443')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'This run' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Always' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '*.example.org' })).toBeInTheDocument();
  });
});

describe('InlineNetworkAccessCard', () => {
  it('shows the pending requests of its workspace', async () => {
    mockInvoke.mockImplementation((cmd: string) =>
      Promise.resolve(
        cmd === 'list_pending_network_access_requests'
          ? [request(), request({ requestId: 'net-2', host: 'cdn.example.com' })]
          : undefined,
      ),
    );
    render(<InlineNetworkAccessCard workspaceId="ws-1" />);
    expect(await screen.findAllByRole('article', { name: 'Network access requested' })).toHaveLength(2);
    expect(mockInvoke).toHaveBeenCalledWith('list_pending_network_access_requests', { workspaceId: 'ws-1' });
  });
});
