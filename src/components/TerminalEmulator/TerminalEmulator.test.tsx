import { describe, it, expect, vi, beforeAll } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
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


describe('TerminalEmulator image attachments', () => {
  beforeAll(() => {
    // jsdom lacks object-URL helpers the composer uses for thumbnails.
    globalThis.URL.createObjectURL = vi.fn(() => 'blob:preview');
    globalThis.URL.revokeObjectURL = vi.fn();
  });

  const imagePart = {
    type: 'image' as const,
    id: 'img-1',
    path: '.clai/images/img-1.png',
    media_type: 'image/png',
    filename: 'shot.png',
    width: null,
    height: null,
  };

  it('attaches a pasted image, shows a thumbnail, and sends it with the message', async () => {
    const user = userEvent.setup();
    const onAttachImage = vi.fn(async () => ({ part: imagePart }));
    const onSendToChat = vi.fn(async () => ({}));

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <TerminalEmulator onAttachImage={onAttachImage} onSendToChat={onSendToChat} />
      </MemoryRouter>
    );

    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    const file = new File(['x'], 'shot.png', { type: 'image/png' });
    fireEvent.paste(input, {
      clipboardData: {
        items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }],
      },
    });

    // Thumbnail appears once the attach resolves.
    await screen.findByAltText('shot.png');
    expect(onAttachImage).toHaveBeenCalledTimes(1);

    // Type text and send — the image rides along, then the tray clears.
    await user.type(input, 'what is this');
    await user.keyboard('{Enter}');

    expect(onSendToChat).toHaveBeenCalledWith(
      'what is this',
      expect.arrayContaining([expect.objectContaining({ id: 'img-1', type: 'image' })])
    );
    expect(screen.queryByAltText('shot.png')).toBeNull();
  });

  it('removes a pasted image from the tray before send', async () => {
    const user = userEvent.setup();
    const onAttachImage = vi.fn(async () => ({ part: imagePart }));

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <TerminalEmulator onAttachImage={onAttachImage} onSendToChat={vi.fn()} />
      </MemoryRouter>
    );

    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    const file = new File(['x'], 'shot.png', { type: 'image/png' });
    fireEvent.paste(input, {
      clipboardData: {
        items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }],
      },
    });

    await screen.findByAltText('shot.png');
    await user.click(screen.getByLabelText('Remove image'));
    expect(screen.queryByAltText('shot.png')).toBeNull();
  });
});
