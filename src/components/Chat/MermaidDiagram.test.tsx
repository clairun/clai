import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';

// Mermaid does real SVG layout (getBBox etc.) which jsdom can't do; mock
// the module so we exercise this component's state machine, not mermaid.
const parseMock = vi.fn();
const renderMock = vi.fn();
const initializeMock = vi.fn();
vi.mock('mermaid', () => ({
  default: {
    initialize: (...args: unknown[]) => initializeMock(...args),
    parse: (...args: unknown[]) => parseMock(...args),
    render: (...args: unknown[]) => renderMock(...args),
  },
}));

import MermaidDiagram from './MermaidDiagram';

const VALID_CODE = 'graph TD\n  A --> B';
const SVG = '<svg><g>diagram</g></svg>';

beforeEach(() => {
  parseMock.mockReset().mockResolvedValue(undefined);
  renderMock.mockReset().mockResolvedValue({ svg: SVG });
  initializeMock.mockReset();
});

describe('MermaidDiagram', () => {
  it('renders the diagram SVG for valid mermaid source', async () => {
    render(<MermaidDiagram code={VALID_CODE} />);
    const diagram = await screen.findByTestId('mermaid-diagram');
    expect(diagram.innerHTML).toContain('diagram');
    expect(parseMock).toHaveBeenCalledWith(VALID_CODE);
  });

  it('shows the raw source before the render resolves', () => {
    // Never-resolving parse keeps the component in its fallback state.
    parseMock.mockReturnValue(new Promise(() => {}));
    render(<MermaidDiagram code={VALID_CODE} />);
    expect(screen.getByText(/graph TD/)).toBeInTheDocument();
    expect(screen.queryByTestId('mermaid-diagram')).not.toBeInTheDocument();
  });

  it('shows error + raw source when final content fails to parse', async () => {
    parseMock.mockRejectedValue(new Error('Parse error on line 2'));
    render(<MermaidDiagram code="graph TD\n  A -->" isStreaming={false} />);
    await waitFor(() => {
      expect(screen.getByText('Mermaid diagram failed to render')).toBeInTheDocument();
    });
    expect(screen.getByText('Parse error on line 2')).toBeInTheDocument();
    expect(renderMock).not.toHaveBeenCalled();
  });

  it('keeps the last good SVG when streaming content transiently fails to parse', async () => {
    const { rerender } = render(<MermaidDiagram code={VALID_CODE} />);
    await screen.findByTestId('mermaid-diagram');

    parseMock.mockRejectedValue(new Error('mid-stream truncation'));
    rerender(<MermaidDiagram code={`${VALID_CODE}\n  B --`} isStreaming />);

    // Debounced attempt fails, but the previous diagram stays up.
    await waitFor(() => expect(parseMock).toHaveBeenCalledTimes(2), { timeout: 2000 });
    expect(screen.getByTestId('mermaid-diagram')).toBeInTheDocument();
    expect(screen.queryByText('Mermaid diagram failed to render')).not.toBeInTheDocument();
  });
});
