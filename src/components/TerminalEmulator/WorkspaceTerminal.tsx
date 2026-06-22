import React, { useCallback, useEffect, useRef } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import '@xterm/xterm/css/xterm.css';
import styles from './WorkspaceTerminal.module.css';

/**
 * Integrated terminal panel (Phase 2 of the integrated-terminal feature — see
 * terminal-feature-design.md). A real PTY-backed shell at the workspace's
 * directory, rendered with xterm.js. The PTY backend + 16ms output coalescing
 * live in `src-tauri/src/commands/terminal.rs`.
 *
 * **Lifecycle (keep-alive).** A shell is spawned once, on first mount, and then
 * kept alive for the whole app session: navigating to chat or another workspace
 * does NOT tear it down — the parent keeps this component mounted and merely
 * toggles `visible`, so the PTY *and* the rendered screen (a running vim, a
 * build log, scrollback) survive untouched and are exactly where you left them
 * on return. The shell is only torn down when it actually exits (the user types
 * `exit` / Ctrl-D, firing `onShellExit`) or the app quits (backend `close_all`).
 * Hiding is pure CSS (`display:none`); we never `dispose()` on hide.
 */

type TerminalEvent = { type: 'output'; dataB64: string } | { type: 'exit'; code: number | null };

function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const len = binary.length;
  const bytes = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

interface WorkspaceTerminalProps {
  /** Workspace whose root directory the shell opens in. */
  workspaceId: string;
  /**
   * Whether this terminal is the one currently on screen. Kept-alive terminals
   * for other workspaces (or while you're in chat) stay mounted with
   * `visible={false}` so their PTY and screen persist; only the visible one
   * renders and takes keyboard focus.
   */
  visible: boolean;
  /** Leave terminal mode but KEEP the shell alive (Chat button / Ctrl+\). */
  onBackToChat: () => void;
  /** The shell process exited (`exit` / Ctrl-D); the session is gone for good. */
  onShellExit: () => void;
  /**
   * Consume-once getter for a command to run when the terminal is shown (the
   * `!cmd` chat fast-path). Returns the command and clears it, so it runs
   * exactly once. Delivered either by the first-prompt path on a fresh mount or
   * by the show effect on an already-running kept-alive shell — never both.
   */
  consumeInitialCommand?: () => string | null;
  /** Whether the terminal fills the detail pane (fullscreen). */
  fullscreen: boolean;
  /** Toggle fullscreen (maximize button / Ctrl+Shift+Enter). */
  onToggleFullscreen: () => void;
}

const WorkspaceTerminal: React.FC<WorkspaceTerminalProps> = ({
  workspaceId,
  visible,
  onBackToChat,
  onShellExit,
  consumeInitialCommand,
  fullscreen,
  onToggleFullscreen,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const sessionRef = useRef<string | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const shellReadyRef = useRef(false);
  // Keep the latest callbacks/getter without re-running the setup effect (which
  // would tear down and respawn the shell — the opposite of keep-alive).
  const onShellExitRef = useRef(onShellExit);
  const onBackToChatRef = useRef(onBackToChat);
  const consumeRef = useRef(consumeInitialCommand);
  useEffect(() => {
    onShellExitRef.current = onShellExit;
    onBackToChatRef.current = onBackToChat;
    consumeRef.current = consumeInitialCommand;
  }, [onShellExit, onBackToChat, consumeInitialCommand]);

  const writeToShell = (cmd: string) => {
    const id = sessionRef.current;
    if (id) void invoke('terminal_write', { sessionId: id, data: `${cmd}\r` });
  };

  // Keep the PTY winsize in lockstep with the xterm grid. fit() resizes the JS
  // grid; we then *explicitly* push cols/rows to the PTY rather than relying on
  // xterm's onResize, which only fires when the grid itself changes. A PTY left
  // stale — a fit that ran before the session id was ready, or a geometry
  // change xterm rounds to the same grid — is exactly what makes full-screen
  // TUIs (vim, htop) draw at the old size while flowing shell output looks fine
  // (the shell wraps to the live grid; a TUI trusts TIOCGWINSZ).
  const syncSizeToPty = useCallback(() => {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit) return;
    try {
      fit.fit();
    } catch {
      return; // container detached mid-teardown
    }
    const id = sessionRef.current;
    if (id) {
      void invoke('terminal_resize', { sessionId: id, cols: term.cols, rows: term.rows });
    }
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return undefined;

    const term = new Terminal({
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
      fontSize: 13,
      scrollback: 8000,
      cursorBlink: true,
      // Dark terminal pane regardless of app theme — the conventional look,
      // and avoids unreadable ANSI colours on a light background.
      theme: { background: '#0b0e14', foreground: '#cbd5e1', cursor: '#7ee787' },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    termRef.current = term;
    fitRef.current = fit;

    // Prefer the WebGL renderer (fastest); fall back to the DOM renderer if
    // the WebKit webview can't provide a GL context. If the GL context is later
    // lost (e.g. while the terminal is hidden), the addon disposes itself and
    // xterm reverts to the DOM renderer — the text buffer is renderer-agnostic,
    // so the screen content (vim, scrollback) survives a hide/show cycle.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* DOM renderer fallback — still correct, just slower. */
    }

    fit.fit();

    // Copy/paste. Ctrl+C must stay SIGINT, so copy uses the conventional
    // terminal chord Ctrl+Shift+C (and Cmd+C on macOS); paste is Ctrl+Shift+V
    // / Cmd+V. `e.code` is physical-key based, so it is keyboard-layout
    // independent. Returning false tells xterm we handled the key and stops it
    // reaching the shell.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown') return true;
      const copyCombo =
        (e.ctrlKey && e.shiftKey && e.code === 'KeyC') || (e.metaKey && e.code === 'KeyC');
      if (copyCombo) {
        const selection = term.getSelection();
        if (selection) {
          void navigator.clipboard.writeText(selection);
          return false;
        }
        // No selection: let it through (e.g. Cmd+C / Ctrl+Shift+C no-op, or
        // shell handles it).
        return true;
      }
      const pasteCombo =
        (e.ctrlKey && e.shiftKey && e.code === 'KeyV') || (e.metaKey && e.code === 'KeyV');
      if (pasteCombo) {
        void navigator.clipboard.readText().then((text) => {
          // term.paste routes through onData -> terminal_write and honours
          // bracketed-paste mode when the shell enables it.
          if (text) term.paste(text);
        });
        return false;
      }
      return true;
    });

    // React StrictMode (dev) mounts -> unmounts -> remounts; closing the
    // throwaway first shell makes the backend emit `exit` on that first
    // channel. Guard so the surviving component doesn't auto-leave terminal
    // mode, and so we never write to a disposed terminal.
    let disposed = false;
    // Inject a queued `!cmd` only after the shell has printed its first prompt
    // (first output). Writing it before the prompt races shell startup: the
    // kernel tty echoes the raw input, then the shell's readline redraws it at
    // the prompt, so the command appears twice.
    let pendingInitial: string | null = null;

    const channel = new Channel<TerminalEvent>();
    channel.onmessage = (event) => {
      if (disposed) return;
      if (event.type === 'output') {
        term.write(base64ToBytes(event.dataB64));
        if (!shellReadyRef.current) {
          shellReadyRef.current = true;
          if (pendingInitial) {
            const cmd = pendingInitial;
            pendingInitial = null;
            writeToShell(cmd);
          }
        }
      } else if (event.type === 'exit') {
        const code = event.code;
        term.write(
          `\r\n\x1b[33m[process exited${code != null ? ` (code ${code})` : ''}]\x1b[0m\r\n`
        );
        // The shell is gone (e.g. the user typed `exit`); tell the parent so it
        // drops this session from the kept-alive set and leaves terminal mode,
        // unless this instance was already torn down.
        window.setTimeout(() => {
          if (!disposed) onShellExitRef.current();
        }, 600);
      }
    };

    void (async () => {
      try {
        const id = await invoke<string>('terminal_open', {
          workspaceId,
          cwd: null,
          cols: term.cols,
          rows: term.rows,
          onEvent: channel,
        });
        if (disposed) {
          void invoke('terminal_close', { sessionId: id });
          return;
        }
        sessionRef.current = id;
        term.focus();
        term.onData((d) => {
          void invoke('terminal_write', { sessionId: id, data: d });
        });
        term.onResize(({ cols, rows }) => {
          void invoke('terminal_resize', { sessionId: id, cols, rows });
        });
        // The real-size fit may have landed before this session id existed
        // (the open IPC is async); re-sync now so the PTY isn't stuck at the
        // tiny startup grid.
        syncSizeToPty();
        // `!cmd` fast-path on a fresh shell: queue the command and run it once
        // the shell is ready (first prompt). If the prompt already arrived, send
        // now. (An already-running kept-alive shell is fed by the show effect.)
        const initial = consumeRef.current?.();
        if (initial) {
          if (shellReadyRef.current) writeToShell(initial);
          else pendingInitial = initial;
        }
      } catch (err) {
        term.write(`\r\n\x1b[31m[failed to open terminal: ${String(err)}]\x1b[0m\r\n`);
      }
    })();

    // Refit when the card resizes (window resize, side panels shifting the
    // conversation column, or entering/leaving fullscreen) and push the new
    // size to the PTY so TUIs reflow.
    const refit = () => syncSizeToPty();
    window.addEventListener('resize', refit);
    const resizeObserver = new ResizeObserver(refit);
    resizeObserver.observe(container);

    return () => {
      disposed = true;
      window.removeEventListener('resize', refit);
      resizeObserver.disconnect();
      termRef.current = null;
      fitRef.current = null;
      const id = sessionRef.current;
      // We only unmount on a real teardown (shell exited, or the app is
      // closing) — never on hide — so closing the backend session here is
      // correct: it drops the (already-dead) session from the registry.
      if (id) {
        void invoke('terminal_close', { sessionId: id });
      }
      term.dispose();
    };
  }, [workspaceId, syncSizeToPty]);

  // When this terminal becomes visible again, its xterm may have been laid out
  // at zero size while hidden — refit and refocus once layout settles. Also
  // deliver any queued `!cmd` to an ALREADY-RUNNING shell here (a fresh mount
  // uses the first-prompt path above instead; gating on shellReady keeps the
  // two from double-sending).
  useEffect(() => {
    if (!visible) return undefined;
    const raf = requestAnimationFrame(() => {
      syncSizeToPty();
      termRef.current?.focus();
      if (shellReadyRef.current && sessionRef.current) {
        const cmd = consumeRef.current?.();
        if (cmd) writeToShell(cmd);
      }
    });
    return () => cancelAnimationFrame(raf);
  }, [visible, syncSizeToPty]);

  // Entering/leaving fullscreen changes the card geometry; refit so both the
  // xterm grid and the PTY winsize match. The rAF catches the snapped height;
  // the post-transition timeout catches the animated width (.terminal
  // transitions `width`) in case the webview coalesces away the ResizeObserver
  // settle tick. Only the on-screen terminal can measure a real size.
  useEffect(() => {
    if (!visible) return undefined;
    const raf = requestAnimationFrame(syncSizeToPty);
    const settle = window.setTimeout(syncSizeToPty, 320);
    return () => {
      cancelAnimationFrame(raf);
      window.clearTimeout(settle);
    };
  }, [fullscreen, visible, syncSizeToPty]);

  return (
    <div
      className={`${styles.panel} ${fullscreen ? styles.panelFullscreen : ''}`}
      style={visible ? undefined : { display: 'none' }}
    >
      <button
        type="button"
        className={styles.iconFloat}
        onClick={onToggleFullscreen}
        title={fullscreen ? 'Exit fullscreen (Ctrl+Shift+Enter)' : 'Fullscreen (Ctrl+Shift+Enter)'}
        aria-label={fullscreen ? 'Exit fullscreen' : 'Fullscreen'}
      >
        {fullscreen ? '⤡' : '⤢'}
      </button>
      <button
        type="button"
        className={styles.exitFloat}
        onClick={() => onBackToChatRef.current()}
        title="Back to chat (Ctrl+\)"
        aria-label="Back to chat"
      >
        Chat ⌄
      </button>
      <div ref={containerRef} className={styles.term} />
    </div>
  );
};

export default WorkspaceTerminal;
