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

const PUSH: PermissionRequest = {
  requestId: 'req-1',
  workspaceId: 'ws-1',
  agentId: 'agent-1',
  agentName: null,
  command: 'cargo test && git push origin main',
  rows: [{ text: 'git push origin main', rule: 'git push' }],
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
};

const TWO_RULES: PermissionRequest = {
  ...PUSH,
  requestId: 'req-2',
  command: 'rm -f build.log && git push',
  rows: [
    { text: 'rm -f build.log', rule: 'rm' },
    { text: 'git push', rule: 'git push' },
  ],
};

const speaker = (prompt: ReactNode) => <div>{prompt}</div>;

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

const submitted = () =>
  mockInvoke.mock.calls.filter(([cmd]) => cmd === 'submit_permission_decision').map(([, args]) => args);

describe('ApprovalCard — the three actions', () => {
  it('lists each matched part with the rule it matched', () => {
    render(<ApprovalCard req={TWO_RULES} />);
    const matches = within(screen.getByRole('list', { name: 'Ask-first matches' })).getAllByRole('listitem');
    expect(matches.map((item) => item.textContent)).toEqual(['rm -f build.logrm', 'git pushgit push']);
    expect(screen.getByText('rm')).toHaveAttribute('title', 'Matches the ask-first rule `rm`');
  });

  it('sends Allow once with no reason', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'allowOnce' } }]);
  });

  it('sends Always allow with no reason', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Always allow (this workspace)' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'alwaysAllow' } }]);
  });

  it('sends no reason with an allow, even after one was typed for a denial', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    await userEvent.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'not yet');
    await userEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'allowOnce' } }]);
  });

  it('does not send the decision twice', async () => {
    let resolve: () => void = () => {};
    mockInvoke.mockImplementation(() => new Promise<void>((done) => { resolve = done; }));
    render(<ApprovalCard req={PUSH} />);
    const once = screen.getByRole('button', { name: 'Allow once' });
    fireEvent.click(once);
    fireEvent.click(screen.getByRole('button', { name: 'Always allow (this workspace)' }));
    expect(submitted()).toHaveLength(1);
    await act(async () => resolve());
  });
});

describe('ApprovalCard — where Always allow applies', () => {
  it('says "this workspace only" for the workspace\'s own Main', () => {
    render(<ApprovalCard req={TWO_RULES} />);
    expect(
      screen.getByText(
        'Always allow: Removes `rm`, `git push` from this agent\'s ask-first list in this workspace only, and runs the command.'
      )
    ).toBeInTheDocument();
    expect(screen.queryByText(/every workspace/)).toBeNull();
  });

  it('names the other workspaces a shared agent works in', () => {
    render(<ApprovalCard req={{ ...PUSH, persistsToSharedAgent: true, alsoAffectsWorkspaces: ['Docs', 'Infra'] }} />);
    expect(screen.getByRole('button', { name: 'Always allow (every workspace)' })).toBeInTheDocument();
    expect(screen.getByText(/shared agent's ask-first list, so it also stops asking in: Docs, Infra/)).toBeInTheDocument();
  });

  it('still says "every workspace" for a shared agent nobody else has added yet', () => {
    render(<ApprovalCard req={{ ...PUSH, persistsToSharedAgent: true }} />);
    expect(screen.getByRole('button', { name: 'Always allow (every workspace)' })).toBeInTheDocument();
    expect(screen.getByText(/wherever this agent is added/)).toBeInTheDocument();
  });
});

describe('ApprovalCard — Deny and its reason', () => {
  it('opens the reason field and waits for the confirm, then sends the reason trimmed', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PUSH} />);
    expect(screen.queryByRole('textbox', { name: /reason for denying/i })).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([]);
    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    await user.type(reason, '  Push after review  ');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));

    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'deny', reason: 'Push after review' } }]);
  });

  it('omits the reason when the field holds only whitespace', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    await userEvent.type(screen.getByRole('textbox', { name: /reason for denying/i }), '   ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'deny' } }]);
  });

  it('says the reason goes to the agent, and how much of the limit it uses', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    expect(reason).toHaveAccessibleDescription(/Sent to the agent with this response\./);
    await userEvent.type(reason, 'abc');
    expect(screen.getByText('3/500 characters')).toBeInTheDocument();
  });

  it('accepts 500 Unicode characters and drops the next', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    fireEvent.change(reason, { target: { value: 'é'.repeat(501) } });
    expect(Array.from((reason as HTMLTextAreaElement).value)).toHaveLength(500);
    expect(screen.getByText('500/500 characters')).toBeInTheDocument();
  });

  it('keeps the reason editable after a failed submission', async () => {
    mockInvoke.mockRejectedValueOnce('Approval channel closed');
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    await userEvent.type(reason, 'no');
    await userEvent.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(await screen.findByText('Approval channel closed')).toBeInTheDocument();
    expect(reason).toHaveValue('no');
    expect(reason).toBeEnabled();
    await userEvent.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(submitted()).toHaveLength(2);
  });
});

describe('ApprovalCard — spoken (the Fleet)', () => {
  it('drops the tag, the collapse and the reach line, and speaks short chips', () => {
    render(<ApprovalCard req={PUSH} speaker={speaker} />);
    expect(screen.queryByText('Permission requested')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /collapse/i })).not.toBeInTheDocument();
    expect(screen.getByLabelText('Command')).toHaveTextContent('cargo test && git push origin main');
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Once', 'Always', 'Deny']);
    expect(screen.getByRole('button', { name: 'Always' }).title).toMatch(/^Always allow \(this workspace\)\. Removes `git push`/);
  });

  it('sends the same decision as the full card', async () => {
    render(<ApprovalCard req={{ ...PUSH, persistsToSharedAgent: true }} speaker={speaker} />);
    const always = screen.getByRole('button', { name: 'Always' });
    expect(always.title).toMatch(/^Always allow \(every workspace\)/);
    await userEvent.click(always);
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'alwaysAllow' } }]);
  });
});

describe('InlineApprovalCard', () => {
  it('renders nothing until a request for this workspace arrives', () => {
    const { container } = render(<InlineApprovalCard workspaceId="ws-1" />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders a card when a matching request fires, and ignores other workspaces', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest({ ...PUSH, requestId: 'req-other', workspaceId: 'ws-2', command: 'elsewhere' });
    fireRequest(PUSH);

    expect(await screen.findByText('cargo test && git push origin main')).toBeInTheDocument();
    expect(screen.getByText('Permission requested')).toBeInTheDocument();
    expect(screen.queryByText('elsewhere')).toBeNull();
  });

  it('drops a previous workspace\'s card when switching workspaces', async () => {
    const { rerender } = render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    expect(await screen.findByText('cargo test && git push origin main')).toBeInTheDocument();

    rerender(<InlineApprovalCard workspaceId="ws-2" />);

    await waitFor(() => expect(screen.queryByText('cargo test && git push origin main')).toBeNull());
  });

  it('drops the card when permissions://resolved fires for it (abandoned or cancelled run)', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    expect(await screen.findByText('cargo test && git push origin main')).toBeInTheDocument();

    fireResolved('req-1');

    await waitFor(() => expect(screen.queryByText('cargo test && git push origin main')).toBeNull());
  });

  it('drops the card once its decision is accepted', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    await userEvent.click(await screen.findByRole('button', { name: 'Allow once' }));
    await waitFor(() => expect(screen.queryByRole('article')).toBeNull());
  });

  it('collapses the card body and expands it again', async () => {
    const user = userEvent.setup();
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    await screen.findByText('cargo test && git push origin main');

    await user.click(screen.getByRole('button', { name: /collapse request/i }));
    expect(screen.queryByRole('button', { name: 'Allow once' })).toBeNull();
    expect(screen.getByText('cargo test && git push origin main')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /expand request/i }));
    expect(screen.getByRole('button', { name: 'Allow once' })).toBeInTheDocument();
  });

  it('shows a failed submit on that card only, and leaves the other card answerable', async () => {
    let reject: (reason: unknown) => void = () => {};
    mockInvoke.mockImplementation((cmd: string) =>
      cmd === 'submit_permission_decision' ? new Promise((_, no) => { reject = no; }) : Promise.resolve([])
    );
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    fireRequest(TWO_RULES);
    await screen.findByText('rm -f build.log && git push');
    const [first, second] = screen.getAllByRole('article', { name: 'Permission requested' }).map((card) => within(card));

    await userEvent.click(first!.getByRole('button', { name: 'Allow once' }));
    expect(first!.getByRole('button', { name: 'Allow once' })).toBeDisabled();
    expect(second!.getByRole('button', { name: 'Allow once' })).toBeEnabled();

    await act(async () => reject('Approval channel closed'));
    expect(await first!.findByText('Approval channel closed')).toBeInTheDocument();
    expect(second!.queryByText('Approval channel closed')).not.toBeInTheDocument();
  });
});
