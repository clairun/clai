import type { ReactNode } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { PathGrantRequest } from '../generated/bindings';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import InlinePathGrantCard, { PathGrantCard } from './InlinePathGrantCard';

const grant = {
  requestId: 'grant-1',
  workspaceId: 'ws-1',
  agentId: null,
  agentName: null,
  requestedPath: '/home/me/.ssh',
  requestedAccess: 'read_only',
  reason: 'Read the deploy key fingerprint.',
} as unknown as PathGrantRequest;

describe('PathGrantCard — spoken (the Fleet)', () => {
  it('says the path and reason, keeps the warnings, and leaves narrowing to the full card', () => {
    render(<PathGrantCard req={grant} speaker={(prompt: ReactNode) => <div>{prompt}</div>} />);
    expect(screen.queryByText('Path access requested')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /collapse/i })).not.toBeInTheDocument();
    expect(screen.getByText('/home/me/.ssh')).toBeInTheDocument();
    expect(screen.getByText('Read the deploy key fingerprint.')).toBeInTheDocument();
    expect(screen.getByText(/typically holds credentials or secrets/)).toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Always' })).toBeInTheDocument();
  });

  it('keeps the tag, the collapse and the path field on the full card', () => {
    render(<PathGrantCard req={grant} />);
    expect(screen.getByText('Path access requested')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /collapse/i })).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('/home/me/.ssh');
  });
});

describe('InlinePathGrantCard — each card its own submit', () => {
  it('shows a failed submit on that card only, and leaves the other card answerable', async () => {
    let reject: (reason: unknown) => void = () => {};
    const other = { ...grant, requestId: 'grant-2', requestedPath: '/srv/data' };
    mockInvoke.mockImplementation((cmd: string) =>
      cmd === 'submit_path_grant_decision'
        ? new Promise((_, no) => { reject = no; })
        : Promise.resolve(cmd === 'list_pending_path_grant_requests' ? [grant, other] : undefined)
    );
    render(<InlinePathGrantCard workspaceId="ws-1" />);
    const [first, second] = (await screen.findAllByRole('article', { name: 'Path grant requested' })).map((card) => within(card));

    await userEvent.click(first!.getByRole('button', { name: 'Deny' }));
    expect(first!.getByRole('button', { name: 'Deny' })).toBeDisabled();
    expect(second!.getByRole('button', { name: 'Deny' })).toBeEnabled();

    reject('Grant channel closed');
    expect(await first!.findByText('Grant channel closed')).toBeInTheDocument();
    expect(second!.queryByText('Grant channel closed')).not.toBeInTheDocument();
    expect(second!.getByRole('button', { name: 'Deny' })).toBeEnabled();
  });
});
