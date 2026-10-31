import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';

// The chart renderer has its own tests; here we only care that markdown
// routes the right constructs to it with the right props.
vi.mock('./VegaChart', async (importOriginal) => ({
  // Keep the real path predicate; only the renderer is stubbed.
  ...(await importOriginal<typeof import('./VegaChart')>()),
  default: ({ source, specPath, isStreaming }: { source?: string; specPath?: string; isStreaming?: boolean }) => (
    <div
      data-testid="vega-chart-mock"
      data-source={source ?? ''}
      data-spec-path={specPath ?? ''}
      data-streaming={String(!!isStreaming)}
    />
  ),
}));

const readWorkspaceFileBase64Mock = vi.fn();
vi.mock('../../workspace/client', () => ({
  readWorkspaceFileBase64: (...args: unknown[]) => readWorkspaceFileBase64Mock(...args),
}));

import MarkdownMessage from './MarkdownMessage';
import { WorkspaceFileContext, type WorkspaceFileLocation } from './WorkspaceFileContext';

const SPEC = '{"mark": "bar", "data": {"url": "sales.csv"}}';

describe('MarkdownMessage — Vega-Lite embedding', () => {
  it('renders a ```vega-lite fenced block as a chart and forwards the streaming flag', () => {
    render(<MarkdownMessage content={`Intro\n\n\`\`\`vega-lite\n${SPEC}\n\`\`\`\n`} isStreaming />);
    const chart = screen.getByTestId('vega-chart-mock');
    expect(chart.dataset.source).toBe(SPEC);
    expect(chart.dataset.specPath).toBe('');
    expect(chart.dataset.streaming).toBe('true');
  });

  it('leaves other fenced blocks (including json) as highlighted code', () => {
    render(<MarkdownMessage content={`\`\`\`json\n${SPEC}\n\`\`\`\n`} />);
    expect(screen.queryByTestId('vega-chart-mock')).not.toBeInTheDocument();
    expect(screen.getByText(/"mark"/)).toBeInTheDocument();
  });

  it('renders an image link to a workspace .vl.json as a chart, outside any <p>', () => {
    render(<MarkdownMessage content={'Before\n\n![Q3 revenue](charts/q3.vl.json)\n\nAfter'} />);
    const chart = screen.getByTestId('vega-chart-mock');
    expect(chart.dataset.specPath).toBe('charts/q3.vl.json');
    expect(chart.dataset.source).toBe('');
    // A block element is not valid inside <p>; the chart's paragraph becomes a <div>.
    expect(chart.closest('p')).toBeNull();
    expect(chart.parentElement?.tagName).toBe('DIV');
    expect(screen.getByText('Before').tagName).toBe('P');
    expect(screen.getByText('After').tagName).toBe('P');
  });

  it('also swaps the paragraph for a div when the chart link shares it with text', () => {
    render(<MarkdownMessage content="See ![Q3](charts/q3.vl.json) below" />);
    const chart = screen.getByTestId('vega-chart-mock');
    expect(chart.closest('p')).toBeNull();
    const wrapper = chart.parentElement;
    expect(wrapper?.tagName).toBe('DIV');
    expect(wrapper?.textContent).toContain('See');
    expect(wrapper?.textContent).toContain('below');
  });

  it('matches the extension case-insensitively and ignores a query/fragment', () => {
    render(<MarkdownMessage content="![Q3](Charts/Q3.VL.JSON#top)" />);
    expect(screen.getByTestId('vega-chart-mock').dataset.specPath).toBe('Charts/Q3.VL.JSON#top');
  });

  it('unwraps a link around a chart so chart interactions cannot navigate', () => {
    render(<MarkdownMessage content="[![Q3](charts/q3.vl.json)](charts/q3.vl.json)" />);
    const chart = screen.getByTestId('vega-chart-mock');
    expect(chart.closest('p')).toBeNull();
    expect(chart.closest('a')).toBeNull();
  });

  it('keeps ordinary Markdown links clickable and opens them safely', () => {
    render(<MarkdownMessage content="Read the [Vega-Lite docs](https://vega.github.io/vega-lite/)" />);
    expect(screen.getByRole('link', { name: 'Vega-Lite docs' })).toHaveAttribute(
      'href',
      'https://vega.github.io/vega-lite/',
    );
    expect(screen.getByRole('link', { name: 'Vega-Lite docs' })).toHaveAttribute('target', '_blank');
    expect(screen.getByRole('link', { name: 'Vega-Lite docs' })).toHaveAttribute(
      'rel',
      'noopener noreferrer',
    );
  });

  it('leaves an external .vl.json image as a plain <img>', () => {
    render(<MarkdownMessage content="![remote](https://example.com/chart.vl.json)" />);
    expect(screen.queryByTestId('vega-chart-mock')).not.toBeInTheDocument();
    expect(screen.getByAltText('remote')).toHaveAttribute('src', 'https://example.com/chart.vl.json');
  });
});

describe('MarkdownMessage — workspace images', () => {
  const CHAT: WorkspaceFileLocation = { workspaceId: 'ws-1', basePath: '' };

  const renderIn = (location: WorkspaceFileLocation | null, content: string) =>
    render(
      <WorkspaceFileContext.Provider value={location}>
        <MarkdownMessage content={content} />
      </WorkspaceFileContext.Provider>,
    );

  beforeEach(() => {
    readWorkspaceFileBase64Mock.mockReset();
  });

  it('loads a relative image from the workspace as a data URL', async () => {
    readWorkspaceFileBase64Mock.mockResolvedValue({ path: 'shots/a.png', mime: 'image/png', base64: 'UE5H' });
    renderIn(CHAT, '![photo](shots/a.png)');

    expect(screen.getByText('Loading image…')).toBeInTheDocument();
    expect(await screen.findByAltText('photo')).toHaveAttribute('src', 'data:image/png;base64,UE5H');
    expect(readWorkspaceFileBase64Mock).toHaveBeenCalledWith('ws-1', 'shots/a.png');
  });

  it('resolves against the enclosing document, with a leading / meaning the workspace root', async () => {
    readWorkspaceFileBase64Mock.mockResolvedValue({ path: '', mime: 'image/webp', base64: 'V0VCUA==' });
    renderIn({ workspaceId: 'ws-1', basePath: 'reports/weekly.md' }, '![a](img/1.webp) ![b](/shots/2.webp)');

    await screen.findByAltText('a');
    await screen.findByAltText('b');
    expect(readWorkspaceFileBase64Mock).toHaveBeenCalledWith('ws-1', 'reports/img/1.webp');
    expect(readWorkspaceFileBase64Mock).toHaveBeenCalledWith('ws-1', 'shots/2.webp');
  });

  it('leaves http(s) and data: images untouched', () => {
    renderIn(CHAT, '![remote](https://example.com/a.png) ![inline](data:image/gif;base64,R0lG)');

    expect(screen.getByAltText('remote')).toHaveAttribute('src', 'https://example.com/a.png');
    // react-markdown's URL sanitizer drops `data:` before it reaches us.
    expect(screen.getByAltText('inline')).not.toHaveAttribute('src');
    expect(readWorkspaceFileBase64Mock).not.toHaveBeenCalled();
  });

  it('shows the read error instead of a broken image', async () => {
    readWorkspaceFileBase64Mock.mockRejectedValue('File not found: shots/missing.png');
    renderIn(CHAT, '![gone](shots/missing.png)');

    expect(await screen.findByText('File not found: shots/missing.png')).toBeInTheDocument();
    expect(screen.getByText('Image failed to load')).toBeInTheDocument();
    expect(screen.queryByAltText('gone')).not.toBeInTheDocument();
  });

  it('reports bytes the WebView cannot decode as an image', async () => {
    readWorkspaceFileBase64Mock.mockResolvedValue({ path: '', mime: 'application/octet-stream', base64: 'eA==' });
    renderIn(CHAT, '![notes](notes.bin)');

    fireEvent.error(await screen.findByAltText('notes'));
    expect(screen.getByText('notes.bin is not a displayable image')).toBeInTheDocument();
  });

  it('explains why a relative image cannot load outside a workspace', () => {
    renderIn(null, '![photo](shots/a.png)');

    expect(screen.getByText('Cannot load shots/a.png: no workspace to read it from')).toBeInTheDocument();
    expect(readWorkspaceFileBase64Mock).not.toHaveBeenCalled();
  });

  it('zooms a workspace image on click and closes on Escape', async () => {
    readWorkspaceFileBase64Mock.mockResolvedValue({ path: 'shots/a.png', mime: 'image/png', base64: 'UE5H' });
    renderIn(CHAT, '![photo](shots/a.png)');

    fireEvent.click(await screen.findByRole('button', { name: 'photo' }));
    expect(screen.getByRole('dialog', { name: 'photo' })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

describe('MarkdownMessage — workspace file links', () => {
  const openFile = vi.fn();
  const CHAT: WorkspaceFileLocation = { workspaceId: 'ws-1', basePath: '', rootPath: '/home/u/ws', openFile };

  const renderIn = (location: WorkspaceFileLocation | null, content: string) =>
    render(
      <WorkspaceFileContext.Provider value={location}>
        <MarkdownMessage content={content} />
      </WorkspaceFileContext.Provider>,
    );

  beforeEach(() => {
    openFile.mockReset();
    readWorkspaceFileBase64Mock.mockReset();
  });

  it.each([
    ['an absolute path inside the workspace', '[bisect.sh](/home/u/ws/scratch/bisect.sh)', 'scratch/bisect.sh'],
    ['a path relative to the workspace root', '[bisect.sh](scratch/bisect.sh)', 'scratch/bisect.sh'],
    ['a /-rooted path', '[bisect.sh](/scratch/bisect.sh)', 'scratch/bisect.sh'],
    ['a percent-encoded path with a fragment', '[bisect.sh](<scratch/mac dns/bisect.sh#L3>)', 'scratch/mac dns/bisect.sh'],
  ])('opens %s in the preview instead of navigating', (_case, content, expected) => {
    renderIn(CHAT, content);
    const link = screen.getByRole('link', { name: 'bisect.sh' });

    expect(link).not.toHaveAttribute('target');
    // fireEvent returns false when the default (WebView navigation) was prevented.
    expect(fireEvent.click(link)).toBe(false);
    expect(openFile).toHaveBeenCalledWith(expected);
  });

  it.each([
    ['[code](/home/u/ws/src/a.ts:78)', 'src/a.ts', 78],
    ['[code](</home/u/ws/src/my file.ts:12>)', 'src/my file.ts', 12],
    ['[code](src/a.ts:3)', 'src/a.ts', 3],
  ])('opens a local link at its source line: %s', (content, path, line) => {
    renderIn(CHAT, content);
    expect(fireEvent.click(screen.getByRole('link', { name: 'code' }))).toBe(false);
    expect(openFile).toHaveBeenCalledWith(path, line);
  });

  it('keeps malformed line suffixes as ordinary file names', () => {
    renderIn(CHAT, '[code](src/a.ts:0)');
    fireEvent.click(screen.getByRole('link', { name: 'code' }));
    expect(openFile).toHaveBeenCalledWith('src/a.ts:0');
  });

  it('opens a filename ending in digits when its colon is percent-encoded', () => {
    renderIn(CHAT, '[report](/home/u/ws/reports/final%3A2026)');
    fireEvent.click(screen.getByRole('link', { name: 'report' }));
    expect(openFile).toHaveBeenCalledWith('reports/final:2026');
  });

  it('resolves a relative link against the previewed document', () => {
    renderIn({ ...CHAT, basePath: 'reports/weekly.md' }, '[q3](q3.md)');
    fireEvent.click(screen.getByRole('link', { name: 'q3' }));
    expect(openFile).toHaveBeenCalledWith('reports/q3.md');
  });

  it('keeps a path outside the workspace inside the workspace, where the preview reports it missing', () => {
    renderIn(CHAT, '[hosts](/etc/hosts)');
    expect(fireEvent.click(screen.getByRole('link', { name: 'hosts' }))).toBe(false);
    expect(openFile).toHaveBeenCalledWith('etc/hosts');
  });

  it('renders a workspace link as plain text when nothing can open it', () => {
    renderIn({ workspaceId: 'ws-1', basePath: '' }, '[bisect.sh](scratch/bisect.sh)');
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
    expect(screen.getByText('bisect.sh')).toHaveAttribute('title', 'scratch/bisect.sh');
  });

  it('keeps http(s) and mailto links external', () => {
    renderIn(CHAT, '[site](https://example.com/a) [mail](mailto:a@example.com)');
    expect(screen.getByRole('link', { name: 'site' })).toHaveAttribute('target', '_blank');
    expect(screen.getByRole('link', { name: 'mail' })).toHaveAttribute('href', 'mailto:a@example.com');
    fireEvent.click(screen.getByRole('link', { name: 'site' }));
    expect(openFile).not.toHaveBeenCalled();
  });

  it('loads an image written as an absolute path inside the workspace', async () => {
    readWorkspaceFileBase64Mock.mockResolvedValue({ path: 'shot.png', mime: 'image/png', base64: 'UE5H' });
    renderIn(CHAT, '![shot](/home/u/ws/shot.png)');
    expect(await screen.findByAltText('shot')).toHaveAttribute('src', 'data:image/png;base64,UE5H');
    expect(readWorkspaceFileBase64Mock).toHaveBeenCalledWith('ws-1', 'shot.png');
  });
});
