import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';

// vega-embed does real layout; mock it so we exercise this component's
// state machine (parse → embed → swap/fallback) and the options it passes.
const embedMock = vi.fn();
vi.mock('vega-embed', () => ({
  default: (...args: unknown[]) => embedMock(...args),
}));

const readWorkspaceFileMock = vi.fn();
vi.mock('../../workspace/client', () => ({
  readWorkspaceFile: (...args: unknown[]) => readWorkspaceFileMock(...args),
}));

import VegaChart, { makeWorkspaceLoader, withContainerWidth } from './VegaChart';
import { WorkspaceFileContext } from './WorkspaceFileContext';

const SPEC = { mark: 'bar', data: { values: [{ a: 1 }] }, encoding: { x: { field: 'a' } } };
const SOURCE = JSON.stringify(SPEC);
const LOCATION = { workspaceId: 'ws-1', basePath: 'reports/q3.md' };

const finalizeMock = vi.fn();

// The shape of the vega-embed call we assert on.
interface CapturedOptions {
  actions: unknown;
  renderer: unknown;
  tooltip: unknown;
  config: { range: { category: unknown }; mark: { tooltip: unknown } };
  loader: { load: (uri: string) => Promise<string> };
}
type EmbedCall = [HTMLElement, Record<string, unknown>, CapturedOptions];
const embedCall = (index: number): EmbedCall => embedMock.mock.calls[index] as EmbedCall;

// Simulate vega-embed: it renders into the target element and returns a
// handle whose finalize() releases the view.
const fakeEmbed = async (el: HTMLElement) => {
  const view = document.createElement('div');
  view.className = 'vega-embed';
  view.textContent = 'chart';
  el.appendChild(view);
  return { finalize: finalizeMock, view: {} };
};

// Let a streaming debounce window elapse (inside act: the effect may set state).
const settle = (ms: number) => act(() => new Promise<void>((resolve) => setTimeout(resolve, ms)));

beforeEach(() => {
  embedMock.mockReset().mockImplementation(fakeEmbed);
  finalizeMock.mockReset();
  readWorkspaceFileMock.mockReset();
  document.documentElement.setAttribute('data-theme', 'light');
});

const renderedCharts = () =>
  screen.getByTestId('vega-chart').querySelectorAll('.vega-embed').length;

describe('withContainerWidth', () => {
  it('fills the container for unit and layered specs without a width', () => {
    expect(withContainerWidth({ mark: 'bar' })).toEqual({ mark: 'bar', width: 'container' });
    expect(withContainerWidth({ layer: [] })).toEqual({ layer: [], width: 'container' });
  });

  it('respects an explicit width and leaves composed specs alone', () => {
    expect(withContainerWidth({ mark: 'bar', width: 200 })).toEqual({ mark: 'bar', width: 200 });
    expect(withContainerWidth({ hconcat: [] })).toEqual({ hconcat: [] });
    expect(withContainerWidth({ facet: {}, spec: {} })).toEqual({ facet: {}, spec: {} });
  });
});

describe('makeWorkspaceLoader', () => {
  it('reads relative data URLs from the workspace, relative to the spec file', async () => {
    readWorkspaceFileMock.mockResolvedValue({ content: 'a,b\n1,2', viewer: 'text', path: '' });
    const loader = makeWorkspaceLoader(LOCATION, 'reports/charts/q3.vl.json');
    await expect(loader.load('../sales.csv')).resolves.toBe('a,b\n1,2');
    expect(readWorkspaceFileMock).toHaveBeenCalledWith('ws-1', 'reports/sales.csv');
  });

  it('fetches http(s) URLs and rejects other schemes', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200, text: async () => 'x,y' });
    vi.stubGlobal('fetch', fetchMock);
    try {
      const loader = makeWorkspaceLoader(LOCATION, '');
      await expect(loader.load('https://example.com/d.csv')).resolves.toBe('x,y');
      expect(fetchMock).toHaveBeenCalledWith('https://example.com/d.csv', undefined);
      await expect(loader.load('data:text/csv,a')).rejects.toThrow(/Unsupported data URL/);
      expect(readWorkspaceFileMock).not.toHaveBeenCalled();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('surfaces a non-2xx response as an error', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 404, text: async () => '' }));
    try {
      const loader = makeWorkspaceLoader(LOCATION, '');
      await expect(loader.load('https://example.com/missing.csv')).rejects.toThrow(/HTTP 404/);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('rejects relative URLs when there is no workspace to resolve them in', async () => {
    const loader = makeWorkspaceLoader(null, '');
    await expect(loader.load('sales.csv')).rejects.toThrow(/no workspace/);
    expect(readWorkspaceFileMock).not.toHaveBeenCalled();
  });
});

describe('VegaChart (inline source)', () => {
  it('renders the parsed spec through vega-embed with the host theme and no action menu', async () => {
    render(<VegaChart source={SOURCE} />);
    await waitFor(() => expect(renderedCharts()).toBe(1));

    const [target, spec, options] = embedCall(0);
    expect(screen.getByTestId('vega-chart').contains(target)).toBe(true);
    expect(spec).toEqual({ ...SPEC, width: 'container' });
    expect(options.actions).toBe(false);
    expect(options.renderer).toBe('svg');
    expect(Array.isArray(options.config.range.category)).toBe(true);
    expect(options.config.mark.tooltip).toBe(true);
    expect(typeof options.loader.load).toBe('function');
    // The raw-source fallback is gone once the chart is up.
    expect(screen.queryByText(SOURCE)).not.toBeInTheDocument();
  });

  it('shows the raw source until the chart renders', () => {
    embedMock.mockReturnValue(new Promise(() => {}));
    render(<VegaChart source={SOURCE} />);
    expect(screen.getByText(SOURCE)).toBeInTheDocument();
    expect(renderedCharts()).toBe(0);
  });

  it('shows the error and the source when final content is not valid JSON', async () => {
    render(<VegaChart source={'{"mark": "bar",'} />);
    await screen.findByText('Vega-Lite chart failed to render');
    expect(screen.getByText('{"mark": "bar",')).toBeInTheDocument();
    expect(embedMock).not.toHaveBeenCalled();
  });

  it('rejects a JSON value that is not an object', async () => {
    render(<VegaChart source="[1, 2]" />);
    await screen.findByText('A Vega-Lite spec must be a JSON object');
    expect(embedMock).not.toHaveBeenCalled();
  });

  it('shows the error when vega-embed rejects the spec', async () => {
    embedMock.mockRejectedValue(new Error('Invalid field type "quantitative2"'));
    render(<VegaChart source={SOURCE} />);
    await screen.findByText('Invalid field type "quantitative2"');
    // The failed attempt's element was cleaned up.
    expect(screen.getByTestId('vega-chart').childElementCount).toBe(0);
  });

  it('keeps showing the source, without an error, while streaming content does not parse', async () => {
    render(<VegaChart source={'{"mark": "ba'} isStreaming />);
    await settle(350);
    expect(screen.getByText('{"mark": "ba')).toBeInTheDocument();
    expect(screen.queryByText('Vega-Lite chart failed to render')).not.toBeInTheDocument();
    expect(embedMock).not.toHaveBeenCalled();
  });

  it('keeps the last good chart when a streaming update fails to parse', async () => {
    const { rerender } = render(<VegaChart source={SOURCE} />);
    await waitFor(() => expect(renderedCharts()).toBe(1));

    rerender(<VegaChart source={`${SOURCE.slice(0, -1)}, "enc`} isStreaming />);
    await settle(350);
    expect(renderedCharts()).toBe(1);
    expect(finalizeMock).not.toHaveBeenCalled();
    expect(screen.queryByText('Vega-Lite chart failed to render')).not.toBeInTheDocument();
  });

  it('replaces the chart (and releases the old view) when the spec changes', async () => {
    const { rerender } = render(<VegaChart source={SOURCE} />);
    await waitFor(() => expect(renderedCharts()).toBe(1));

    const next = JSON.stringify({ ...SPEC, mark: 'line' });
    rerender(<VegaChart source={next} />);
    await waitFor(() => expect(embedMock).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(finalizeMock).toHaveBeenCalledTimes(1));
    expect(renderedCharts()).toBe(1);
    expect(embedCall(1)[1].mark).toBe('line');
  });

  it('re-renders with the new tooltip theme when the app theme flips', async () => {
    render(<VegaChart source={SOURCE} />);
    await waitFor(() => expect(embedMock).toHaveBeenCalledTimes(1));
    expect(embedCall(0)[2].tooltip).toEqual({ theme: 'light' });

    document.documentElement.setAttribute('data-theme', 'dark');
    await waitFor(() => expect(embedMock).toHaveBeenCalledTimes(2));
    expect(embedCall(1)[2].tooltip).toEqual({ theme: 'dark' });
  });

  it('releases the vega view on unmount', async () => {
    const { unmount } = render(<VegaChart source={SOURCE} />);
    await waitFor(() => expect(renderedCharts()).toBe(1));
    unmount();
    expect(finalizeMock).toHaveBeenCalledTimes(1);
  });

  it('resolves inline-spec data relative to the enclosing document', async () => {
    readWorkspaceFileMock.mockResolvedValue({ content: 'a\n1', viewer: 'text', path: '' });
    render(
      <WorkspaceFileContext.Provider value={LOCATION}>
        <VegaChart source={SOURCE} />
      </WorkspaceFileContext.Provider>,
    );
    await waitFor(() => expect(embedMock).toHaveBeenCalledTimes(1));
    const options = embedCall(0)[2];
    await options.loader.load('data/sales.csv');
    expect(readWorkspaceFileMock).toHaveBeenCalledWith('ws-1', 'reports/data/sales.csv');
  });
});

describe('VegaChart (spec file)', () => {
  it('reads the .vl.json relative to the document and resolves its data relative to the spec', async () => {
    readWorkspaceFileMock.mockImplementation(async (_ws: string, path: string) => {
      if (path === 'reports/charts/q3.vl.json') {
        return { content: SOURCE, viewer: 'vega-lite', path };
      }
      return { content: 'a\n1', viewer: 'text', path };
    });
    render(
      <WorkspaceFileContext.Provider value={LOCATION}>
        <VegaChart specPath="charts/q3.vl.json" />
      </WorkspaceFileContext.Provider>,
    );
    await waitFor(() => expect(renderedCharts()).toBe(1));
    expect(readWorkspaceFileMock).toHaveBeenCalledWith('ws-1', 'reports/charts/q3.vl.json');

    const [, spec, options] = embedCall(0);
    expect(spec.mark).toBe('bar');
    await options.loader.load('../sales.csv');
    expect(readWorkspaceFileMock).toHaveBeenCalledWith('ws-1', 'reports/sales.csv');
  });

  it('shows a loading placeholder, not raw source, before the file arrives', () => {
    readWorkspaceFileMock.mockReturnValue(new Promise(() => {}));
    render(
      <WorkspaceFileContext.Provider value={LOCATION}>
        <VegaChart specPath="charts/q3.vl.json" />
      </WorkspaceFileContext.Provider>,
    );
    expect(screen.getByText('Loading chart…')).toBeInTheDocument();
  });

  it('shows the read error when the spec file cannot be loaded', async () => {
    readWorkspaceFileMock.mockRejectedValue(new Error('File not found: charts/q3.vl.json'));
    render(
      <WorkspaceFileContext.Provider value={LOCATION}>
        <VegaChart specPath="charts/q3.vl.json" />
      </WorkspaceFileContext.Provider>,
    );
    await screen.findByText('Vega-Lite chart failed to render');
    expect(screen.getByText('File not found: charts/q3.vl.json')).toBeInTheDocument();
    expect(embedMock).not.toHaveBeenCalled();
  });

  it('explains when there is no workspace to read the spec from', async () => {
    render(<VegaChart specPath="charts/q3.vl.json" />);
    await screen.findByText(/no workspace to read it from/);
    expect(readWorkspaceFileMock).not.toHaveBeenCalled();
  });

  it('reloads when the referenced path changes', async () => {
    readWorkspaceFileMock.mockResolvedValue({ content: SOURCE, viewer: 'vega-lite', path: '' });
    const { rerender } = render(
      <WorkspaceFileContext.Provider value={LOCATION}>
        <VegaChart specPath="charts/q3.vl.json" />
      </WorkspaceFileContext.Provider>,
    );
    await waitFor(() => expect(embedMock).toHaveBeenCalledTimes(1));
    rerender(
      <WorkspaceFileContext.Provider value={LOCATION}>
        <VegaChart specPath="charts/q4.vl.json" />
      </WorkspaceFileContext.Provider>,
    );
    await waitFor(() => expect(readWorkspaceFileMock).toHaveBeenCalledWith('ws-1', 'reports/charts/q4.vl.json'));
    await waitFor(() => expect(embedMock).toHaveBeenCalledTimes(2));
  });
});
