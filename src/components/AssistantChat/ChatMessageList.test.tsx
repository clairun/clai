import { beforeEach, describe, expect, it, vi } from 'vitest';
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
vi.mock('../Chat/StreamingMarkdown', async () => {
  const { useWorkspaceFileLocation } = await import('../Chat/WorkspaceFileContext');
  const StreamingMock = ({ content }: { content: string }) => {
    const location = useWorkspaceFileLocation();
    return (
      <div data-testid="streaming" data-workspace={location?.workspaceId ?? ''}>
        {content}
      </div>
    );
  };
  return { default: StreamingMock };
});
// VegaChart pulls in vega-embed; stubbed so a stray chart card is detectable.
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

// Rows get only a compact view of each call; the full result is fetched on
// expand. Tests seed it per call id.
const fullResults = vi.hoisted(() => new Map<string, unknown>());
const getToolCallResult = vi.hoisted(() =>
  vi.fn(async (_sessionId: string, toolCallId: string): Promise<unknown> =>
    fullResults.get(toolCallId) ?? null
  )
);
const fullInputs = vi.hoisted(() => new Map<string, unknown>());
const getToolCallInput = vi.hoisted(() =>
  vi.fn(async (_sessionId: string, toolCallId: string): Promise<unknown> =>
    fullInputs.get(toolCallId) ?? null
  )
);
vi.mock('../../assistant/client', () => ({ getToolCallResult, getToolCallInput }));

import ChatMessageList from './ChatMessageList';
import { clearToolCallResultCache } from '../../assistant/toolCallResult';
import { mainIdentity } from '../Agents/agentIdentity';
import type { AssistantMessage, ToolInvocation } from '../../generated/bindings';

// Whose face the in-flight footer wears. A module constant, the way call
// sites are required to pass it (the footer memoizes on the reference).
const RUNNING_IDENTITY = mainIdentity();

const msg = (
  over: Partial<AssistantMessage> & Pick<AssistantMessage, 'id' | 'role' | 'content'>
): AssistantMessage => ({
  sessionId: 'sess-1',
  createdAt: 0n,
  providerMetadata: null,
  ...over,
});

describe('ChatMessageList', () => {
  beforeEach(() => {
    clearToolCallResultCache();
    fullResults.clear();
    getToolCallResult.mockClear();
    fullInputs.clear();
    getToolCallInput.mockClear();
  });

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
        hasFullResult: true,
        hasFullInput: false,
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
        result: { ok: true, path: 'charts/q3-revenue.vl.json', display: true }, // legacy `display` is ignored
        hasFullResult: true,
        hasFullInput: false,
        error: null,
        startedAt: 0n,
        completedAt: 1n,
        ...over,
      },
    ],
  ];

  it('shows a saved chart as a row that opens the file, with no chart card or fetch', () => {
    const [messages, toolCalls] = chartCall({});
    const onOpenArtifact = vi.fn();
    render(
      <ChatMessageList
        messages={messages}
        toolCalls={toolCalls}
        workspaceId="ws-1"
        onOpenArtifact={onOpenArtifact}
      />
    );
    const row = screen.getByRole('button', { name: 'Open chart Q3 Revenue' });
    expect(row).not.toHaveAttribute('aria-expanded');
    expect(screen.getByText('Chart')).toBeInTheDocument();
    expect(screen.getByText('charts/q3-revenue.vl.json')).toBeInTheDocument();
    expect(screen.queryByTestId('vega-chart')).toBeNull();

    fireEvent.click(row);
    expect(onOpenArtifact).toHaveBeenCalledWith('charts/q3-revenue.vl.json');
    expect(screen.queryByText('Output')).toBeNull();
    expect(getToolCallResult).not.toHaveBeenCalled();
  });

  it('shows a running chart call as a plain tool row', () => {
    const [messages, toolCalls] = chartCall({
      status: 'running',
      result: null,
      hasFullResult: false,
      completedAt: null,
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} onOpenArtifact={vi.fn()} />);
    expect(screen.getByRole('button', { name: /Q3 Revenue/ })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByText('running…')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Open chart/ })).toBeNull();
  });

  it('keeps a failed chart call expandable, so its error stays reachable', () => {
    const [messages, toolCalls] = chartCall({
      status: 'failed',
      result: null,
      hasFullResult: false,
      error: 'The spec is not a valid Vega-Lite chart',
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} onOpenArtifact={vi.fn()} />);
    const row = screen.getByRole('button', { name: /Q3 Revenue/ });
    expect(row).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(row);
    expect(row).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Output')).toBeInTheDocument();
    expect(screen.getByText(/The spec is not a valid Vega-Lite chart/)).toBeInTheDocument();
  });

  it('leaves a chart row expandable when nothing can open artifacts', () => {
    const [messages, toolCalls] = chartCall({});
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByRole('button', { name: /Q3 Revenue/ })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByTestId('vega-chart')).toBeNull();
  });

  it("hands a reply's .vl.json embed to markdown with the workspace to resolve it against", () => {
    const [toolMessages, toolCalls] = chartCall({});
    const reply = msg({
      id: 'm2',
      role: 'assistant',
      content: [{ type: 'text', text: '![Q3 Revenue](/charts/q3-revenue.vl.json)' }],
    });
    render(
      <ChatMessageList
        messages={[...toolMessages, reply]}
        toolCalls={toolCalls}
        workspaceId="ws-1"
        onOpenArtifact={vi.fn()}
      />
    );
    const markdown = screen.getByText('![Q3 Revenue](/charts/q3-revenue.vl.json)');
    expect(markdown).toHaveAttribute('data-workspace', 'ws-1');
  });

  const taskPayload = (over: Record<string, unknown> = {}) => ({
    ok: true,
    task: {
      id: 'task-9',
      workspaceId: 'ws-1',
      assignedToWorkspaceAgentId: 'wa-review',
      assignedAgentDefinitionId: 'def-review',
      title: 'Round 3 review',
      instructions: 'Review the branch end to end.',
      status: 'queued',
      resultSummary: null,
      error: null,
      ...over,
    },
  });

  const taskCalls = (
    calls: Array<{ tool: string; result: ToolInvocation['result'] }>
  ): { messages: AssistantMessage[]; toolCalls: ToolInvocation[] } => ({
    messages: [
      msg({
        id: 'm1',
        role: 'assistant',
        content: calls.map((call, i) => ({
          type: 'tool_use' as const,
          tool_call_id: `tk-${i}`,
          tool_name: call.tool,
          arguments: {},
        })),
      }),
    ],
    toolCalls: calls.map((call, i) => ({
      id: `tk-${i}`,
      runId: 'r-1',
      sessionId: 'sess-1',
      toolName: call.tool,
      params: {},
      status: 'completed',
      result: call.result,
      hasFullResult: true,
      hasFullInput: false,
      error: null,
      startedAt: 0n,
      completedAt: 1n,
    })),
  });

  it('draws a delegated task as a card, and opens it on click', async () => {
    const onOpenTask = vi.fn();
    const { messages, toolCalls } = taskCalls([
      { tool: 'workspace_assignTask', result: taskPayload() },
    ]);
    render(
      <ChatMessageList
        messages={messages}
        toolCalls={toolCalls}
        taskRoster={[]}
        onOpenTask={onOpenTask}
      />
    );
    expect(screen.getByText('Round 3 review')).toBeInTheDocument();
    expect(screen.getByText('Delegated to')).toBeInTheDocument();
    // The card replaces the row: no raw tool name is shown.
    expect(screen.queryByText('workspace_assignTask')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: /Round 3 review/ }));
    expect(onOpenTask).toHaveBeenCalledWith('task-9');
  });

  it('keeps a failed assignment as a plain row, where its error is', () => {
    const { messages, toolCalls } = taskCalls([
      { tool: 'workspace_assignTask', result: null },
    ]);
    toolCalls[0]!.status = 'failed';
    toolCalls[0]!.error = 'workspaceAgentId must be one of the workspace agent ids';
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('workspace_assignTask')).toBeInTheDocument();
    expect(screen.queryByText('Round 3 review')).toBeNull();
  });

  it('collapses the polls of a wait but never the answer', () => {
    const { messages, toolCalls } = taskCalls([
      { tool: 'workspace_assignTask', result: taskPayload() },
      ...Array.from({ length: 6 }, () => ({
        tool: 'workspace_getTaskResult',
        result: taskPayload({ status: 'running' }),
      })),
      {
        tool: 'workspace_getTaskResult',
        result: taskPayload({ status: 'completed', resultSummary: 'Found 3 issues.' }),
      },
    ]);
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} taskRoster={[]} />);

    // Six identical "still running" lines collapse down to the last four.
    expect(screen.getByText('Show 2 earlier calls')).toBeInTheDocument();
    expect(screen.getAllByText('Running')).toHaveLength(4);
    // The hand-off and the answer are cards of their own, outside the run.
    expect(screen.getByText('Delegated to')).toBeInTheDocument();
    expect(screen.getByText('Found 3 issues.')).toBeInTheDocument();
  });

  it('keeps every card in the order its call was made', () => {
    // A card leaves the run it was found in; it must re-enter the transcript
    // where it happened. An answer printed above the polls it followed would
    // read as if the task finished before it was waited on.
    const { messages, toolCalls } = taskCalls([
      { tool: 'workspace_assignTask', result: taskPayload() },
      { tool: 'workspace_getTaskResult', result: taskPayload({ status: 'running' }) },
      {
        tool: 'workspace_getTaskResult',
        result: taskPayload({ status: 'completed', resultSummary: 'Found 3 issues.' }),
      },
      { tool: 'workspace_assignTask', result: taskPayload({ id: 'task-10', title: 'Round 4 review' }) },
    ]);
    const { container } = render(
      <ChatMessageList messages={messages} toolCalls={toolCalls} taskRoster={[]} />
    );
    const text = container.textContent ?? '';
    const at = (needle: string) => {
      const index = text.indexOf(needle);
      expect(index, `${needle} is missing`).toBeGreaterThan(-1);
      return index;
    };
    // hand-off → the slim poll in its run → the answer → the next hand-off.
    expect(at('Round 3 review')).toBeLessThan(at('Running'));
    expect(at('Running')).toBeLessThan(at('Found 3 issues.'));
    expect(at('Found 3 issues.')).toBeLessThan(at('Round 4 review'));
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
      hasFullResult: true,
      hasFullInput: false,
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

  it('collapses a chart row with the plain rows around it', () => {
    const { messages, toolCalls } = toolGroup(6);
    (messages[0]?.content[0] as { tool_name: string }).tool_name = 'create_vega_chart';
    const first = toolCalls[0];
    if (!first) throw new Error('expected a tool call');
    toolCalls[0] = {
      ...first,
      toolName: 'create_vega_chart',
      params: { title: 'Q3 Revenue', spec: {} },
      result: { ok: true, path: 'charts/q3-revenue.vl.json' },
    };
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} onOpenArtifact={vi.fn()} />);
    expect(screen.getByText('Show 2 earlier calls')).toBeInTheDocument();
    expect(screen.queryByText('Q3 Revenue')).toBeNull();
    fireEvent.click(screen.getByText('Show 2 earlier calls'));
    expect(screen.getByRole('button', { name: /Open chart Q3 Revenue/ })).toBeInTheDocument();
  });

  const singleCall = (
    over: Partial<ToolInvocation> & Pick<ToolInvocation, 'toolName' | 'params'>
  ): { messages: AssistantMessage[]; toolCalls: ToolInvocation[] } => ({
    messages: [
      msg({
        id: 'm1',
        role: 'assistant',
        content: [
          { type: 'tool_use', tool_call_id: 'tc-1', tool_name: over.toolName, arguments: {} },
        ],
      }),
    ],
    toolCalls: [
      {
        id: 'tc-1',
        runId: 'r-1',
        sessionId: 'sess-1',
        status: 'completed',
        result: null,
        hasFullResult: true,
        hasFullInput: false,
        error: null,
        startedAt: 0n,
        completedAt: 1n,
        ...over,
      },
    ],
  });

  it('shows a bash row summary (verb, command, exit code) without fetching the output', () => {
    const { messages, toolCalls } = singleCall({
      toolName: 'bash_exec',
      params: { command: 'npm run build' },
      resultSummary: { text: 'exit 0', tone: 'neutral' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('Bash')).toBeInTheDocument();
    expect(screen.getByText('npm run build')).toBeInTheDocument();
    expect(screen.getByText('exit 0')).toBeInTheDocument();
    expect(getToolCallResult).not.toHaveBeenCalled();
  });

  it('fetches the full bash output on expand and reads its MCP envelope', async () => {
    // Claude Code reaches our built-ins through the local MCP server, so the
    // result is stored as the wire envelope rather than the tool's own JSON.
    fullResults.set('tc-1', [
      { type: 'text', text: JSON.stringify({ exitCode: 0, stdout: 'Build complete', stderr: '' }) },
    ]);
    const { messages, toolCalls } = singleCall({
      toolName: 'bash_exec',
      params: { command: 'ls' },
      resultSummary: { text: 'exit 0', tone: 'neutral' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);

    fireEvent.click(screen.getByText('Bash'));
    expect(screen.getByRole('status')).toHaveTextContent('Loading output…');
    // The terminal block shows the command's output, not the raw envelope.
    expect(await screen.findByText('Build complete')).toBeInTheDocument();
    expect(getToolCallResult).toHaveBeenCalledWith('sess-1', 'tc-1');
  });

  it('shows file content from an MCP-enveloped fs_read result', async () => {
    fullResults.set('tc-1', [
      { type: 'text', text: JSON.stringify({ path: '/main.rs', content: 'fn main() {}' }) },
    ]);
    const { messages, toolCalls } = singleCall({
      toolName: 'fs_read',
      params: { path: '/main.rs' },
      resultSummary: { text: '1 line', tone: 'neutral' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    expect(screen.getByText('1 line')).toBeInTheDocument();

    fireEvent.click(screen.getByText('Read'));
    // Fenced with the language guessed from the path, not dumped as JSON.
    await waitFor(() =>
      expect(screen.getAllByTestId('markdown').map((el) => el.textContent ?? '')).toContain(
        '```rust\nfn main() {}\n```'
      )
    );
  });

  it('pretty-prints an MCP-enveloped JSON result instead of one escaped line', async () => {
    fullResults.set('tc-1', [{ type: 'text', text: '{"matches":[{"path":"/a.rs"}]}' }]);
    const { messages, toolCalls } = singleCall({
      toolName: 'fs_glob',
      params: { pattern: '**/*.rs' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    fireEvent.click(screen.getByText('Glob'));
    await waitFor(() =>
      expect(
        screen
          .getAllByTestId('markdown')
          .some((el) => (el.textContent ?? '').includes('```json\n{\n  "matches"'))
      ).toBe(true)
    );
  });

  it('fetches an ancestor-session call from its own session', async () => {
    fullResults.set('tc-1', { exitCode: 0, stdout: 'from the parent', stderr: '' });
    const { messages, toolCalls } = singleCall({
      toolName: 'bash_exec',
      params: { command: 'ls' },
      sessionId: 'sess-parent',
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    fireEvent.click(screen.getByText('Bash'));
    expect(await screen.findByText('from the parent')).toBeInTheDocument();
    expect(getToolCallResult).toHaveBeenCalledWith('sess-parent', 'tc-1');
  });

  it('offers Retry when the output fails to load, and recovers', async () => {
    getToolCallResult.mockRejectedValueOnce(new Error('database is locked'));
    fullResults.set('tc-1', { exitCode: 0, stdout: 'second try', stderr: '' });
    const { messages, toolCalls } = singleCall({
      toolName: 'bash_exec',
      params: { command: 'ls' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    fireEvent.click(screen.getByText('Bash'));
    expect(await screen.findByRole('alert')).toHaveTextContent("Couldn't load output");

    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByText('second try')).toBeInTheDocument();
    expect(getToolCallResult).toHaveBeenCalledTimes(2);
  });

  it('says the output is gone, without Retry, when the call no longer exists', async () => {
    getToolCallResult.mockRejectedValueOnce('Tool call not found in session: tc-1');
    const { messages, toolCalls } = singleCall({
      toolName: 'bash_exec',
      params: { command: 'ls' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    fireEvent.click(screen.getByText('Bash'));
    expect(await screen.findByRole('alert')).toHaveTextContent('Output no longer available');
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
  });

  it('fetches once the running call it is expanded on completes', async () => {
    fullResults.set('tc-1', { exitCode: 0, stdout: 'finished', stderr: '' });
    const running = singleCall({
      toolName: 'bash_exec',
      params: { command: 'make' },
      status: 'running',
      hasFullResult: false,
      completedAt: null,
    });
    const { rerender } = render(
      <ChatMessageList messages={running.messages} toolCalls={running.toolCalls} />
    );
    fireEvent.click(screen.getByText('Bash'));
    expect(screen.getByText('Executing…')).toBeInTheDocument();
    expect(getToolCallResult).not.toHaveBeenCalled();

    const done = singleCall({
      toolName: 'bash_exec',
      params: { command: 'make' },
      resultSummary: { text: 'exit 0', tone: 'neutral' },
    });
    rerender(<ChatMessageList messages={running.messages} toolCalls={done.toolCalls} />);
    expect(await screen.findByText('finished')).toBeInTheDocument();
    expect(getToolCallResult).toHaveBeenCalledTimes(1);
  });

  it('reuses a fetched result when the row is expanded again', async () => {
    fullResults.set('tc-1', { exitCode: 0, stdout: 'cached', stderr: '' });
    const { messages, toolCalls } = singleCall({
      toolName: 'bash_exec',
      params: { command: 'ls' },
    });
    render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
    fireEvent.click(screen.getByText('Bash'));
    expect(await screen.findByText('cached')).toBeInTheDocument();
    fireEvent.click(screen.getByText('Bash'));
    fireEvent.click(screen.getByText('Bash'));
    expect(screen.getByText('cached')).toBeInTheDocument();
    expect(getToolCallResult).toHaveBeenCalledTimes(1);
  });

  describe('projected input', () => {
    const LARGE = 'large payload line\n'.repeat(2_000);
    const writeCall = (over: Partial<ToolInvocation> = {}) =>
      singleCall({
        toolName: 'fs_write',
        params: { path: 'report.md' },
        hasFullInput: true,
        hasFullResult: true,
        resultSummary: { text: 'written', tone: 'neutral' },
        ...over,
      });
    const markdownText = () => screen.queryAllByTestId('markdown').map((el) => el.textContent ?? '');

    it('keeps a large input out of the page until the Input tab asks for it', async () => {
      fullInputs.set('tc-1', { path: 'report.md', content: LARGE });
      fullResults.set('tc-1', { path: 'report.md', bytesWritten: LARGE.length });
      const { messages, toolCalls } = writeCall();
      render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);

      expect(screen.getByText('report.md')).toBeInTheDocument();
      expect(document.body.textContent).not.toContain('large payload line');
      expect(getToolCallInput).not.toHaveBeenCalled();

      fireEvent.click(screen.getByText('Write'));
      fireEvent.click(screen.getByRole('button', { name: 'Input' }));
      expect(screen.getByRole('status')).toHaveTextContent('Loading input…');
      await waitFor(() =>
        expect(markdownText().some((text) => text.includes('"content": "large payload line'))).toBe(
          true
        )
      );
      expect(getToolCallInput).toHaveBeenCalledTimes(1);
      expect(getToolCallInput).toHaveBeenCalledWith('sess-1', 'tc-1');
    });

    it('draws the written content in the Output tab from the full input', async () => {
      fullInputs.set('tc-1', { path: 'main.rs', content: 'fn main() {}' });
      fullResults.set('tc-1', { path: 'main.rs', bytesWritten: 12 });
      const { messages, toolCalls } = writeCall({ params: { path: 'main.rs' } });
      render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
      fireEvent.click(screen.getByText('Write'));
      await waitFor(() => expect(markdownText()).toContain('```rust\nfn main() {}\n```'));

      // The Input tab reuses the input the Output tab already fetched.
      fireEvent.click(screen.getByRole('button', { name: 'Input' }));
      expect(markdownText().some((text) => text.includes('"content": "fn main() {}"'))).toBe(true);
      expect(getToolCallInput).toHaveBeenCalledTimes(1);
    });

    it('shows a complete input as sent, without fetching', () => {
      const { messages, toolCalls } = singleCall({
        toolName: 'fs_glob',
        params: { pattern: '**/*.rs' },
        hasFullInput: false,
      });
      render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
      fireEvent.click(screen.getByText('Glob'));
      fireEvent.click(screen.getByRole('button', { name: 'Input' }));
      expect(markdownText()).toContain('```json\n{\n  "pattern": "**/*.rs"\n}\n```');
      expect(getToolCallInput).not.toHaveBeenCalled();
    });

    it('offers an Input tab even when the projection kept no field', async () => {
      fullInputs.set('tc-1', { items: [1, 2, 3] });
      const { messages, toolCalls } = singleCall({
        toolName: 'custom_tool',
        params: {},
        hasFullInput: true,
      });
      render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
      fireEvent.click(screen.getByText('custom_tool'));
      fireEvent.click(screen.getByRole('button', { name: 'Input' }));
      await waitFor(() =>
        expect(markdownText().some((text) => text.includes('"items": ['))).toBe(true)
      );
    });

    it('fetches an ancestor-session input from its own session and retries a failure', async () => {
      getToolCallInput.mockRejectedValueOnce(new Error('database is locked'));
      fullInputs.set('tc-1', { path: 'report.md', content: 'from the parent' });
      const { messages, toolCalls } = writeCall({ sessionId: 'sess-parent' });
      render(<ChatMessageList messages={messages} toolCalls={toolCalls} />);
      fireEvent.click(screen.getByText('Write'));
      fireEvent.click(screen.getByRole('button', { name: 'Input' }));
      expect(await screen.findByRole('alert')).toHaveTextContent("Couldn't load input");

      fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
      await waitFor(() =>
        expect(markdownText().some((text) => text.includes('from the parent'))).toBe(true)
      );
      expect(getToolCallInput).toHaveBeenCalledTimes(2);
      expect(getToolCallInput).toHaveBeenLastCalledWith('sess-parent', 'tc-1');
    });

    it('waits for completion before fetching the input an Output view draws from', async () => {
      const command = `echo ${'x'.repeat(400)}`;
      fullInputs.set('tc-1', { command });
      fullResults.set('tc-1', { exitCode: 0, stdout: 'finished', stderr: '' });
      const projected = { command: `${command.slice(0, 300)}…` };
      const running = singleCall({
        toolName: 'bash_exec',
        params: projected,
        hasFullInput: true,
        status: 'running',
        hasFullResult: false,
        completedAt: null,
      });
      const { rerender } = render(
        <ChatMessageList messages={running.messages} toolCalls={running.toolCalls} />
      );
      fireEvent.click(screen.getByText('Bash'));
      expect(screen.getByText('Executing…')).toBeInTheDocument();
      expect(getToolCallInput).not.toHaveBeenCalled();

      const done = singleCall({ toolName: 'bash_exec', params: projected, hasFullInput: true });
      rerender(<ChatMessageList messages={running.messages} toolCalls={done.toolCalls} />);
      expect(await screen.findByText('finished')).toBeInTheDocument();
      expect(screen.getByText(`$ ${command}`)).toBeInTheDocument();
      expect(getToolCallInput).toHaveBeenCalledTimes(1);
      expect(getToolCallResult).toHaveBeenCalledTimes(1);
    });
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
    const { rerender } = render(
      <ChatMessageList messages={messages} isStreaming runningIdentity={RUNNING_IDENTITY} />
    );
    expect(screen.getByTestId('virtual-list').children).toHaveLength(2); // user item + footer
    expect(screen.queryByText('Clai')).toBeNull();

    // First streamed delta for the placeholder makes it visible.
    rerender(
      <ChatMessageList
        messages={messages}
        isStreaming
        runningIdentity={RUNNING_IDENTITY}
        streamingText={{ m2: 'on it' }}
      />
    );
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

  it("shows the running agent's face and an elapsed timer in the running footer", () => {
    const messages: AssistantMessage[] = [
      msg({ id: 'm1', role: 'assistant', content: [{ type: 'text', text: 'working…' }] }),
    ];
    render(
      <ChatMessageList
        messages={messages}
        isStreaming
        runStartedAt={Date.now() - 8000}
        runningIdentity={RUNNING_IDENTITY}
      />
    );
    // The face of whoever is running, wearing its running expression…
    const face = screen.getByRole('img', { name: 'Working' });
    expect(face.dataset.activity).toBe('running');
    // …but without the status ring, because that ring spins and this footer
    // is on screen for the whole of every run (see AssistantChat.module.css).
    expect(face.className).not.toMatch(/ring/);
    expect(face.querySelector('svg')).not.toBeNull();
    // An m:ss timer (~0:08), and no token count.
    expect(screen.getByText(/^0:0\d$/)).toBeInTheDocument();
    expect(screen.queryByText(/tokens/)).toBeNull();
  });

  it('renders no running footer while streaming when no identity is given', () => {
    // How task transcripts render: the card and the panel header already say
    // who is running, so the log carries no third marker. The store also only
    // knows `isStreaming` for runs it saw start live, so a footer here was
    // present or absent depending on when the panel was opened.
    const messages: AssistantMessage[] = [
      msg({ id: 'm1', role: 'assistant', content: [{ type: 'text', text: 'working…' }] }),
    ];
    render(<ChatMessageList messages={messages} isStreaming runStartedAt={Date.now() - 8000} />);
    // The transcript still renders — only the footer is gone.
    expect(screen.getByText('working…')).toBeInTheDocument();
    expect(screen.queryByRole('img', { name: 'Working' })).toBeNull();
    expect(screen.queryByText(/^0:0\d$/)).toBeNull();
    expect(screen.getByTestId('virtual-list').children).toHaveLength(1);
  });
});
