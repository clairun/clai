import React, { memo, useEffect, useMemo, useRef, useState } from 'react';
import type { EmbedOptions, Result, VisualizationSpec } from 'vega-embed';
import type { Loader } from 'vega';
import { readWorkspaceFileBase64 } from '../../workspace/client';
import { openExternal } from '../../utils/openExternal';
import { base64ToText, isWorkspaceRelativeHref, resolveWorkspacePath } from '../../utils/htmlBundle';
import { useWorkspaceFileLocation, type WorkspaceFileLocation } from './WorkspaceFileContext';
import { useAppTheme } from './useAppTheme';
import { buildVegaConfig, readChartThemeTokens } from './vegaTheme';
import { withChartInteractions } from './vegaInteractions';
import styles from './VegaChart.module.css';

/**
 * VegaChart
 *
 * Renders a Vega-Lite spec as an interactive chart. The spec comes either
 * inline (a ```vega-lite fenced block: `source`) or from a `.vl.json` file
 * in the workspace (`specPath`, used by `![title](charts/x.vl.json)` image
 * links and by the artifact preview).
 *
 * Behavior mirrors MermaidDiagram:
 *   - While the source is incomplete (streaming) or fails to parse, the raw
 *     JSON is shown; once a render succeeds the chart replaces it.
 *   - When streaming ends with a spec that still doesn't render, the error
 *     and the raw JSON are shown.
 *   - Styling is injected by the host (see vegaTheme.ts) and follows the app
 *     theme live. A re-render (theme flip, spec file change) is drawn into a
 *     hidden sibling and swapped in only when it succeeds, so the visible
 *     chart never blanks or collapses and a failed re-render keeps the
 *     previous one.
 *
 * Data: a spec's `data.url` is resolved relative to the enclosing document
 * (chat → workspace root, artifact → the file's directory, `.vl.json` link →
 * the spec file's directory) and read through the workspace file API, since
 * the WebView has no origin the relative URL could resolve against.
 * `http(s)` URLs are fetched as usual.
 *
 * vega-embed is imported dynamically so code paths that never see a chart
 * don't pay for it; with the singlefile build the chunk is inlined anyway.
 */

type EmbedFn = (typeof import('vega-embed'))['default'];

interface VegaRuntime {
  embed: EmbedFn;
  /** Vega's default loader; supplies the URL sanitizer we delegate to. */
  createLoader: () => Loader;
}

let runtimePromise: Promise<VegaRuntime> | null = null;
const loadVega = (): Promise<VegaRuntime> => {
  if (!runtimePromise) {
    runtimePromise = import('vega-embed').then((m) => ({
      embed: m.default,
      createLoader: () => m.vega.loader(),
    }));
  }
  return runtimePromise;
};

// Debounce while streaming: mid-arrival JSON is almost never valid, and a
// Vega-Lite compile + render is expensive enough not to attempt per chunk.
const STREAMING_RENDER_DEBOUNCE_MS = 250;

const isPlainObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

/**
 * Whether a workspace path (or link target) names a Vega-Lite spec file.
 * The one place this rule lives on the frontend; the backend mirrors it in
 * `viewer_for_path` (`.vl.json` → viewer "vega-lite"). Ignores any
 * `?query`/`#fragment`, case-insensitive.
 */
export const isVegaLiteSpecPath = (path: string): boolean =>
  /\.vl\.json$/i.test(path.replace(/[?#].*$/, ''));

/**
 * Read a workspace text file for chart use. Goes through the bytes endpoint
 * rather than `readWorkspaceFile`: the text endpoint silently truncates at
 * 200 KB (a preview convenience), which would chart a prefix of a large CSV
 * as if it were the whole dataset; the bytes endpoint rejects oversize files
 * outright (10 MB) so the failure is visible.
 */
const readWorkspaceText = async (workspaceId: string, path: string): Promise<string> =>
  base64ToText((await readWorkspaceFileBase64(workspaceId, path)).base64);

/**
 * Unit and layered specs without an explicit width fill the card, so charts
 * size to the chat column / preview panel instead of Vega-Lite's fixed
 * default. Composed specs (facet/concat/repeat) don't support "container".
 */
export const withContainerWidth = (spec: Record<string, unknown>): Record<string, unknown> => {
  const isSingleView = 'mark' in spec || 'layer' in spec;
  if (!isSingleView || spec.width !== undefined) return spec;
  return { ...spec, width: 'container' };
};

/**
 * A Vega data loader that serves relative `data.url`s from the workspace.
 * `baseFilePath` is the document the URL is relative to. Only `load` is ours;
 * URL sanitizing is delegated to Vega's default loader (`base`), which is
 * what rejects `javascript:` and other disallowed schemes.
 *
 * `href` marks: on click Vega sanitizes the URL with `context: 'href'`, then
 * dispatches a synthetic click on a *detached* `<a>` built from the result
 * (vega-scenegraph `Handler.handleHref`). A detached anchor never reaches the
 * app's link interception, so in the WebView the click opened nothing. The
 * loader therefore opens allowed `http(s)` hrefs itself through
 * `openExternal` and rejects, which `handleHref` swallows — Vega builds no
 * anchor at all. Only click handling uses this context (the SVG *string*
 * renderer's `sanitizeURL` does too, but the live renderer is `svg`).
 */
export const makeWorkspaceLoader = (
  location: WorkspaceFileLocation | null,
  baseFilePath: string,
  base: Loader,
  onLoadError?: (error: Error) => void,
): Loader => {
  const http = async (uri: string, options?: Partial<RequestInit>): Promise<string> => {
    const response = await fetch(uri, options);
    if (!response.ok) {
      throw new Error(`Failed to load ${uri}: HTTP ${response.status}`);
    }
    return response.text();
  };
  return {
    load: async (uri) => {
      try {
        if (/^https?:/i.test(uri)) return await http(uri);
        if (!isWorkspaceRelativeHref(uri)) {
          throw new Error(`Unsupported data URL: ${uri}`);
        }
        if (!location) {
          throw new Error(`Cannot load ${uri}: no workspace to resolve it in`);
        }
        const path = resolveWorkspacePath(baseFilePath, uri);
        return await readWorkspaceText(location.workspaceId, path);
      } catch (error) {
        const loadError = error instanceof Error ? error : new Error(String(error));
        onLoadError?.(loadError);
        throw loadError;
      }
    },
    sanitize: async (uri, options) => {
      const result = await base.sanitize(uri, options);
      if (options?.context !== 'href') return result;
      if (!/^https?:\/\//i.test(result.href)) {
        throw new Error(`Unsupported link target: ${result.href}`);
      }
      void openExternal(result.href);
      throw new Error(`Opened externally: ${result.href}`);
    },
    http,
    file: async (filename) => {
      throw new Error(`File loading is not available: ${filename}`);
    },
  };
};

interface VegaChartProps {
  /** JSON text of a Vega-Lite spec (from a fenced block). */
  source?: string;
  /**
   * Path of a `.vl.json` spec file, relative to the enclosing document
   * (see WorkspaceFileContext). Mutually exclusive with `source`.
   */
  specPath?: string;
  isStreaming?: boolean;
}

interface LoadedSpecFile {
  path: string;
  text: string | null;
  error: string | null;
}

const VegaChart = memo(({ source, specPath, isStreaming = false }: VegaChartProps) => {
  const location = useWorkspaceFileLocation();
  const appTheme = useAppTheme();
  const hostRef = useRef<HTMLDivElement | null>(null);
  // The live vega view (finalize() on replace/unmount) and the element it
  // was rendered into. Kept in refs: they are DOM bookkeeping, not render
  // state.
  const currentRef = useRef<{
    result: Result; el: HTMLElement; resetEvent: string | null;
  } | null>(null);
  const [interactionState, setInteractionState] = useState({
    legendFocus: false, canPanZoom: false, unavailable: false,
  });
  const [panZoomActive, setPanZoomActive] = useState(false);
  const [rendered, setRendered] = useState(false);
  // Parse/render failure for a specific spec text; ignored once the text
  // changes so a switched spec link never shows the previous one's error.
  const [renderError, setRenderError] = useState<{ text: string; message: string } | null>(null);
  const [specFile, setSpecFile] = useState<LoadedSpecFile | null>(null);

  // Where the spec lives, for resolving its `data.url`: the enclosing
  // document for inline specs, the spec file itself for `.vl.json` links.
  const resolvedSpecPath = useMemo(() => {
    if (!specPath) return null;
    return resolveWorkspacePath(location?.basePath ?? '', specPath);
  }, [specPath, location?.basePath]);
  const dataBasePath = resolvedSpecPath ?? location?.basePath ?? '';

  // Load a referenced spec file. Inline sources need no fetch.
  useEffect(() => {
    if (!specPath || resolvedSpecPath === null || !location) return undefined;
    let cancelled = false;
    readWorkspaceText(location.workspaceId, resolvedSpecPath)
      .then((content) => {
        if (!cancelled) setSpecFile({ path: resolvedSpecPath, text: content, error: null });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setSpecFile({
          path: resolvedSpecPath,
          text: null,
          error: err instanceof Error ? err.message : String(err),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [specPath, resolvedSpecPath, location]);

  const fileIsCurrent = specFile !== null && specFile.path === resolvedSpecPath;
  const text: string | null = specPath
    ? fileIsCurrent ? specFile.text : null
    : source ?? null;
  // A spec link outside any workspace context can never resolve.
  const error = renderError !== null && renderError.text === text ? renderError.message : null;
  const fileError = specPath
    ? !location
      ? `Cannot load ${specPath}: no workspace to read it from`
      : fileIsCurrent ? specFile.error : null
    : null;

  useEffect(() => {
    if (text === null) return undefined;
    const host = hostRef.current;
    if (!host) return undefined;

    let cancelled = false;
    const timer = setTimeout(async () => {
      const fail = (err: unknown) => {
        if (!cancelled) setRenderError({ text, message: err instanceof Error ? err.message : String(err) });
      };
      let spec: unknown;
      try {
        spec = JSON.parse(text);
      } catch (err) {
        fail(err);
        return;
      }
      if (!isPlainObject(spec)) {
        fail('A Vega-Lite spec must be a JSON object');
        return;
      }

      // Render into a hidden sibling: the previous chart stays up (no blank,
      // no height collapse) until the new one is ready, and survives if the
      // new one fails.
      const target = document.createElement('div');
      target.className = styles.pending ?? '';
      host.appendChild(target);
      try {
        const { embed, createLoader } = await loadVega();
        if (cancelled) {
          target.remove();
          return;
        }
        let loadError: Error | null = null;
        const options: EmbedOptions = {
          actions: false,
          renderer: 'svg',
          config: buildVegaConfig(readChartThemeTokens(), spec),
          loader: makeWorkspaceLoader(location, dataBasePath, createLoader(), (error) => {
            loadError ??= error;
          }),
          tooltip: { theme: appTheme },
        };
        const baseSpec = withContainerWidth(spec);
        const interactions = withChartInteractions(baseSpec);
        const renderSpec = async (candidate: Record<string, unknown>): Promise<Result> => {
          loadError = null;
          const rendered = await embed(target, candidate as VisualizationSpec, options);
          if (loadError) {
            rendered.finalize();
            throw loadError;
          }
          return rendered;
        };
        let result: Result;
        let unavailable = false;
        try {
          result = await renderSpec(interactions.spec);
        } catch (err) {
          // A data failure belongs to the authored chart, not to our automatic
          // interaction parameters. Retrying the base spec would only repeat
          // the same read before showing the error.
          if (err === loadError || interactions.spec === baseSpec || cancelled) throw err;
          // Automatic conveniences must not prevent an otherwise valid chart
          // from rendering. Retry the untouched spec, then expose its error if
          // that also fails. Do not advertise controls that did not render.
          // vega-embed exposes no View when it rejects. It cannot be finalized
          // here if failure happened after View construction (an upstream limit).
          target.replaceChildren();
          result = await renderSpec(baseSpec);
          unavailable = true;
        }
        if (cancelled) {
          result.finalize();
          target.remove();
          return;
        }
        if (currentRef.current) {
          currentRef.current.result.finalize();
          currentRef.current.el.remove();
        }
        // Only drop our own marker: vega-embed tags the target with its
        // `vega-embed` class, which the stylesheet relies on.
        if (styles.pending) target.classList.remove(styles.pending);
        currentRef.current = { result, el: target, resetEvent: unavailable ? null : interactions.resetEvent };
        setInteractionState({
          legendFocus: !unavailable && interactions.legendFocus,
          canPanZoom: !unavailable && interactions.canPanZoom,
          unavailable,
        });
        setPanZoomActive(false);
        setRendered(true);
        setRenderError(null);
      } catch (err) {
        target.remove();
        fail(err);
      }
    }, isStreaming ? STREAMING_RENDER_DEBOUNCE_MS : 0);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [text, isStreaming, appTheme, location, dataBasePath]);

  // Release the vega view on unmount.
  useEffect(
    () => () => {
      if (currentRef.current) {
        currentRef.current.result.finalize();
        currentRef.current.el.remove();
        currentRef.current = null;
      }
    },
    [],
  );

  // Block only our automatic Vega gesture handlers. stopPropagation leaves
  // browser scrolling/text selection intact; authored interactions bypass it.
  const gatePanZoom = (event: React.SyntheticEvent) => {
    if (interactionState.canPanZoom && !panZoomActive) event.stopPropagation();
  };

  const resetChart = () => {
    const chart = currentRef.current;
    if (!chart?.resetEvent) return;
    // Native selection clearing keeps the displayed View, its loaded data
    // and current dimensions, even when a newer source failed to render.
    chart.el.querySelector('svg')?.dispatchEvent(new Event(chart.resetEvent));
    setPanZoomActive(false);
  };

  const finalError = fileError ?? (isStreaming ? null : error);
  const showFallback = !rendered && !finalError;

  // Errors on final content are always reported; when a previous chart is
  // still up (a failed re-render) it stays visible above the message, and
  // the source is only repeated when there is no chart to look at.
  return (
    <div className={styles.card}>
      <div
        ref={hostRef}
        className={styles.chart}
        data-testid="vega-chart"
        data-pan-zoom={panZoomActive || undefined}
        onWheelCapture={gatePanZoom}
        onPointerDownCapture={gatePanZoom}
      />
      {rendered && (interactionState.legendFocus || interactionState.canPanZoom) && (
        <div className={styles.toolbar} role="group" aria-label="Chart interactions">
          <span className={styles.hint}>
            {panZoomActive
              ? 'Drag to pan · Scroll to zoom'
              : interactionState.legendFocus ? 'Click legend to focus · Shift-click for multiple' : 'Enable pan & zoom to explore'}
          </span>
          {interactionState.canPanZoom && (
            <button
              type="button"
              className={styles.control}
              aria-pressed={panZoomActive}
              onClick={() => setPanZoomActive((active) => !active)}
            >
              Pan &amp; zoom
            </button>
          )}
          <button
            type="button"
            className={styles.control}
            onClick={resetChart}
          >
            Reset chart
          </button>
        </div>
      )}
      {rendered && interactionState.unavailable && (
        <div className={styles.hint}>Automatic interactions unavailable for this chart.</div>
      )}
      {finalError && (
        <div className={styles.errorContainer}>
          <div className={styles.errorLabel}>Vega-Lite chart failed to render</div>
          <div className={styles.errorMessage}>{finalError}</div>
          {source !== undefined && !rendered && (
            <pre className={styles.codeFallback}>
              <code>{source}</code>
            </pre>
          )}
        </div>
      )}
      {showFallback && (source !== undefined ? (
        <pre className={styles.codeFallback}>
          <code>{source}</code>
        </pre>
      ) : (
        <div className={styles.loading}>Loading chart…</div>
      ))}
    </div>
  );
});

VegaChart.displayName = 'VegaChart';

export default VegaChart;
