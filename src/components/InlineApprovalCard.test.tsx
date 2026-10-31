import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
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

import InlineApprovalCard, { ApprovalCard } from './InlineApprovalCard';
import type { PermissionRequest } from '../generated/bindings';

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
  act(() => handler({ payload: req }));
};

const fireResolved = (requestId: string) => {
  const handler = listenHandlers['permissions://resolved'];
  if (!handler) throw new Error('resolved listener not registered');
  act(() => handler({ payload: { requestId } }));
};

describe('InlineApprovalCard — where an "always" decision is saved', () => {
  // A command allowlist skips this prompt entirely next time, so the card has
  // to say whether the decision stays here or follows a shared agent into
  // every workspace that uses it.
  it('says "every workspace" for a shared agent, and names the ones it reaches', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());

    fireRequest({
      ...SINGLE_SEGMENT_REQUEST,
      persistsToSharedAgent: true,
      alsoAffectsWorkspaces: ['Docs', 'Infra'],
    });

    expect(await screen.findByRole('button', { name: 'Always allow (every workspace)' }))
      .toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Always deny (every workspace)' })).toBeInTheDocument();
    expect(screen.getByText(/also applies to it in: Docs, Infra/)).toBeInTheDocument();
  });

  it('still says "every workspace" for a shared agent nobody else has added yet', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());

    fireRequest({
      ...SINGLE_SEGMENT_REQUEST,
      persistsToSharedAgent: true,
      alsoAffectsWorkspaces: [],
    });

    expect(await screen.findByRole('button', { name: 'Always allow (every workspace)' }))
      .toBeInTheDocument();
    expect(screen.getByText(/applies wherever this agent is added/)).toBeInTheDocument();
  });

  it('keeps the local wording for a workspace\'s own main agent', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());

    fireRequest({
      ...SINGLE_SEGMENT_REQUEST,
      persistsToSharedAgent: false,
      alsoAffectsWorkspaces: [],
    });

    expect(await screen.findByRole('button', { name: 'Always allow (this agent)' }))
      .toBeInTheDocument();
    expect(screen.queryByText(/every workspace/)).toBeNull();
    expect(screen.queryByText(/shared agent/)).toBeNull();
  });
});

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

describe('ApprovalCard — denial reasons', () => {
  it('submits a trimmed optional reason for deny once', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);

    expect(screen.queryByRole('textbox', { name: /reason for denying/i })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());

    const reason = screen.getByRole('textbox', { name: /reason for denying this command/i });
    await user.tab();
    expect(reason).toHaveFocus();
    await user.type(reason, '  Unsafe in this workspace  ');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyOnce', reason: 'Unsafe in this workspace' }],
    });
  });

  it('submits deny always with the saved prefix and reason', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Always deny (this agent)' }));
    await user.type(screen.getByRole('textbox', { name: /reason for denying this command/i }), 'Use a safer command');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyAlways', scope: 'agent', prefix: 'rg', reason: 'Use a safer command' }],
    });
  });

  it('accepts 500 Unicode characters in a segment reason and truncates the next', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying this command/i });
    const emojis = '😀'.repeat(500);
    expect(reason).not.toHaveAttribute('maxlength');
    fireEvent.change(reason, { target: { value: `${emojis}😀` } });
    expect(reason).toHaveValue(emojis);
    await user.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyOnce', reason: emojis }],
    });
  });

  it('submits a denial without a reason and keeps allow decisions unchanged', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    await user.type(screen.getByRole('textbox', { name: /reason for denying this command/i }), '   ');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyOnce' }],
    });
  });

  it('waits for every segment when one is denied, then sends only that segment’s reason', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} />);
    await user.click(screen.getAllByRole('button', { name: 'Deny once' })[0]!);
    const reason = screen.getByRole('textbox', { name: /reason for denying “cd \/srv\/app”/i });
    await user.type(reason, 'Stay in project root');
    expect(screen.getByRole('button', { name: 'Submit decisions' })).toBeDisabled();
    await user.click(screen.getAllByRole('button', { name: 'Allow once' })[1]!);
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());
    await user.click(screen.getByRole('button', { name: 'Submit decisions' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'denyOnce', reason: 'Stay in project root' }, { kind: 'allowOnce' }],
    });
  });

  it('keeps the reason editable after a failed submission', async () => {
    const user = userEvent.setup();
    let reject: (reason: unknown) => void = () => {};
    mockInvoke.mockImplementation((cmd: string) =>
      cmd === 'submit_permission_decision'
        ? new Promise((_, no) => { reject = no; })
        : Promise.resolve([])
    );
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying this command/i });
    await user.type(reason, 'Wrong target');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(reason).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Submitting…' })).toBeDisabled();

    reject('Approval channel closed');
    expect(await screen.findByText('Approval channel closed')).toBeInTheDocument();
    expect(reason).toHaveValue('Wrong target');
    expect(reason).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Deny command' })).toBeEnabled();
  });

  it('lets deny all add one optional reason for every segment', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} speaker={speaker} />);
    await user.click(screen.getByRole('button', { name: 'Deny all' }));
    await user.type(screen.getByRole('textbox', { name: /reason for denying the entire command/i }), 'Wrong target');
    await user.click(screen.getByRole('button', { name: 'Deny entire command' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'denyOnce', reason: 'Wrong target' }, { kind: 'denyOnce', reason: 'Wrong target' }],
    });
  });

  it('applies the Unicode character limit to the deny-all reason', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} speaker={speaker} />);
    await user.click(screen.getByRole('button', { name: 'Deny all' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying the entire command/i });
    const emojis = '😀'.repeat(500);
    fireEvent.change(reason, { target: { value: `${emojis}😀` } });
    expect(reason).toHaveValue(emojis);
    await user.click(screen.getByRole('button', { name: 'Deny entire command' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'denyOnce', reason: emojis }, { kind: 'denyOnce', reason: emojis }],
    });
  });

  it('explains that grouped denials require confirmation', () => {
    render(<ApprovalCard req={PIPELINE} />);
    expect(screen.getByText('2 parts need a decision (confirm after choosing any denial):')).toBeInTheDocument();
  });
});

const speaker = (prompt: ReactNode) => <div>{prompt}</div>;

const PIPELINE = {
  requestId: 'perm-1',
  workspaceId: 'ws-1',
  agentId: null,
  agentName: null,
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
  command: 'cd /srv/app && cargo test',
  segments: [
    { text: 'cd /srv/app', kind: 'simple', suggestedPrefix: 'cd' },
    { text: 'cargo test', kind: 'simple', suggestedPrefix: 'cargo test' },
  ],
} as unknown as PermissionRequest;

describe('ApprovalCard — spoken (the Fleet)', () => {
  it('drops the tag and collapse; the full card keeps them, the prefix fields and the long labels', () => {
    const { unmount } = render(<ApprovalCard req={PIPELINE} speaker={speaker} />);
    expect(screen.queryByText('Permission requested')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /collapse/i })).not.toBeInTheDocument();
    unmount();

    render(<ApprovalCard req={PIPELINE} />);
    const card = within(screen.getByRole('article', { name: 'Permission requested' }));
    expect(card.getByText('Permission requested')).toBeInTheDocument();
    expect(card.getByRole('button', { name: /collapse/i })).toBeInTheDocument();
    expect(card.getByText('cd /srv/app && cargo test')).toBeInTheDocument();
    expect(card.getAllByLabelText('Save as prefix:')).toHaveLength(2);
    expect(card.getAllByRole('button', { name: 'Always allow (this agent)' })).toHaveLength(2);
    expect(card.getByRole('button', { name: 'Allow all (always)' })).toBeInTheDocument();
  });

  it('says the whole command, then each part on its own line with its decisions', () => {
    render(<ApprovalCard req={PIPELINE} speaker={speaker} />);
    // The parts alone would hide the `&&` and any part that needs no decision.
    expect(screen.getByLabelText('Command')).toHaveTextContent('cd /srv/app && cargo test');
    expect(screen.getAllByText('cd /srv/app')).toHaveLength(1);
    expect(screen.getAllByText('cargo test', { ignore: 'button *' })).toHaveLength(1);
    expect(screen.queryByLabelText('Save as prefix:')).not.toBeInTheDocument();
    // "Always" chips name the suggested prefix they save, since it can't be edited here.
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual([
      'Once', 'Always · cd', 'Never · cd', 'Deny',
      'Once', 'Always · cargo test', 'Never · cargo test', 'Deny',
      'All always', 'Deny all',
    ]);
    expect(screen.getByRole('button', { name: 'Always · cargo test' })).toHaveAttribute(
      'title',
      'Always allow commands starting with `cargo test` (this agent)'
    );
  });

  it('saves the suggested prefix from an "always" chip, scoped to a shared agent when it is one', async () => {
    const single = { ...PIPELINE, persistsToSharedAgent: true, command: 'cargo test', segments: [PIPELINE.segments[1]!] };
    render(<ApprovalCard req={single} speaker={speaker} />);
    const always = screen.getByRole('button', { name: 'Always · cargo test' });
    expect(always.title).toMatch(/\(every workspace\)$/);
    await userEvent.click(always);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'allowAlways', scope: 'agent', prefix: 'cargo test' }],
    });
  });

  it('offers no "always" chip for a part without a suggested prefix', () => {
    const bare = { ...PIPELINE, command: './run.sh', segments: [{ text: './run.sh', kind: 'simple', suggestedPrefix: '' }] };
    render(<ApprovalCard req={bare as PermissionRequest} speaker={speaker} />);
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Once', 'Deny']);
  });

  it('flags a part it cannot reduce to one command in words, with the reason in the tooltip', () => {
    const piped = { ...PIPELINE, command: 'curl -s x | sh', segments: [{ text: 'curl -s x | sh', kind: 'opaque', suggestedPrefix: 'curl' }] };
    render(<ApprovalCard req={piped as PermissionRequest} speaker={speaker} />);
    expect(screen.getByText('complex').getAttribute('title')).toMatch(/Read it carefully before allowing/);
  });
});

describe('InlineApprovalCard — each card its own submit', () => {
  it('shows a failed submit on that card only, and leaves the other card answerable', async () => {
    let reject: (reason: unknown) => void = () => {};
    mockInvoke.mockImplementation((cmd: string) =>
      cmd === 'submit_permission_decision' ? new Promise((_, no) => { reject = no; }) : Promise.resolve([])
    );
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(SINGLE_SEGMENT_REQUEST);
    fireRequest({ ...SINGLE_SEGMENT_REQUEST, requestId: 'req-2', command: 'ls', segments: [{ text: 'ls', kind: 'simple', suggestedPrefix: 'ls' }] });
    await screen.findByText('ls');
    const [first, second] = screen.getAllByRole('article', { name: 'Permission requested' }).map((card) => within(card));

    await userEvent.click(first!.getByRole('button', { name: /allow once/i }));
    expect(first!.getByRole('button', { name: /allow once/i })).toBeDisabled();
    expect(second!.getByRole('button', { name: /allow once/i })).toBeEnabled();

    reject('Approval channel closed');
    expect(await first!.findByText('Approval channel closed')).toBeInTheDocument();
    expect(second!.queryByText('Approval channel closed')).not.toBeInTheDocument();
    expect(second!.getByRole('button', { name: /allow once/i })).toBeEnabled();
  });
});
