import { describe, it, expect, vi, beforeAll } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, useNavigate } from 'react-router-dom';

// jsdom lacks ResizeObserver, which the composer observes on mount.
beforeAll(() => {
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  );
});

// Stub the heavy children so the test doesn't pull in xterm/Tauri/MCP fetches.
// The real PTY terminal is exercised elsewhere; here we only care that the
// composer shows it (terminal mode) for the right workspace.
vi.mock('./WorkspaceTerminal', () => ({
  default: () => <div data-testid="workspace-terminal" />,
}));
vi.mock('../../workspace/components/WorkspaceContextBar', () => ({
  default: () => <div data-testid="context-bar" />,
}));

import TerminalEmulator from './TerminalEmulator';

// A button that navigates the shared router so the (persistent, global)
// TerminalEmulator instance sees a workspace switch — exactly how MainLayout
// keeps one composer mounted across route changes.
function NavTo({ to, label }: { to: string; label: string }) {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate(to)}>
      {label}
    </button>
  );
}

function renderComposer() {
  return render(
    <MemoryRouter initialEntries={['/workspace/A']}>
      <NavTo to="/workspace/A" label="go-A" />
      <NavTo to="/workspace/B" label="go-B" />
      <NavTo to="/fleet" label="go-fleet" />
      <TerminalEmulator />
    </MemoryRouter>
  );
}

describe('TerminalEmulator per-workspace composer state', () => {
  it('keeps an unsent draft per workspace across switches', async () => {
    const user = userEvent.setup();
    renderComposer();

    const input = () => screen.getByRole('textbox') as HTMLTextAreaElement;

    await user.type(input(), 'draft for A');
    expect(input().value).toBe('draft for A');

    // Switch to workspace B — its draft is empty, A's must not leak in.
    await user.click(screen.getByText('go-B'));
    expect(input().value).toBe('');

    // Type a B draft, then go back to A — A's draft is restored, B's saved.
    await user.type(input(), 'draft for B');
    await user.click(screen.getByText('go-A'));
    expect(input().value).toBe('draft for A');

    await user.click(screen.getByText('go-B'));
    expect(input().value).toBe('draft for B');
  });

  it('keeps terminal mode per workspace across switches', async () => {
    const user = userEvent.setup();
    renderComposer();

    // Enable terminal mode in A.
    await user.click(screen.getByRole('button', { name: /terminal mode/i }));
    expect(screen.getByTestId('workspace-terminal')).toBeInTheDocument();

    // Switch to B — terminal must NOT carry over (B was never in terminal mode).
    await user.click(screen.getByText('go-B'));
    expect(screen.queryByTestId('workspace-terminal')).not.toBeInTheDocument();

    // Back to A — terminal mode is restored.
    await user.click(screen.getByText('go-A'));
    expect(screen.getByTestId('workspace-terminal')).toBeInTheDocument();
  });

  it('closes the terminal when navigating off a workspace route', async () => {
    const user = userEvent.setup();
    renderComposer();

    // Terminal on in A, then navigate to a non-workspace route.
    await user.click(screen.getByRole('button', { name: /terminal mode/i }));
    expect(screen.getByTestId('workspace-terminal')).toBeInTheDocument();

    await user.click(screen.getByText('go-fleet'));
    expect(screen.queryByTestId('workspace-terminal')).not.toBeInTheDocument();

    // Returning to A restores its terminal mode (stored per workspace).
    await user.click(screen.getByText('go-A'));
    expect(screen.getByTestId('workspace-terminal')).toBeInTheDocument();
  });
});
