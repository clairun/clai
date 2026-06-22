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
  // Surface `visible` + `workspaceId` so tests can assert keep-alive: a hidden
  // terminal stays mounted (in the DOM) but `data-visible="false"`. The
  // exit button lets tests drive `onShellExit` (a real shell exit / Ctrl-D).
  default: ({
    visible,
    workspaceId,
    onShellExit,
  }: {
    visible: boolean;
    workspaceId: string;
    onShellExit: () => void;
  }) => (
    <div data-testid="workspace-terminal" data-workspace={workspaceId} data-visible={String(visible)}>
      <button type="button" onClick={onShellExit}>
        exit-{workspaceId}
      </button>
    </div>
  ),
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

  // The terminal is now kept alive for the whole app session (approach A): a
  // hidden terminal stays MOUNTED (so its PTY + screen survive) with
  // data-visible="false"; only the active workspace's terminal is visible.
  const visibleWorkspaces = () =>
    screen
      .queryAllByTestId('workspace-terminal')
      .filter((el) => el.getAttribute('data-visible') === 'true')
      .map((el) => el.getAttribute('data-workspace'));

  it('keeps a workspace terminal alive (hidden) across switches', async () => {
    const user = userEvent.setup();
    renderComposer();

    // Enable terminal mode in A — A's terminal is the visible one.
    await user.click(screen.getByRole('button', { name: /terminal mode/i }));
    expect(visibleWorkspaces()).toEqual(['A']);

    // Switch to B (never opened a terminal): A's terminal stays MOUNTED but
    // hidden (PTY kept alive), and nothing is visible.
    await user.click(screen.getByText('go-B'));
    expect(screen.getByTestId('workspace-terminal').getAttribute('data-workspace')).toBe('A');
    expect(visibleWorkspaces()).toEqual([]);

    // Back to A — the SAME kept-alive terminal is shown again.
    await user.click(screen.getByText('go-A'));
    expect(visibleWorkspaces()).toEqual(['A']);
  });

  it('keeps the terminal alive (hidden) when navigating off a workspace route', async () => {
    const user = userEvent.setup();
    renderComposer();

    // Terminal on in A, then navigate to a non-workspace route.
    await user.click(screen.getByRole('button', { name: /terminal mode/i }));
    expect(visibleWorkspaces()).toEqual(['A']);

    await user.click(screen.getByText('go-fleet'));
    // Still mounted (PTY survives), just not visible anywhere.
    expect(screen.queryAllByTestId('workspace-terminal')).toHaveLength(1);
    expect(visibleWorkspaces()).toEqual([]);

    // Returning to A shows it again (terminal mode stored per workspace).
    await user.click(screen.getByText('go-A'));
    expect(visibleWorkspaces()).toEqual(['A']);
  });

  it('reopens a terminal in one Ctrl+\\ after its shell exited while hidden', async () => {
    const user = userEvent.setup();
    renderComposer();

    // Open A's terminal, then switch to B so A is mounted-but-hidden.
    await user.click(screen.getByRole('button', { name: /terminal mode/i }));
    expect(visibleWorkspaces()).toEqual(['A']);
    await user.click(screen.getByText('go-B'));
    expect(visibleWorkspaces()).toEqual([]);

    // A's shell exits while hidden -> A leaves the kept-alive set entirely.
    await user.click(screen.getByText('exit-A'));

    // Back on A: its saved terminalMode is still true, but with no live
    // terminal we fall back to the composer (no blank pane), not a mounted
    // hidden terminal.
    await user.click(screen.getByText('go-A'));
    expect(screen.queryAllByTestId('workspace-terminal')).toHaveLength(0);

    // Regression: a SINGLE Ctrl+\ must reopen A's terminal. Before the fix the
    // toggle keyed off the stale terminalMode flag and the first press was a
    // no-op, so it took two.
    await user.keyboard('{Control>}\\{/Control}');
    expect(visibleWorkspaces()).toEqual(['A']);
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

  it('falls back to the native clipboard when the paste event has no image (WebKit)', async () => {
    const onAttachImage = vi.fn(async () => ({ part: imagePart }));
    const file = new File(['x'], 'pasted.png', { type: 'image/png' });
    const onReadClipboardImage = vi.fn(async () => file);

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <TerminalEmulator
          onAttachImage={onAttachImage}
          onReadClipboardImage={onReadClipboardImage}
          onSendToChat={vi.fn()}
        />
      </MemoryRouter>
    );

    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    // Empty DataTransfer — the WebKit case where the webview surfaces no file.
    fireEvent.paste(input, { clipboardData: { items: [] } });

    await screen.findByAltText('shot.png');
    expect(onReadClipboardImage).toHaveBeenCalledTimes(1);
    expect(onAttachImage).toHaveBeenCalledTimes(1);
  });

  it('lets a non-image paste through (native clipboard returns null)', async () => {
    const onAttachImage = vi.fn(async () => ({ part: imagePart }));
    const onReadClipboardImage = vi.fn(async () => null);

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <TerminalEmulator
          onAttachImage={onAttachImage}
          onReadClipboardImage={onReadClipboardImage}
          onSendToChat={vi.fn()}
        />
      </MemoryRouter>
    );

    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    fireEvent.paste(input, { clipboardData: { items: [] } });

    // Gave the native read a chance, but nothing attached (text paste path).
    await Promise.resolve();
    expect(onReadClipboardImage).toHaveBeenCalledTimes(1);
    expect(onAttachImage).not.toHaveBeenCalled();
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

  it('attaches an image via the file-picker button and sends it', async () => {
    const user = userEvent.setup();
    const onPickImage = vi.fn(async () => ({ part: imagePart }));
    const onSendToChat = vi.fn(async () => ({}));

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <TerminalEmulator
          onPickImage={onPickImage}
          onAttachImage={vi.fn()}
          onSendToChat={onSendToChat}
        />
      </MemoryRouter>
    );

    await user.click(screen.getByLabelText('Attach image'));

    // Picked images have no object URL, so the tray shows a filename chip.
    await screen.findByText('shot.png');
    expect(onPickImage).toHaveBeenCalledTimes(1);

    // Focus the composer (the button had focus) then image-only send (no text).
    await user.click(screen.getByRole('textbox'));
    await user.keyboard('{Enter}');
    expect(onSendToChat).toHaveBeenCalledWith(
      '',
      expect.arrayContaining([expect.objectContaining({ id: 'img-1', type: 'image' })])
    );
  });

  it('drops a pending attachment on workspace switch (no wrong-root send)', async () => {
    const user = userEvent.setup();
    const onAttachImage = vi.fn(async () => ({ part: imagePart }));

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <NavTo to="/workspace/B" label="go-B" />
        <TerminalEmulator onAttachImage={onAttachImage} onSendToChat={vi.fn(async () => ({}))} />
      </MemoryRouter>
    );

    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    const file = new File(['x'], 'shot.png', { type: 'image/png' });
    fireEvent.paste(input, {
      clipboardData: { items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }] },
    });
    await screen.findByAltText('shot.png');

    // Switching workspace must drop the attachment — its stored path is
    // relative to A's root and would silently fail to resolve under B.
    await user.click(screen.getByText('go-B'));
    expect(screen.queryByAltText('shot.png')).toBeNull();
  });

  it('keeps the attachment when the send fails, so it can be retried', async () => {
    const user = userEvent.setup();
    const onAttachImage = vi.fn(async () => ({ part: imagePart }));
    const onSendToChat = vi.fn(async () => ({ error: 'send failed' }));

    render(
      <MemoryRouter initialEntries={['/workspace/A']}>
        <TerminalEmulator onAttachImage={onAttachImage} onSendToChat={onSendToChat} />
      </MemoryRouter>
    );

    const input = screen.getByRole('textbox') as HTMLTextAreaElement;
    const file = new File(['x'], 'shot.png', { type: 'image/png' });
    fireEvent.paste(input, {
      clipboardData: { items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }] },
    });
    await screen.findByAltText('shot.png');

    await user.type(input, 'hi');
    await user.keyboard('{Enter}');

    expect(onSendToChat).toHaveBeenCalled();
    // Failed send keeps the image attached (cleared only on success).
    expect(await screen.findByAltText('shot.png')).toBeInTheDocument();
  });
});
