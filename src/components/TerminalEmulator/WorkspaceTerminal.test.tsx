import { render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({
  invoke: mockInvoke,
  Channel: class {
    onmessage: ((event: unknown) => void) | null = null;
  },
}));
vi.mock('./webglSupport', () => ({ shouldUseWebglRenderer: () => false }));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

import WorkspaceTerminal from './WorkspaceTerminal';

const originalResizeObserver = globalThis.ResizeObserver;
const originalMatchMedia = window.matchMedia;

function terminalWrites(): string[] {
  return mockInvoke.mock.calls
    .filter(([cmd]) => cmd === 'terminal_write')
    .map(([, args]) => (args as { data: string }).data);
}

/**
 * A keyboard paste in a real webview is a keydown whose default action is a
 * native `paste` event on the focused element (xterm's hidden textarea).
 * jsdom has no default actions, so dispatch both halves explicitly.
 */
function keyboardPaste(textarea: HTMLTextAreaElement, init: KeyboardEventInit, text: string) {
  const keydown = new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init });
  textarea.dispatchEvent(keydown);
  if (keydown.defaultPrevented) return;
  const paste = new Event('paste', { bubbles: true, cancelable: true });
  Object.defineProperty(paste, 'clipboardData', {
    value: { getData: (type: string) => (type === 'text/plain' ? text : '') },
  });
  textarea.dispatchEvent(paste);
}

describe('WorkspaceTerminal paste', () => {
  beforeEach(() => {
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
    window.matchMedia = vi.fn().mockReturnValue({
      matches: false,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    });
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { readText: vi.fn().mockResolvedValue('echo hi'), writeText: vi.fn() },
    });
    mockInvoke.mockImplementation((cmd: string) =>
      Promise.resolve(cmd === 'terminal_open' ? 'session-1' : undefined)
    );
  });

  afterEach(() => {
    globalThis.ResizeObserver = originalResizeObserver;
    window.matchMedia = originalMatchMedia;
  });

  it.each([
    ['Ctrl+Shift+V', { ctrlKey: true, shiftKey: true, code: 'KeyV', key: 'V' }],
    ['Cmd+V', { metaKey: true, code: 'KeyV', key: 'v' }],
  ])('%s inserts the clipboard text exactly once', async (_label, init) => {
    const { container } = render(
      <WorkspaceTerminal
        workspaceId="ws-1"
        visible
        onBackToChat={vi.fn()}
        onShellExit={vi.fn()}
        fullscreen={false}
        onToggleFullscreen={vi.fn()}
      />
    );
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('terminal_open', expect.anything())
    );
    // Let terminal_open resolve so onData is wired to terminal_write.
    await new Promise((resolve) => setTimeout(resolve, 0));

    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    keyboardPaste(textarea, init, 'echo hi');
    // Flush any async clipboard read a handler might have started.
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(terminalWrites()).toEqual(['echo hi']);
  });

  it('paints xterm with the --color-terminal-bg token and keeps both buttons in one row', async () => {
    document.documentElement.style.setProperty('--color-terminal-bg', '#123456');
    try {
      const { container, getByRole } = render(
        <WorkspaceTerminal
          workspaceId="ws-1"
          visible
          onBackToChat={vi.fn()}
          onShellExit={vi.fn()}
          fullscreen={false}
          onToggleFullscreen={vi.fn()}
        />
      );
      await waitFor(() =>
        expect((container.querySelector('.xterm-viewport') as HTMLElement).style.backgroundColor).toBe('rgb(18, 52, 86)')
      );
      // One row: the label's width can no longer push one button under the other.
      expect(getByRole('button', { name: 'Fullscreen' }).parentElement).toBe(
        getByRole('button', { name: 'Back to chat' }).parentElement
      );
    } finally {
      document.documentElement.style.removeProperty('--color-terminal-bg');
    }
  });

  it('repaints xterm with the new token when the app theme switches', async () => {
    const root = document.documentElement;
    root.setAttribute('data-theme', 'light');
    root.style.setProperty('--color-terminal-bg', '#123456');
    try {
      const { container } = render(
        <WorkspaceTerminal
          workspaceId="ws-1"
          visible
          onBackToChat={vi.fn()}
          onShellExit={vi.fn()}
          fullscreen={false}
          onToggleFullscreen={vi.fn()}
        />
      );
      const viewport = () => container.querySelector('.xterm-viewport') as HTMLElement;
      await waitFor(() => expect(viewport().style.backgroundColor).toBe('rgb(18, 52, 86)'));

      // Settings switches the theme: the token's value changes with it.
      root.style.setProperty('--color-terminal-bg', '#654321');
      root.setAttribute('data-theme', 'dark');

      await waitFor(() => expect(viewport().style.backgroundColor).toBe('rgb(101, 67, 33)'));
    } finally {
      root.style.removeProperty('--color-terminal-bg');
      root.removeAttribute('data-theme');
    }
  });
});
