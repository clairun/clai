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
vi.mock('../Chat/MarkdownMessage', () => ({
  default: ({ content }: { content: string }) => <div data-testid="markdown">{content}</div>,
}));
vi.mock('../Chat/StreamingMarkdown', () => ({
  default: ({ content }: { content: string }) => <div data-testid="streaming">{content}</div>,
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
