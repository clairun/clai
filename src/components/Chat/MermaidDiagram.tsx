import React, { memo, useEffect, useState } from 'react';
import styles from './MermaidDiagram.module.css';

/**
 * MermaidDiagram
 *
 * Renders a ```mermaid fenced code block as an SVG diagram.
 *
 * Behavior:
 *   - While the source is incomplete (streaming) or fails to parse, the raw
 *     code is shown in a plain block; once a parse succeeds the SVG replaces
 *     it. Subsequent parse failures during streaming keep the last good SVG
 *     instead of flickering back to text.
 *   - When streaming ends with source that still doesn't parse, the error
 *     and the raw code are shown so the user can see what was wrong.
 *   - The mermaid theme follows the app theme (data-theme on <html>) and
 *     diagrams re-render live when it changes.
 *
 * Mermaid is imported dynamically so test environments (jsdom) and code
 * paths that never see a mermaid block don't pay for it. With the
 * singlefile build the chunk is inlined anyway, so there's no runtime
 * fetch in Flatpak/WebKitGTK.
 */

type MermaidAPI = (typeof import('mermaid'))['default'];

let mermaidPromise: Promise<MermaidAPI> | null = null;
const loadMermaid = (): Promise<MermaidAPI> => {
  if (!mermaidPromise) {
    mermaidPromise = import('mermaid').then((m) => m.default);
  }
  return mermaidPromise;
};

const resolveAppTheme = (): 'light' | 'dark' =>
  document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light';

// mermaid's theme is global initialize() state, not per-render; track what
// we last initialized with and re-initialize only on change.
let initializedTheme: string | null = null;
const ensureTheme = (mermaid: MermaidAPI, theme: 'light' | 'dark'): void => {
  if (initializedTheme === theme) return;
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    theme: theme === 'dark' ? 'dark' : 'neutral',
    fontFamily: 'inherit',
  });
  initializedTheme = theme;
};

// mermaid.render() requires a document-unique element id per call.
let renderSeq = 0;

// Debounce while streaming: mid-arrival source is usually invalid, and
// parse+render is expensive enough that we don't want it per keystroke.
const STREAMING_RENDER_DEBOUNCE_MS = 250;

interface MermaidDiagramProps {
  code: string;
  isStreaming?: boolean;
}

const MermaidDiagram = memo(({ code, isStreaming = false }: MermaidDiagramProps) => {
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [appTheme, setAppTheme] = useState<'light' | 'dark'>(resolveAppTheme);

  // Follow live theme switches (Settings toggle / OS change with "system").
  useEffect(() => {
    const observer = new MutationObserver(() => setAppTheme(resolveAppTheme()));
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(
      async () => {
        try {
          const mermaid = await loadMermaid();
          if (cancelled) return;
          ensureTheme(mermaid, appTheme);
          // Validate before render: a failed render() can leave error
          // artifacts in the DOM, parse() fails cleanly.
          await mermaid.parse(code);
          const { svg: rendered } = await mermaid.render(`mermaid-diagram-${++renderSeq}`, code);
          if (cancelled) return;
          setSvg(rendered);
          setError(null);
        } catch (err) {
          if (cancelled) return;
          setError(err instanceof Error ? err.message : String(err));
        }
      },
      isStreaming ? STREAMING_RENDER_DEBOUNCE_MS : 0
    );
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [code, isStreaming, appTheme]);

  // Final content that doesn't parse: show the error + raw source.
  if (error && !isStreaming && !svg) {
    return (
      <div className={styles.errorContainer}>
        <div className={styles.errorLabel}>Mermaid diagram failed to render</div>
        <div className={styles.errorMessage}>{error}</div>
        <pre className={styles.codeFallback}>
          <code>{code}</code>
        </pre>
      </div>
    );
  }

  // Last good SVG (kept through transient parse failures while streaming).
  if (svg) {
    return (
      <div
        className={styles.container}
        data-testid="mermaid-diagram"
        // Safe: this is mermaid.render() output, sanitized internally
        // (securityLevel: strict), never raw user/LLM markup.
        dangerouslySetInnerHTML={{ __html: svg }}
      />
    );
  }

  // Nothing rendered yet — show the source as a plain code block.
  return (
    <pre className={styles.codeFallback}>
      <code>{code}</code>
    </pre>
  );
});

MermaidDiagram.displayName = 'MermaidDiagram';

export default MermaidDiagram;
