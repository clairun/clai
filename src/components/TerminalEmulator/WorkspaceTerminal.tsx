import React, { useEffect, useRef, useState } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import '@xterm/xterm/css/xterm.css';
import styles from './WorkspaceTerminal.module.css';

/**
 * Integrated terminal panel (Phase 2 of the integrated-terminal feature — see
 * terminal-feature-design.md). A real PTY-backed shell at the workspace's
 * directory, rendered with xterm.js. Mounting opens a fresh shell; unmounting
 * kills it (decision #4: spawn-on-enter / kill-on-exit, no state recovery).
 *
 * The PTY backend + 16ms output coalescing live in
 * `src-tauri/src/commands/terminal.rs` (validated by the Phase 1 perf spike).
 * This component is the production surface that replaces the chat composer
 * while terminal mode is active.
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
  /** Called to leave terminal mode (exit button or when the shell exits). */
  onExit: () => void;
}

const WorkspaceTerminal: React.FC<WorkspaceTerminalProps> = ({ workspaceId, onExit }) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const sessionRef = useRef<string | null>(null);
  // Keep the latest onExit without re-running the setup effect (which would
  // tear down and respawn the shell on every parent render).
  const onExitRef = useRef(onExit);
  useEffect(() => {
    onExitRef.current = onExit;
  }, [onExit]);

  const [status, setStatus] = useState<'starting' | 'connected' | 'exited' | 'error'>('starting');

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

    // Prefer the WebGL renderer (fastest); fall back to the DOM renderer if
    // the WebKit webview can't provide a GL context.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* DOM renderer fallback — still correct, just slower. */
    }

    fit.fit();

    // React StrictMode (dev) mounts -> unmounts -> remounts; closing the
    // throwaway first shell makes the backend emit `exit` on that first
    // channel. Guard so the surviving component doesn't auto-leave terminal
    // mode, and so we never write to a disposed terminal.
    let disposed = false;

    const channel = new Channel<TerminalEvent>();
    channel.onmessage = (event) => {
      if (disposed) return;
      if (event.type === 'output') {
        term.write(base64ToBytes(event.dataB64));
      } else if (event.type === 'exit') {
        const code = event.code;
        term.write(
          `\r\n\x1b[33m[process exited${code != null ? ` (code ${code})` : ''}]\x1b[0m\r\n`
        );
        setStatus('exited');
        // The shell is gone (e.g. the user typed `exit`); leave terminal mode,
        // unless this instance was already torn down.
        window.setTimeout(() => {
          if (!disposed) onExitRef.current();
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
        setStatus('connected');
        term.focus();
        term.onData((d) => {
          void invoke('terminal_write', { sessionId: id, data: d });
        });
        term.onResize(({ cols, rows }) => {
          void invoke('terminal_resize', { sessionId: id, cols, rows });
        });
      } catch (err) {
        setStatus('error');
        term.write(`\r\n\x1b[31m[failed to open terminal: ${String(err)}]\x1b[0m\r\n`);
      }
    })();

    // Refit when the card resizes (window resize, or side panels shifting the
    // conversation column width).
    const refit = () => {
      try {
        fit.fit();
      } catch {
        /* container detached mid-teardown */
      }
    };
    window.addEventListener('resize', refit);
    const resizeObserver = new ResizeObserver(refit);
    resizeObserver.observe(container);

    return () => {
      disposed = true;
      window.removeEventListener('resize', refit);
      resizeObserver.disconnect();
      const id = sessionRef.current;
      if (id) {
        void invoke('terminal_close', { sessionId: id });
      }
      term.dispose();
    };
  }, [workspaceId]);

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <span className={styles.prompt}>{'>_'}</span>
        <span className={styles.title}>Terminal</span>
        <span className={styles.dot} data-status={status} />
        <span className={styles.hint}>workspace shell</span>
        <span className={styles.spacer} />
        <button
          type="button"
          className={styles.exitBtn}
          onClick={onExit}
          title="Back to chat (Ctrl+`)"
        >
          Chat ⌄
        </button>
      </div>
      <div ref={containerRef} className={styles.term} />
    </div>
  );
};

export default WorkspaceTerminal;
