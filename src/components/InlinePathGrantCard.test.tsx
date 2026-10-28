import type { ReactNode } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { PathGrantRequest } from '../generated/bindings';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import { PathGrantCard } from './InlinePathGrantCard';

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
