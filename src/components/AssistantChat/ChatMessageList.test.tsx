import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';

// VirtualizedList windows its children by scroll geometry, which jsdom
// doesn't have (zero heights). Replace it with a plain list that renders
// every item, so we exercise ChatMessageList's grouping/segmenting logic
// rather than the virtualizer.
vi.mock('../common/VirtualizedList', () => ({
  default: <T,>({
    items,
    renderItem,
    itemKey,
  }: {
    items: T[];
    renderItem: (item: T, index: number) => React.ReactNode;
    itemKey: (item: T) => string;
  }) => (
    <div data-testid="virtual-list">
      {items.map((item, index) => (
        <div key={itemKey(item)}>{renderItem(item, index)}</div>
      ))}
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

import ChatMessageList from './ChatMessageList';
import type { AssistantMessage, ToolInvocation } from '../../generated/bindings';

const msg = (
  over: Partial<AssistantMessage> & Pick<AssistantMessage, 'id' | 'role' | 'content'>,
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

  it('collapses many consecutive tool calls into a summary group', () => {
    const content: AssistantMessage['content'] = Array.from({ length: 3 }, (_, i) => ({
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
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    // The multi-tool group shows a "N tool calls" summary row.
    expect(screen.getByText('3 tool calls')).toBeInTheDocument();
  });
});
