import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
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

// One part on two rules: the backend sends a row for each.
const ONE_PART_TWO_RULES: PermissionRequest = {
  ...PUSH,
  requestId: 'req-3',
  command: 'cargo build && sudo rm -rf target',
  rows: [
    { text: 'sudo rm -rf target', rule: 'sudo' },
    { text: 'sudo rm -rf target', rule: 'rm' },
  ],
};

// The same part on the same rule twice: the backend can send identical rows.
const REPEATED_ROW: PermissionRequest = {
  ...PUSH,
  requestId: 'req-4',
  command: 'rm -rf y; rm -rf y',
  rows: [
    { text: 'rm -rf y', rule: 'rm' },
    { text: 'rm -rf y', rule: 'rm' },
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

/** The backend rejects Always allow when it cannot save the rule; every other call succeeds. */
const failAlwaysAllow = (message: string) =>
  mockInvoke.mockImplementation((cmd: string, args?: { decision?: { kind: string } }) =>
    cmd === 'submit_permission_decision' && args?.decision?.kind === 'alwaysAllow'
      ? Promise.reject(message)
      : Promise.resolve([])
  );

describe('ApprovalCard — the three actions', () => {
  it('lists each matched part with the rule it matched', () => {
    render(<ApprovalCard req={TWO_RULES} />);
    const list = within(screen.getByRole('list', { name: 'Ask-first matches' }));
    const matches = list.getAllByRole('listitem');
    expect(matches.map((item) => item.textContent)).toEqual(['rm -f build.logrm', 'git pushgit push']);
    expect(list.getByText('rm')).toHaveAttribute('title', 'Matches the ask-first rule “rm”');
  });

  it('lists a part that matches two rules once, with a chip for each rule', () => {
    render(<ApprovalCard req={ONE_PART_TWO_RULES} />);
    expect(screen.getByText('Part of this command is on the ask-first list:')).toBeInTheDocument();
    const matches = within(screen.getByRole('list', { name: 'Ask-first matches' })).getAllByRole('listitem');
    expect(matches).toHaveLength(1);
    const part = within(matches[0]!);
    expect(part.getByText('sudo rm -rf target').tagName).toBe('CODE');
    expect(part.getAllByTitle(/^Matches the ask-first rule/).map((chip) => chip.textContent)).toEqual(['sudo', 'rm']);
    expect(screen.getByText(/^Always allow removes/)).toHaveTextContent(/^Always allow removes sudo and rm from /);
  });

  it('lists identical rows as one part with one chip', () => {
    render(<ApprovalCard req={REPEATED_ROW} />);
    const matches = within(screen.getByRole('list', { name: 'Ask-first matches' })).getAllByRole('listitem');
    expect(matches).toHaveLength(1);
    expect(within(matches[0]!).getAllByTitle(/^Matches the ask-first rule/).map((chip) => chip.textContent)).toEqual(['rm']);
  });

  it('says whether the whole command or only part of it is on the list', () => {
    const { unmount } = render(<ApprovalCard req={{ ...PUSH, command: 'git push origin main' }} />);
    expect(screen.getByText('This command is on the ask-first list:')).toBeInTheDocument();
    unmount();

    const { unmount: unmountPart } = render(<ApprovalCard req={PUSH} />);
    expect(screen.getByText('Part of this command is on the ask-first list:')).toBeInTheDocument();
    unmountPart();

    render(<ApprovalCard req={TWO_RULES} />);
    expect(screen.getByText('These parts of the command are on the ask-first list:')).toBeInTheDocument();
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
    await userEvent.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'not yet');
    await userEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'allowOnce' } }]);
  });

});

describe('ApprovalCard — where Always allow applies', () => {
  const scopeLine = () => screen.getByText(/^Always allow (removes|stops asking about)/);

  it('says "this workspace only" for the workspace\'s own Main, naming each rule as code', () => {
    render(<ApprovalCard req={TWO_RULES} />);
    expect(scopeLine()).toHaveTextContent(
      /^Always allow removes rm and git push from this agent's ask-first list in this workspace only\.$/
    );
    expect(within(scopeLine()).getByText('git push').tagName).toBe('CODE');
    expect(scopeLine()).not.toHaveTextContent('`');
    expect(screen.getByRole('button', { name: 'Always allow (this workspace)' })).toBeInTheDocument();
  });

  it('names every workspace a shared agent works in', () => {
    render(<ApprovalCard req={{ ...PUSH, persistsToSharedAgent: true, alsoAffectsWorkspaces: ['Docs', 'Infra'] }} />);
    expect(screen.getByRole('button', { name: 'Always allow (every workspace)' })).toBeInTheDocument();
    expect(scopeLine()).toHaveTextContent(
      /^Always allow removes git push from this shared agent's ask-first list in every workspace that has it, now and later\. Today that is this one, “Docs” and “Infra”\.$/
    );
  });

  it('still says "every workspace" for a shared agent nobody else has added yet', () => {
    render(<ApprovalCard req={{ ...PUSH, persistsToSharedAgent: true }} />);
    expect(screen.getByRole('button', { name: 'Always allow (every workspace)' })).toBeInTheDocument();
    expect(scopeLine()).toHaveTextContent(/in every workspace that has it, now and later\.$/);
  });

  it('says this run only when there is no agent to save to', () => {
    render(<ApprovalCard req={{ ...PUSH, agentId: null }} />);
    expect(screen.getByRole('button', { name: 'Always allow (this run)' })).toBeInTheDocument();
    expect(scopeLine()).toHaveTextContent(
      /^Always allow stops asking about git push for the rest of this run only\. Nothing is saved\.$/
    );
  });

  it('names the workspace instead of "this workspace" where one is given (the Fleet)', () => {
    const { unmount } = render(<ApprovalCard req={PUSH} speaker={speaker} workspaceName="Alpha" />);
    expect(scopeLine()).toHaveTextContent(/in “Alpha” only\.$/);
    expect(screen.getByRole('button', { name: 'Always' })).toHaveAttribute('title', 'Always allow (Alpha)');
    unmount();

    render(
      <ApprovalCard
        req={{ ...PUSH, persistsToSharedAgent: true, alsoAffectsWorkspaces: ['Docs'] }}
        speaker={speaker}
        workspaceName="Alpha"
      />
    );
    expect(scopeLine()).toHaveTextContent(/Today that is “Alpha” and “Docs”\.$/);
  });
});

describe('ApprovalCard — Deny and its reason', () => {
  const reasonField = () => screen.getByRole('textbox', { name: /reason for denying/i });

  it('shows the reason field from the start, without taking focus', () => {
    render(<ApprovalCard req={PUSH} />);
    expect(reasonField()).toBeVisible();
    expect(reasonField()).not.toHaveFocus();
    expect(screen.getByRole('button', { name: 'Deny' })).toHaveAccessibleDescription('Reason for denying (optional)');
  });

  it('denies in one click with no reason when the field is empty', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'deny' } }]);
  });

  it('denies in one click with the typed reason, trimmed', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.type(reasonField(), '  Push after review  ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'deny', reason: 'Push after review' } }]);
  });

  it('omits the reason when the field holds only whitespace', async () => {
    render(<ApprovalCard req={PUSH} />);
    await userEvent.type(reasonField(), '   ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'deny' } }]);
  });

  it('keeps the reason editable after a failed submission', async () => {
    mockInvoke.mockRejectedValueOnce('Approval channel closed');
    render(<ApprovalCard req={PUSH} />);
    await userEvent.type(reasonField(), 'no');
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Approval channel closed');
    expect(reasonField()).toHaveValue('no');
    expect(reasonField()).toBeEnabled();
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([
      { requestId: 'req-1', decision: { kind: 'deny', reason: 'no' } },
      { requestId: 'req-1', decision: { kind: 'deny', reason: 'no' } },
    ]);
  });
});

describe('ApprovalCard — spoken (the Fleet)', () => {
  it('drops the tag and the collapse, speaks short chips, and still says where Always allow applies', () => {
    render(<ApprovalCard req={PUSH} speaker={speaker} />);
    expect(screen.queryByText('Permission requested')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /collapse/i })).not.toBeInTheDocument();
    expect(screen.getByLabelText('Command')).toHaveTextContent('cargo test && git push origin main');
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['Once', 'Always', 'Deny']);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Deny' })).not.toHaveAttribute('aria-describedby');
    expect(screen.getByRole('button', { name: 'Always' }).title).toBe('Always allow (this workspace)');
    expect(screen.getByText(/^Always allow removes/)).toHaveTextContent(/in this workspace only\.$/);
  });

  it('sends the same decision as the full card', async () => {
    render(<ApprovalCard req={{ ...PUSH, persistsToSharedAgent: true }} speaker={speaker} />);
    const always = screen.getByRole('button', { name: 'Always' });
    expect(always.title).toMatch(/^Always allow \(every workspace\)/);
    await userEvent.click(always);
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'alwaysAllow' } }]);
  });

  it('keeps the answer open when Always allow cannot be saved, and a denial still answers it', async () => {
    failAlwaysAllow('Could not save Always allow: disk full');
    render(<ApprovalCard req={PUSH} speaker={speaker} />);
    await userEvent.click(screen.getByRole('button', { name: 'Always' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not save Always allow: disk full');

    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([
      { requestId: 'req-1', decision: { kind: 'alwaysAllow' } },
      { requestId: 'req-1', decision: { kind: 'deny' } },
    ]);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});

describe('InlineApprovalCard', () => {
  it('draws no card until a request for this workspace arrives, then names who asked', async () => {
    render(<InlineApprovalCard workspaceId="ws-1" />);
    expect(screen.queryByRole('article')).toBeNull();
    expect(screen.getByRole('status')).toBeEmptyDOMElement();
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    expect(screen.getByRole('status')).toHaveTextContent('An agent wants to run a command on your ask-first list.');
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

  it('keeps the card when Always allow cannot be saved, and Allow once then answers it', async () => {
    const failure = 'Could not save Always allow: Workspace agent not found: agent-1';
    failAlwaysAllow(failure);
    render(<InlineApprovalCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    fireRequest(PUSH);
    await userEvent.click(await screen.findByRole('button', { name: 'Always allow (this workspace)' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(failure);
    // The failed save re-emits the request; a view already showing its card ignores the repeat.
    fireRequest(PUSH);
    expect(screen.getAllByRole('article')).toHaveLength(1);
    expect(screen.getByRole('alert')).toHaveTextContent(failure);
    expect(screen.getByRole('button', { name: 'Deny' })).toBeEnabled();

    await userEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    expect(submitted()).toEqual([
      { requestId: 'req-1', decision: { kind: 'alwaysAllow' } },
      { requestId: 'req-1', decision: { kind: 'allowOnce' } },
    ]);
    await waitFor(() => expect(screen.queryByRole('article')).toBeNull());
  });

  it('leaves focus where the user is typing when cards arrive; Deny answers in one click and focus moves on', async () => {
    const user = userEvent.setup();
    render(
      <>
        <textarea aria-label="Message" />
        <InlineApprovalCard workspaceId="ws-1" />
      </>
    );
    await waitFor(() => expect(listenHandlers['permissions://request']).toBeTruthy());
    const composer = screen.getByRole('textbox', { name: 'Message' });
    await user.click(composer);
    fireRequest(PUSH);
    fireRequest(TWO_RULES);
    await screen.findByText('rm -f build.log && git push');
    await user.keyboard('hi');
    expect(composer).toHaveFocus();
    expect(composer).toHaveValue('hi');

    const first = within(screen.getAllByRole('article')[0]!);
    await user.type(first.getByRole('textbox', { name: /reason for denying/i }), ' not now ');
    await user.click(first.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([{ requestId: 'req-1', decision: { kind: 'deny', reason: 'not now' } }]);
    await waitFor(() => expect(screen.getAllByRole('article')).toHaveLength(1));
    expect(screen.getByRole('article')).toHaveFocus();
    expect(screen.getByRole('textbox', { name: /reason for denying/i })).toHaveValue('');
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
