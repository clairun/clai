import React, { useEffect, useRef, useState } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import '@xterm/xterm/css/xterm.css';
import styles from './TerminalSpike.module.css';

/**
 * Phase 1 perf spike for the integrated terminal — see
 * `terminal-feature-design.md`. This is an ISOLATED dev route
 * (`/_terminal-spike`), not wired into the production UI. Its only job is to
 * answer the existential question: can xterm.js + a PTY stay fast in WebKitGTK
 * (Tauri's Linux webview) under flood output?
 *
 * The toolbar buttons fire representative workloads (`yes` flood, large
 * base64 dump, recursive ls) and the readout shows bytes received + elapsed so
 * you can eyeball throughput and responsiveness on native Linux AND a Flatpak
 * build. The agent runs headless and cannot measure this — a human must.
 */

type TerminalEvent =
  | { type: 'output'; dataB64: string }
  | { type: 'exit'; code: number | null };

function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const len = binary.length;
  const bytes = new Uint8Array(len);
  for (let i = 0; i < len; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

const TerminalSpike: React.FC = () => {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const sessionRef = useRef<string | null>(null);
  const bytesRef = useRef(0);
  const floodStartRef = useRef<number | null>(null);
  const rendererRef = useRef<string>('pending');
  const statusRef = useRef<string>('starting…');

  const [renderer, setRenderer] = useState<string>('pending');
  const [bytes, setBytes] = useState(0);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [status, setStatus] = useState<string>('starting…');

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return undefined;

    const term = new Terminal({
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
      fontSize: 13,
      scrollback: 8000,
      cursorBlink: true,
      theme: { background: '#0b0e14', foreground: '#cbd5e1' },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);

    // Prefer the WebGL renderer (fastest); fall back to the default DOM
    // renderer if the WebKit webview can't give us a GL context.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        webgl.dispose();
        rendererRef.current = 'dom (webgl context lost)';
      });
      term.loadAddon(webgl);
      rendererRef.current = 'webgl';
    } catch {
      rendererRef.current = 'dom (no webgl)';
    }

    fit.fit();
    termRef.current = term;

    const channel = new Channel<TerminalEvent>();
    channel.onmessage = (event) => {
      if (event.type === 'output') {
        const data = base64ToBytes(event.dataB64);
        bytesRef.current += data.length;
        term.write(data);
      } else if (event.type === 'exit') {
        const code = event.code;
        term.write(
          `\r\n\x1b[33m[process exited${code != null ? ` (code ${code})` : ''}]\x1b[0m\r\n`,
        );
        statusRef.current = 'shell exited';
      }
    };

    let disposed = false;
    void (async () => {
      try {
        const id = await invoke<string>('terminal_open', {
          workspaceId: null,
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
        statusRef.current = 'connected';
        term.focus();
        term.onData((d) => {
          void invoke('terminal_write', { sessionId: id, data: d });
        });
        term.onResize(({ cols, rows }) => {
          void invoke('terminal_resize', { sessionId: id, cols, rows });
        });
      } catch (err) {
        statusRef.current = `failed: ${String(err)}`;
        term.write(`\r\n\x1b[31m[terminal_open failed: ${String(err)}]\x1b[0m\r\n`);
      }
    })();

    const onWindowResize = () => fit.fit();
    window.addEventListener('resize', onWindowResize);

    // Sample the throughput counters at a low rate (the heavy work is in
    // term.write, driven by the channel — this is just the readout).
    const statsTimer = window.setInterval(() => {
      setBytes(bytesRef.current);
      setRenderer(rendererRef.current);
      setStatus(statusRef.current);
      if (floodStartRef.current != null) {
        setElapsedMs(performance.now() - floodStartRef.current);
      }
    }, 200);

    return () => {
      disposed = true;
      window.removeEventListener('resize', onWindowResize);
      window.clearInterval(statsTimer);
      const id = sessionRef.current;
      if (id) {
        void invoke('terminal_close', { sessionId: id });
      }
      term.dispose();
    };
  }, []);

  const run = (cmd: string) => {
    const id = sessionRef.current;
    if (!id) return;
    bytesRef.current = 0;
    setBytes(0);
    floodStartRef.current = performance.now();
    setElapsedMs(0);
    void invoke('terminal_write', { sessionId: id, data: `${cmd}\n` });
  };

  return (
    <div className={styles.wrap}>
      <div className={styles.toolbar}>
        <strong className={styles.title}>Terminal perf spike</strong>
        <span className={styles.stat}>renderer: <b>{renderer}</b></span>
        <span className={styles.stat}>status: {status}</span>
        <span className={styles.stat}>bytes: {bytes.toLocaleString()}</span>
        <span className={styles.stat}>
          elapsed: {(elapsedMs / 1000).toFixed(2)}s
        </span>
        <span className={styles.stat}>
          rate:{' '}
          {elapsedMs > 0 ? `${((bytes / 1024 / 1024) / (elapsedMs / 1000)).toFixed(1)} MB/s` : '—'}
        </span>
        <span className={styles.spacer} />
        <button className={styles.btn} onClick={() => run('yes "the quick brown fox 0123456789" | head -n 200000')}>
          flood 200k lines
        </button>
        <button className={styles.btn} onClick={() => run('head -c 5000000 /dev/urandom | base64')}>
          5MB base64
        </button>
        <button className={styles.btn} onClick={() => run('ls -laR / 2>/dev/null | tail -n 50')}>
          ls -laR /
        </button>
        <button className={styles.btn} onClick={() => run('clear')}>
          clear
        </button>
        <button
          className={styles.btn}
          onClick={() => {
            bytesRef.current = 0;
            setBytes(0);
            floodStartRef.current = null;
            setElapsedMs(0);
            termRef.current?.clear();
          }}
        >
          reset stats
        </button>
      </div>
      <div ref={containerRef} className={styles.term} />
    </div>
  );
};

export default TerminalSpike;
