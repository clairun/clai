import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

// VirtualizedList windows its children by scroll geometry, which jsdom
// doesn't have (zero heights). Replace it with a plain list that renders
// every item, so we exercise ChatMessageList's grouping/segmenting logic
// rather than the virtualizer.
vi.mock('../common/VirtualizedList', () => ({
  default: <T,>({
    items,
    renderItem,
    itemKey,
    footer,
  }: {
    items: T[];
    renderItem: (item: T, index: number) => React.ReactNode;
    itemKey: (item: T) => string;
    footer?: React.ReactNode;
  }) => (
    <div data-testid="virtual-list">
      {items.map((item, index) => (
        <div key={itemKey(item)}>{renderItem(item, index)}</div>
      ))}
      {footer}
    </div>
  ),
}));

// MarkdownMessage / StreamingMarkdown render markdown via heavy deps
// (react-markdown, prism). For these tests we only care that the text
// reaches the DOM, so render it plainly.
vi.mock('../Chat/MarkdownMessage', async () => {
  const { useWorkspaceFileLocation } = await import('../Chat/WorkspaceFileContext');
  const MarkdownMock = ({ content }: { content: string }) => {
    // Expose the workspace location markdown would resolve relative links
    // (`.vl.json` charts, `data.url`) against.
    const location = useWorkspaceFileLocation();
    return (
      <div data-testid="markdown" data-workspace={location?.workspaceId ?? ''} data-base={location?.basePath ?? ''}>
        {content}
      </div>
    );
  };
  return { default: MarkdownMock };
});
vi.mock('../Chat/StreamingMarkdown', () => ({
  default: ({ content }: { content: string }) => <div data-testid="streaming">{content}</div>,
}));
// VegaChart pulls in vega-embed; the list only needs to hand it the path.
vi.mock('../Chat/VegaChart', () => ({
  default: ({ specPath }: { specPath?: string }) => (
    <div data-testid="vega-chart" data-spec-path={specPath ?? ''} />
  ),
}));

// ImageAttachment loads bytes from the workspace image store; stub the fetch
// so the transcript renders a data-URL thumbnail without a real backend.
vi.mock('../../workspace/client', () => ({
  readWorkspaceFileBase64: vi.fn(async () => ({
    path: '.clai/images/abc.png',
    mime: 'image/png',
    base64: 'QUJD',
  })),
}));

import ChatMessageList from './ChatMessageList';
import type { AssistantMessage, ToolInvocation } from '../../generated/bindings';

const msg = (
  over: Partial<AssistantMessage> & Pick<AssistantMessage, 'id' | 'role' | 'content'>
): AssistantMessage => ({
  sessionId: 'sess-1',
  createdAt: 0n,
  providerMetadata: null,
  ...over,
});

describe('ChatMessageList', () => {
  it('renders a user message and an assistant text reply', () => {
    const messages: AssistantMessage[] = [
      msg({ id: 'm1', role: 'user', content: [{ type: 'text', text: 'hello there' }] }),
      msg({ id: 'm2', role: 'assistant', content: [{ type: 'text', text: 'general kenobi' }] }),
    ];
    render(<ChatMessageList messages={messages} userLabel="You" />);
    expect(screen.getByText('hello there')).toBeInTheDocument();
    expect(screen.getByText('general kenobi')).toBeInTheDocument();
    expect(screen.getByText('You')).toBeInTheDocument();
    expect(screen.getByText('Clai')).toBeInTheDocument();
  });

  it('renders a single tool call with its (cleaned) name and result', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [
          {
            type: 'tool_use',
            tool_call_id: 'tc-1',
            tool_name: 'mcp.abc123.get_metric_data',
            arguments: {},
          },
        ],
      }),
    ];
    const toolCalls: ToolInvocation[] = [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        toolName: 'mcp.abc123.get_metric_data',
        params: {},
        status: 'completed',
        result: 'done',
        error: null,
        startedAt: 0n,
        completedAt: 1n,
      },
    ];
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    // cleanToolName strips the mcp.<id>. prefix.
    expect(screen.getByText('get_metric_data')).toBeInTheDocument();
  });

  const chartCall = (over: Partial<ToolInvocation>): [AssistantMessage[], ToolInvocation[]] => [
    [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [
          { type: 'tool_use', tool_call_id: 'tc-1', tool_name: 'create_vega_chart', arguments: {} },
        ],
      }),
    ],
    [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        toolName: 'create_vega_chart',
        params: { title: 'Q3 Revenue', spec: {} },
        status: 'completed',
        result: { ok: true, path: 'charts/q3-revenue.vl.json', display: true },
        error: null,
        startedAt: 0n,
        completedAt: 1n,
        ...over,
      },
    ],
  ];

  it('renders the chart a completed create_vega_chart call produced, under its row', () => {
    const [messages, toolCalls] = chartCall({});
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} workspaceId="ws-1" />);
    expect(screen.getByText('Chart')).toBeInTheDocument();
    expect(screen.getByText('Q3 Revenue')).toBeInTheDocument();
    expect(screen.getByTestId('vega-chart')).toHaveAttribute('data-spec-path', 'charts/q3-revenue.vl.json');
  });

  it('renders no chart for a failed call or one with display:false', () => {
    const [failedMessages, failedCalls] = chartCall({
      status: 'failed',
      result: null,
      error: 'The spec is not a valid Vega-Lite chart',
    });
    const { unmount } = render(<ChatMessageList messages={failedMessages} toolCalls={failedCalls} />);
    expect(screen.queryByTestId('vega-chart')).toBeNull();
    unmount();

    const [hiddenMessages, hiddenCalls] = chartCall({
      result: { ok: true, path: 'charts/q3-revenue.vl.json', display: false },
    });
    render(<ChatMessageList messages={hiddenMessages} toolCalls={hiddenCalls} />);
    expect(screen.queryByTestId('vega-chart')).toBeNull();
  });

  it('renders a collapsed thinking block', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [
          { type: 'thinking', text: 'let me reason about this' },
          { type: 'text', text: 'the answer is 42' },
        ],
      }),
    ];
    render(<ChatMessageList messages={messages} />);
    expect(screen.getByText('Thinking')).toBeInTheDocument();
    expect(screen.getByText('the answer is 42')).toBeInTheDocument();
  });

  it('hides scheduled-run boundary marker messages', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'user',
        content: [{ type: 'text', text: '--- New scheduled run at 12:00' }],
      }),
      msg({ id: 'm2', role: 'assistant', content: [{ type: 'text', text: 'visible reply' }] }),
    ];
    render(<ChatMessageList messages={messages} />);
    expect(screen.queryByText(/New scheduled run/)).toBeNull();
    expect(screen.getByText('visible reply')).toBeInTheDocument();
  });

  const toolGroup = (
    count: number
  ): { messages: AssistantMessage[]; toolCalls: ToolInvocation[] } => {
    const content: AssistantMessage['content'] = Array.from({ length: count }, (_, i) => ({
      type: 'tool_use' as const,
      tool_call_id: `tc-${i}`,
      tool_name: `tool_${i}`,
      arguments: {},
    }));
    const messages: AssistantMessage[] = [msg({ id: 'm1', role: 'assistant', content })];
    const toolCalls: ToolInvocation[] = content.map((part) => ({
      id: (part as { tool_call_id: string }).tool_call_id,
      runId: 'r-1',
      sessionId: 'sess-1',
      toolName: 'x',
      params: {},
      status: 'completed',
      result: 'ok',
      error: null,
      startedAt: 0n,
      completedAt: 1n,
    }));
    return { messages, toolCalls };
  };

  it('renders each tool as its own one-line row when under the cap', () => {
    const { messages, toolCalls } = toolGroup(3);
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('tool_0')).toBeInTheDocument();
    expect(screen.getByText('tool_2')).toBeInTheDocument();
    expect(screen.queryByText(/earlier/)).toBeNull();
  });

  it('caps a large tool group at 4 rows behind a "show earlier" toggle', () => {
    const { messages, toolCalls } = toolGroup(6);
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    // 6 - 4 = 2 hidden behind the toggle; only the last 4 render.
    expect(screen.getByText('Show 2 earlier calls')).toBeInTheDocument();
    expect(screen.queryByText('tool_0')).toBeNull();
    expect(screen.queryByText('tool_1')).toBeNull();
    expect(screen.getByText('tool_2')).toBeInTheDocument();
    expect(screen.getByText('tool_5')).toBeInTheDocument();
  });

  // A tool group where call `chartIndex` is a completed create_vega_chart;
  // every other call is a plain tool_N.
  const toolGroupWithChart = (
    count: number,
    chartIndex: number,
    display = true
  ): { messages: AssistantMessage[]; toolCalls: ToolInvocation[] } => {
    const { messages, toolCalls } = toolGroup(count);
    const part = messages[0]?.content[chartIndex] as { tool_name: string };
    part.tool_name = 'create_vega_chart';
    return {
      messages,
      toolCalls: toolCalls.map((tc, i) =>
        i === chartIndex
          ? {
              ...tc,
              toolName: 'create_vega_chart',
              params: { title: 'Q3 Revenue', spec: {} },
              result: { ok: true, path: 'charts/q3-revenue.vl.json', display },
            }
          : tc
      ),
    };
  };

  it('never collapses a displayed chart row; plain runs on each side collapse on their own', () => {
    // 7 plain, chart, 6 plain → "Show 3 earlier" + 4 rows, chart, "Show 2 earlier" + 4 rows.
    const { messages, toolCalls } = toolGroupWithChart(14, 7);
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} workspaceId="ws-1" />);
    expect(screen.getByTestId('vega-chart')).toHaveAttribute('data-spec-path', 'charts/q3-revenue.vl.json');
    expect(screen.getByText('Show 3 earlier calls')).toBeInTheDocument();
    expect(screen.getByText('Show 2 earlier calls')).toBeInTheDocument();
    expect(screen.queryByText('tool_2')).toBeNull();
    expect(screen.getByText('tool_3')).toBeInTheDocument();
    expect(screen.getByText('tool_6')).toBeInTheDocument();
    expect(screen.queryByText('tool_9')).toBeNull();
    expect(screen.getByText('tool_10')).toBeInTheDocument();
    expect(screen.getByText('tool_13')).toBeInTheDocument();

    // Each run toggles on its own: expanding the first leaves the second collapsed.
    fireEvent.click(screen.getByText('Show 3 earlier calls'));
    expect(screen.getByText('tool_0')).toBeInTheDocument();
    expect(screen.queryByText('tool_9')).toBeNull();
    expect(screen.getByText('Show 2 earlier calls')).toBeInTheDocument();
  });

  it('renders adjacent chart rows back to back with no empty run between them', () => {
    const { messages, toolCalls } = toolGroupWithChart(2, 0);
    const second = toolCalls[1];
    if (!second) throw new Error('expected two tool calls');
    (messages[0]?.content[1] as { tool_name: string }).tool_name = 'create_vega_chart';
    toolCalls[1] = {
      ...second,
      toolName: 'create_vega_chart',
      params: { title: 'Second', spec: {} },
      result: { ok: true, path: 'charts/second.vl.json', display: true },
    };
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} workspaceId="ws-1" />);
    const charts = screen.getAllByTestId('vega-chart');
    expect(charts.map((el) => el.getAttribute('data-spec-path'))).toEqual([
      'charts/q3-revenue.vl.json',
      'charts/second.vl.json',
    ]);
    expect(screen.queryByText(/earlier/)).toBeNull();
  });

  it('keeps a trailing chart row visible even when the run before it overflows', () => {
    const { messages, toolCalls } = toolGroupWithChart(6, 5);
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} workspaceId="ws-1" />);
    expect(screen.getByTestId('vega-chart')).toBeInTheDocument();
    // 5 plain rows before the chart → 1 hidden.
    expect(screen.getByText('Show 1 earlier call')).toBeInTheDocument();
    expect(screen.queryByText('tool_0')).toBeNull();
    expect(screen.getByText('tool_4')).toBeInTheDocument();
  });

  it('collapses a display:false chart call like any other row', () => {
    const { messages, toolCalls } = toolGroupWithChart(6, 0, false);
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} workspaceId="ws-1" />);
    expect(screen.queryByTestId('vega-chart')).toBeNull();
    expect(screen.getByText('Show 2 earlier calls')).toBeInTheDocument();
    expect(screen.queryByText('Q3 Revenue')).toBeNull();
  });

  it('shows a bash row summary (verb, command, exit code) without expanding', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [
          { type: 'tool_use', tool_call_id: 'tc-1', tool_name: 'bash_exec', arguments: {} },
        ],
      }),
    ];
    const toolCalls: ToolInvocation[] = [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        toolName: 'bash_exec',
        params: { command: 'npm run build' },
        status: 'completed',
        result: { exitCode: 0, stdout: 'Build complete', stderr: '' },
        error: null,
        startedAt: 0n,
        completedAt: 1n,
      },
    ];
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('Bash')).toBeInTheDocument();
    expect(screen.getByText('npm run build')).toBeInTheDocument();
    expect(screen.getByText('exit 0')).toBeInTheDocument();
  });

  it('reads a bash result delivered as an MCP content envelope', () => {
    // Claude Code reaches our built-ins through the local MCP server, so the
    // result is stored as the wire envelope rather than the tool's own JSON.
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [
          { type: 'tool_use', tool_call_id: 'tc-1', tool_name: 'bash_exec', arguments: {} },
        ],
      }),
    ];
    const toolCalls: ToolInvocation[] = [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        toolName: 'bash_exec',
        params: { command: 'ls' },
        status: 'completed',
        result: [
          {
            type: 'text',
            text: JSON.stringify({ exitCode: 0, stdout: 'Build complete', stderr: '' }),
          },
        ],
        error: null,
        startedAt: 0n,
        completedAt: 1n,
      },
    ];
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('exit 0')).toBeInTheDocument();

    fireEvent.click(screen.getByText('Bash'));
    // The terminal block shows the command's output, not the raw envelope.
    expect(screen.getByText('Build complete')).toBeInTheDocument();
  });

  it('shows file content from an MCP-enveloped fs_read result', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [{ type: 'tool_use', tool_call_id: 'tc-1', tool_name: 'fs_read', arguments: {} }],
      }),
    ];
    const toolCalls: ToolInvocation[] = [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        toolName: 'fs_read',
        params: { path: '/main.rs' },
        status: 'completed',
        result: [
          { type: 'text', text: JSON.stringify({ path: '/main.rs', content: 'fn main() {}' }) },
        ],
        error: null,
        startedAt: 0n,
        completedAt: 1n,
      },
    ];
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('1 line')).toBeInTheDocument();

    fireEvent.click(screen.getByText('Read'));
    // Fenced with the language guessed from the path, not dumped as JSON.
    const rendered = screen.getAllByTestId('markdown').map((el) => el.textContent ?? '');
    expect(rendered).toContain('```rust\nfn main() {}\n```');
  });

  it('pretty-prints an MCP-enveloped JSON result instead of one escaped line', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [{ type: 'tool_use', tool_call_id: 'tc-1', tool_name: 'fs_glob', arguments: {} }],
      }),
    ];
    const toolCalls: ToolInvocation[] = [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        toolName: 'fs_glob',
        params: { pattern: '**/*.rs' },
        status: 'completed',
        result: [{ type: 'text', text: '{"matches":[{"path":"/a.rs"}]}' }],
        error: null,
        startedAt: 0n,
        completedAt: 1n,
      },
    ];
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    fireEvent.click(screen.getByText('Glob'));
    const rendered = screen.getAllByTestId('markdown').map((el) => el.textContent ?? '');
    expect(rendered.some((text) => text.includes('```json\n{\n  "matches"'))).toBe(true);
  });

  it('hides the empty assistant placeholder until content or streaming text arrives', () => {
    // Each turn is seeded with an empty Text placeholder before anything
    // streams. Rendering it would create a zero-height virtual item whose
    // 0px measurement the virtualizer can't cache, leaving a phantom
    // estimate-sized gap above the running footer.
    const messages: AssistantMessage[] = [
      msg({ id: 'm1', role: 'user', content: [{ type: 'text', text: 'do the thing' }] }),
      msg({ id: 'm2', role: 'assistant', content: [{ type: 'text', text: '' }] }),
    ];
    const { rerender } = render(<ChatMessageList messages={messages} isStreaming />);
    expect(screen.getByTestId('virtual-list').children).toHaveLength(2); // user item + footer
    expect(screen.queryByText('Clai')).toBeNull();

    // First streamed delta for the placeholder makes it visible.
    rerender(<ChatMessageList messages={messages} isStreaming streamingText={{ m2: 'on it' }} />);
    expect(screen.getByText('on it')).toBeInTheDocument();
    expect(screen.getByText('Clai')).toBeInTheDocument();
  });

  it('renders an image-only user message as a thumbnail from the store', async () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'user',
        content: [
          {
            type: 'image',
            id: 'img-1',
            path: '.clai/images/abc.png',
            media_type: 'image/png',
            filename: 'shot.png',
          },
        ],
      }),
    ];
    render(<ChatMessageList messages={messages} workspaceId="ws-1" userLabel="You" />);
    // Image-only message is not hidden, and the thumbnail loads from the store.
    const img = await screen.findByAltText('shot.png');
    expect(img).toHaveAttribute('src', 'data:image/png;base64,QUJD');
  });

  it('provides the workspace (root-relative) to markdown so chart links resolve', () => {
    const messages: AssistantMessage[] = [
      msg({ id: 'u1', role: 'user', content: [{ type: 'text', text: 'See ![Q3](charts/q3.vl.json)' }] }),
    ];
    render(<ChatMessageList messages={messages} workspaceId="ws-1" userLabel="You" />);
    const markdown = screen.getByTestId('markdown');
    expect(markdown.dataset.workspace).toBe('ws-1');
    expect(markdown.dataset.base).toBe('');
  });

  it('provides no workspace location when workspaceId is absent', () => {
    const messages: AssistantMessage[] = [
      msg({ id: 'u1', role: 'user', content: [{ type: 'text', text: 'hello' }] }),
    ];
    render(<ChatMessageList messages={messages} userLabel="You" />);
    expect(screen.getByTestId('markdown').dataset.workspace).toBe('');
  });

  it('hides image parts when no workspaceId is provided', () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'user',
        content: [
          {
            type: 'image',
            id: 'img-1',
            path: '.clai/images/abc.png',
            media_type: 'image/png',
            filename: 'shot.png',
          },
        ],
      }),
    ];
    // No workspaceId → cannot resolve the store, so the image is not rendered.
    render(<ChatMessageList messages={messages} userLabel="You" />);
    expect(screen.queryByAltText('shot.png')).toBeNull();
  });

  it('opens a zoom lightbox when a transcript image is clicked, and closes on Escape', async () => {
    const messages: AssistantMessage[] = [
      msg({
        id: 'm1',
        role: 'user',
        content: [
          {
            type: 'image',
            id: 'img-1',
            path: '.clai/images/abc.png',
            media_type: 'image/png',
            filename: 'shot.png',
          },
        ],
      }),
    ];
    render(<ChatMessageList messages={messages} workspaceId="ws-1" userLabel="You" />);
    const thumb = await screen.findByAltText('shot.png');
    expect(screen.queryByRole('dialog')).toBeNull();

    fireEvent.click(thumb);
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toBeInTheDocument();

    fireEvent.keyDown(window, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('shows an elapsed timer in the running footer', () => {
    const messages: AssistantMessage[] = [
      msg({ id: 'm1', role: 'assistant', content: [{ type: 'text', text: 'working…' }] }),
    ];
    render(<ChatMessageList messages={messages} isStreaming runStartedAt={Date.now() - 8000} />);
    // An m:ss timer (~0:08), and no token count.
    expect(screen.getByText(/^0:0\d$/)).toBeInTheDocument();
    expect(screen.queryByText(/tokens/)).toBeNull();
  });
});
