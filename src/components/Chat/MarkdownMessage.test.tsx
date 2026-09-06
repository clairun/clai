import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';

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

import MarkdownMessage from './MarkdownMessage';

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

  it('keeps ordinary and external images as <img>', () => {
    render(
      <MarkdownMessage
        content={'![photo](shots/a.png)\n\n![remote](https://example.com/chart.vl.json)'}
      />,
    );
    expect(screen.queryByTestId('vega-chart-mock')).not.toBeInTheDocument();
    expect(screen.getByAltText('photo')).toHaveAttribute('src', 'shots/a.png');
    expect(screen.getByAltText('remote')).toHaveAttribute('src', 'https://example.com/chart.vl.json');
  });
});
