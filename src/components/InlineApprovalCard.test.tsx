import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// Tauri invoke (submit_permission_decision) + the event-bus listen are
// both mocked. `listen` captures the handler so we can fire a synthetic
// permissions://request event; the seed list returns empty.
const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

// The component registers one listener per event name (request + resolved),
// so capture handlers keyed by event name rather than a single slot.
let listenHandlers: Record<string, (event: { payload: unknown }) => void> = {};
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listenHandlers[name] = handler;
    return Promise.resolve(() => {});
  }),
}));

import InlineApprovalCard from './InlineApprovalCard';

const SINGLE_SEGMENT_REQUEST = {
  requestId: 'req-1',
  workspaceId: 'ws-1',
  agentId: 'agent-1',
  agentName: 'Builder',
  command: 'rg --files',
  segments: [{ text: 'rg', kind: 'simple', suggestedPrefix: 'rg' }],
};

beforeEach(() => {
  mockInvoke.mockReset();
  listenHandlers = {};
  // list_pending_permission_requests seed returns empty so only the
  // fired event populates the card.
  mockInvoke.mockResolvedValue([]);
});

const fireRequest = (req: unknown) => {
  const handler = listenHandlers['permissions://request'];
  if (!handler) throw new Error('request listener not registered');
  handler({ payload: req });
};

const fireResolved = (requestId: string) => {
  const handler = listenHandlers['permissions://resolved'];
  if (!handler) throw new Error('resolved listener not registered');
  handler({ payload: { requestId } });
};

describe('InlineApprovalCard', () => {
  it('renders nothing until a request for this workspace arrives', () => {
    const { container } = render(<InlineApprovalCard workspaceId="ws-1" />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders a card when a matching permission request fires', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(SINGLE_SEGMENT_REQUEST);

    expect(await screen.findByText('rg --files')).toBeInTheDocument();
    expect(screen.getByText('Permission requested')).toBeInTheDocument();
  });

  it('ignores requests for a different workspace', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest({ ...SINGLE_SEGMENT_REQUEST, requestId: 'req-other', workspaceId: 'ws-2' });

    expect(screen.queryByText('rg --files')).toBeNull();
  });

  it('submits an allow-once decision via submit_permission_decision', async () => {
    const user = userEvent.setup();
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(SINGLE_SEGMENT_REQUEST);
    await screen.findByText('rg --files');

    // single-segment card auto-submits once every segment is decided.
    mockInvoke.mockResolvedValueOnce(undefined);
    await user.click(screen.getByRole('button', { name: /allow once/i }));

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
        requestId: 'req-1',
        decisions: [{ kind: 'allowOnce' }],
      });
    });
  });

  it('drops a previous workspace\'s card when switching workspaces', async () => {
    // Regression: the Workspace page reuses this component instance across
    // workspace→workspace navigation (no remount), so a pending card from
    // workspace A must not linger in workspace B's view. Changing the
    // workspaceId prop has to clear the stale request.
    const { rerender } = render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(SINGLE_SEGMENT_REQUEST);
    expect(await screen.findByText('rg --files')).toBeInTheDocument();

    rerender(<InlineApprovalCard workspaceId="ws-2" />);

    await waitFor(() => expect(screen.queryByText('rg --files')).toBeNull());
  });

  it('drops the card when permissions://resolved fires for it (abandoned tool call)', async () => {
    // Regression: when the agent's bash_exec call is abandoned mid-wait
    // (the CLI drops the MCP transport, "response for tool bash_exec was
    // lost"), the backend clears the request and emits resolved; the now-
    // useless card must disappear instead of lingering.
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(SINGLE_SEGMENT_REQUEST);
    expect(await screen.findByText('rg --files')).toBeInTheDocument();

    fireResolved('req-1');

    await waitFor(() => expect(screen.queryByText('rg --files')).toBeNull());
  });

  it('collapses the card body and expands it again', async () => {
    const user = userEvent.setup();
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(SINGLE_SEGMENT_REQUEST);
    await screen.findByText('rg --files');

    // Expanded by default: the decision buttons are present.
    expect(screen.getByRole('button', { name: /allow once/i })).toBeInTheDocument();

    // Collapse hides the body (buttons) but keeps the command summary.
    await user.click(screen.getByRole('button', { name: /collapse request/i }));
    expect(screen.queryByRole('button', { name: /allow once/i })).toBeNull();
    expect(screen.getByText('rg --files')).toBeInTheDocument();

    // Expand restores the decision buttons.
    await user.click(screen.getByRole('button', { name: /expand request/i }));
    expect(screen.getByRole('button', { name: /allow once/i })).toBeInTheDocument();
  });
});
