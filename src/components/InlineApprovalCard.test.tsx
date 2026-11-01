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

// Every string the card can show: the text, plus the tooltip, label and
// placeholder attributes.
const cardStrings = (root: HTMLElement): string[] => {
  const strings: string[] = [];
  const visit = (node: Node) => {
    node.childNodes.forEach((child) => {
      if (child.nodeType === Node.TEXT_NODE) {
        const text = child.textContent?.trim();
        if (text) strings.push(text);
        return;
      }
      if (child instanceof HTMLElement) {
        (['title', 'aria-label', 'placeholder'] as const).forEach((attr) => {
          const value = child.getAttribute(attr);
          if (value) strings.push(value);
        });
        visit(child);
      }
    });
  };
  visit(root);
  return strings;
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

describe('ApprovalCard — one optional reason for the denial', () => {
  it('offers one reason field once a denial is chosen, and sends it trimmed', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);

    expect(screen.queryByRole('textbox', { name: /reason for denying/i })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    // The denial waits for the confirm below, so the reason has a place to go.
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());

    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    await user.tab();
    expect(reason).toHaveFocus();
    await user.type(reason, '  Unsafe in this workspace  ');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyOnce', reason: 'Unsafe in this workspace' }],
    });
  });

  it('sends the reason with deny always, next to the saved prefix', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Always deny (this agent)' }));
    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'Use a safer command');
    await user.click(screen.getByRole('button', { name: 'Deny and block always' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyAlways', scope: 'agent', prefix: 'rg', reason: 'Use a safer command' }],
    });
  });

  it('omits the reason when the field holds only whitespace', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), '   ');
    await user.click(screen.getByRole('button', { name: 'Deny command' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyOnce' }],
    });
    // The key is absent, not an undefined value.
    expect(Object.keys(mockInvoke.mock.calls.at(-1)![1].decisions[0])).toEqual(['kind']);
  });

  it('never sends a reason with an allow decision', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Allow once' }));

    expect(screen.queryByRole('textbox', { name: /reason for denying/i })).not.toBeInTheDocument();
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'allowOnce' }],
    });
  });

  it('sends no reason key at all on an allow, even after a reason was typed', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} />);
    // Deny the first part so the reason field appears, type one, then let
    // the part be allowed instead.
    await user.click(screen.getAllByRole('button', { name: 'Deny once' })[0]!);
    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'Stay in project root');
    await user.click(screen.getAllByRole('button', { name: 'Always allow (this agent)' })[0]!);
    await user.click(screen.getAllByRole('button', { name: 'Deny once' })[1]!);
    await user.click(screen.getByRole('button', { name: 'Submit decisions' }));

    const decisions = mockInvoke.mock.calls.at(-1)![1].decisions;
    // The allow carries no reason key, not even an undefined one.
    expect(Object.keys(decisions[0])).toEqual(['kind', 'scope', 'prefix']);
    // The denial carries the one reason.
    expect(Object.keys(decisions[1])).toEqual(['kind', 'reason']);
    expect(decisions[1].reason).toBe('Stay in project root');
  });

  it('does not send the decision twice', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));

    const confirm = screen.getByRole('button', { name: 'Deny command' });
    await user.click(confirm);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(confirm).toBeEnabled());
    // Nothing removed the card, so a second click must not send again.
    await user.click(confirm);
    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });

  it('waits for every part, then sends the one reason to the denied part only', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} />);
    await user.click(screen.getAllByRole('button', { name: 'Deny once' })[0]!);
    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'Stay in project root');
    expect(screen.getByRole('button', { name: 'Submit decisions' })).toBeDisabled();
    await user.click(screen.getAllByRole('button', { name: 'Allow once' })[1]!);
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());
    await user.click(screen.getByRole('button', { name: 'Submit decisions' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'denyOnce', reason: 'Stay in project root' }, { kind: 'allowOnce' }],
    });
  });

  it('gives "deny all" the same single reason field for every part', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} speaker={speaker} />);
    await user.click(screen.getByRole('button', { name: 'Deny all' }));
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());
    expect(screen.getAllByRole('textbox', { name: /reason for denying/i })).toHaveLength(1);

    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'Wrong target');
    await user.click(screen.getByRole('button', { name: 'Submit decisions' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'denyOnce', reason: 'Wrong target' }, { kind: 'denyOnce', reason: 'Wrong target' }],
    });
  });

  it('sends the one reason with every denied part, always or not', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={PIPELINE} />);
    await user.click(screen.getAllByRole('button', { name: 'Always deny (this agent)' })[0]!);
    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'Wrong target');
    await user.click(screen.getAllByRole('button', { name: 'Deny once' })[1]!);
    await user.click(screen.getByRole('button', { name: 'Submit decisions' }));

    const decisions = mockInvoke.mock.calls.at(-1)![1].decisions;
    expect(decisions[0]).toEqual({ kind: 'denyAlways', scope: 'agent', prefix: 'cd', reason: 'Wrong target' });
    expect(decisions[1]).toEqual({ kind: 'denyOnce', reason: 'Wrong target' });
  });

  it('says the reason goes to the agent with this response, and never that it is kept', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));

    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    expect(reason).toHaveAccessibleDescription(/Sent to the agent with this response\./);
    expect(screen.getByText('Sent to the agent with this response.')).toBeInTheDocument();
    // No string on the card may claim the reason is kept somewhere. (The
    // prefix is saved, so a persistence word is only a problem when it is
    // about the reason.)
    const card = screen.getByRole('article', { name: 'Permission requested' });
    for (const text of cardStrings(card)) {
      if (!/\b(save|saved|saves|remember|remembered|persist|persisted|store|stored|keep|kept)\b/i.test(text)) continue;
      expect(text).not.toMatch(/reason|why/i);
    }
  });

  it('shows how much of the limit the reason uses', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));

    expect(screen.getByText('0/500 characters')).toBeVisible();
    await user.type(screen.getByRole('textbox', { name: /reason for denying/i }), 'abc');
    expect(screen.getByText('3/500 characters')).toBeVisible();
  });

  it('accepts 500 Unicode characters and drops the next', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Deny once' }));
    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
    const emojis = '😀'.repeat(500);
    // No maxlength: it counts UTF-16 units, so it would cut a character in
    // half. The field counts code points, as the backend does.
    expect(reason).not.toHaveAttribute('maxlength');
    fireEvent.change(reason, { target: { value: `${emojis}😀` } });
    expect(reason).toHaveValue(emojis);
    expect(screen.getByText('500/500 characters')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Deny command' }));

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'req-1',
      decisions: [{ kind: 'denyOnce', reason: emojis }],
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
    const reason = screen.getByRole('textbox', { name: /reason for denying/i });
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

  it('explains that a denial needs a confirm', () => {
    render(<ApprovalCard req={PIPELINE} />);
    expect(screen.getByText('2 parts need a decision (confirm after choosing any denial):')).toBeInTheDocument();
  });

  it('says a denial needs a confirm, even with one part', () => {
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    expect(screen.getByText('Choose how to handle this command (a denial needs a confirm):')).toBeInTheDocument();
  });

  it('names the block rule on the confirm, because it outlives the request', async () => {
    const user = userEvent.setup();
    render(<ApprovalCard req={SINGLE_SEGMENT_REQUEST as PermissionRequest} />);
    await user.click(screen.getByRole('button', { name: 'Always deny (this agent)' }));

    expect(screen.getByRole('button', { name: 'Deny and block always' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Deny command' })).not.toBeInTheDocument();
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

// A part the backend could not reduce to a stable prefix: it can be
// allowed, but not saved as a rule. The bulk "allow all" click falls back
// to allow-once for it, so the card has to say that before the click.
const MIXED = {
  requestId: 'perm-mixed',
  workspaceId: 'ws-1',
  agentId: null,
  agentName: null,
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
  command: 'git status && ./deploy --now',
  segments: [
    { text: 'git status', kind: 'simple', suggestedPrefix: 'git' },
    { text: './deploy --now', kind: 'opaque', suggestedPrefix: '' },
  ],
} as unknown as PermissionRequest;

const NOTHING_SAVABLE = {
  ...MIXED,
  requestId: 'perm-none',
  command: './deploy --now && ./migrate',
  segments: [
    { text: './deploy --now', kind: 'opaque', suggestedPrefix: '' },
    { text: './migrate', kind: 'opaque', suggestedPrefix: '' },
  ],
} as unknown as PermissionRequest;

// Two parts with the same text, next to one that can be saved: the note
// has to tell the two apart.
const SAME_TEXT_TWICE = {
  ...MIXED,
  requestId: 'perm-same',
  command: 'cd /srv/app && git status && git status',
  segments: [
    { text: 'cd /srv/app', kind: 'simple', suggestedPrefix: 'cd' },
    { text: 'git status', kind: 'simple', suggestedPrefix: '' },
    { text: 'git status', kind: 'simple', suggestedPrefix: '' },
  ],
} as unknown as PermissionRequest;

// A part the backend sent without text: it still gets a name.
const PART_WITHOUT_TEXT = {
  ...MIXED,
  requestId: 'perm-blank',
  command: 'cd /srv/app &&  && ./deploy --now',
  segments: [
    { text: 'cd /srv/app', kind: 'simple', suggestedPrefix: 'cd' },
    { text: '', kind: 'simple', suggestedPrefix: '' },
    { text: './deploy --now', kind: 'opaque', suggestedPrefix: '' },
  ],
} as unknown as PermissionRequest;

// The note the button points at, so the assertions read what the user
// is actually shown next to the button they are about to press.
const noteOf = (button: HTMLElement) => {
  const id = button.getAttribute('aria-describedby');
  if (!id) throw new Error('the bulk button describes no note');
  const note = document.getElementById(id);
  if (!note) throw new Error(`no note with id ${id}`);
  return note;
};

describe('ApprovalCard — what the bulk "allow all" click actually saves', () => {
  it('names the parts it saves and the parts it only allows once', async () => {
    render(<ApprovalCard req={MIXED} />);

    // The button cannot claim "always" for a part that has no prefix.
    const bulk = screen.getByRole('button', { name: 'Allow all (always where possible)' });
    expect(screen.queryByRole('button', { name: 'Allow all (always)' })).toBeNull();

    // The note is visible before the click (the card is dismissed on
    // submit) and the button points at it.
    const note = noteOf(bulk);
    // The saved part is not in the once-only list.
    expect(note).not.toHaveTextContent('git status');
    expect(note).toHaveTextContent('This click saves: 1) git');
    expect(note).toHaveTextContent('Allowed once only: 2) ./deploy --now');

    await userEvent.click(bulk);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-mixed',
      decisions: [
        { kind: 'allowAlways', scope: 'agent', prefix: 'git' },
        { kind: 'allowOnce' },
      ],
    });
  });

  it('offers no bulk action when no part can be saved: each part is answered on its own', () => {
    render(<ApprovalCard req={NOTHING_SAVABLE} />);

    // Nothing the bulk click could decide would be saved, so there is no
    // bulk action: the parts are answered with their own chips.
    expect(screen.queryByRole('button', { name: /^Allow (all|the rest)/ })).toBeNull();
    expect(screen.queryByText(/Allowed once only/)).toBeNull();
    expect(screen.getAllByRole('button', { name: 'Allow once' })).toHaveLength(2);
    expect(screen.getAllByRole('button', { name: 'Deny once' })).toHaveLength(2);
  });

  it('keeps the plain label and says nothing when every part can be saved', async () => {
    render(<ApprovalCard req={PIPELINE} />);

    const bulk = screen.getByRole('button', { name: 'Allow all (always)' });
    expect(screen.queryByText(/Allowed once only/)).toBeNull();
    expect(bulk.getAttribute('aria-describedby')).toBeNull();

    await userEvent.click(bulk);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [
        { kind: 'allowAlways', scope: 'agent', prefix: 'cd' },
        { kind: 'allowAlways', scope: 'agent', prefix: 'cargo test' },
      ],
    });
  });

  it('never turns a decision the user made into the opposite one', async () => {
    render(<ApprovalCard req={PIPELINE} />);

    // The user denies the first part. Nothing is submitted yet: the
    // other part is still undecided.
    await userEvent.click(screen.getAllByRole('button', { name: 'Deny once' })[0]!);
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());

    // The bulk click says what it will do with the rest, and keeps the
    // deny where the user put it.
    const bulk = screen.getByRole('button', { name: 'Allow the rest (always)' });
    const note = noteOf(bulk);
    expect(note).toHaveTextContent('Keeps your decision on 1) cd /srv/app (Deny once)');
    expect(note).toHaveTextContent('This click saves: 2) cargo test');

    await userEvent.click(bulk);
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());
    await userEvent.type(screen.getByRole('textbox', { name: 'Reason for denying (optional)' }), 'Use local data');
    await userEvent.click(screen.getByRole('button', { name: 'Submit decisions' }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [
        { kind: 'denyOnce', reason: 'Use local data' },
        { kind: 'allowAlways', scope: 'agent', prefix: 'cargo test' },
      ],
    });
  });

  it('keeps an explicit allow-once as allow-once', async () => {
    render(<ApprovalCard req={PIPELINE} />);

    await userEvent.click(screen.getAllByRole('button', { name: 'Allow once' })[0]!);
    const bulk = screen.getByRole('button', { name: 'Allow the rest (always)' });
    await userEvent.click(bulk);

    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [
        { kind: 'allowOnce' },
        { kind: 'allowAlways', scope: 'agent', prefix: 'cargo test' },
      ],
    });
  });

  it('says what allowing a part without a prefix means, without denying the deny chip', () => {
    const bare = { ...MIXED, requestId: 'perm-bare', command: './run.sh', segments: [MIXED.segments[1]!] };
    render(<ApprovalCard req={bare} />);

    // Nothing to save as an allow rule, so there is no "always allow"...
    expect(screen.queryByRole('button', { name: 'Always allow (this agent)' })).toBeNull();
    // ...but the part is still saved as a deny rule, so the card must not
    // claim the part cannot be saved at all.
    expect(screen.getByRole('button', { name: 'Always deny (this agent)' })).toBeInTheDocument();
    const hint = screen.getByText(/No prefix to save for this part/);
    expect(hint).toHaveTextContent('allowing it applies to this run only');
    expect(hint).not.toHaveTextContent(/cannot be saved/);
  });

  it('drops the row\'s "always allow" when the prefix field is cleared, and the bulk action goes with it', async () => {
    render(<ApprovalCard req={MIXED} />);

    await userEvent.clear(screen.getByDisplayValue('git'));

    // The row no longer offers to save a rule...
    expect(screen.queryByRole('button', { name: 'Always allow (this agent)' })).toBeNull();
    // ...and with no part left that could be saved, the bulk action goes
    // with it: the parts are answered one by one.
    expect(screen.queryByRole('button', { name: /^Allow (all|the rest)/ })).toBeNull();

    await userEvent.click(screen.getAllByRole('button', { name: 'Allow once' })[0]!);
    await userEvent.click(screen.getAllByRole('button', { name: 'Allow once' })[1]!);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-mixed',
      decisions: [{ kind: 'allowOnce' }, { kind: 'allowOnce' }],
    });
  });

  it('falls back to allow-once when the prefix of an "always allow" is cleared', async () => {
    render(<ApprovalCard req={PIPELINE} />);

    // The user picks "always allow" for the first part...
    await userEvent.click(screen.getAllByRole('button', { name: 'Always allow (this agent)' })[0]!);
    // ...and then empties the prefix field: there is nothing left to save.
    await userEvent.clear(screen.getByDisplayValue('cd'));
    expect(screen.queryAllByRole('button', { name: 'Always allow (this agent)' })).toHaveLength(1);

    // The note says the part is allowed once only, not saved.
    const bulk = screen.getByRole('button', { name: 'Allow the rest (always)' });
    expect(noteOf(bulk)).toHaveTextContent('Keeps your decision on 1) cd /srv/app (Allow once)');

    // Deciding the other part submits both as allow-once.
    await userEvent.click(screen.getAllByRole('button', { name: 'Allow once' })[1]!);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [{ kind: 'allowOnce' }, { kind: 'allowOnce' }],
    });
  });

  it('saves the prefix the user typed, trimmed', async () => {
    render(<ApprovalCard req={MIXED} />);

    const field = screen.getByDisplayValue('git');
    await userEvent.clear(field);
    await userEvent.type(field, '  git  ');

    const bulk = screen.getByRole('button', { name: 'Allow all (always where possible)' });
    const note = noteOf(bulk);
    expect(note).toHaveTextContent('This click saves: 1) git');
    // The note names the prefix the click will save, trimmed like the
    // submitted one.
    expect(note.querySelector('code')?.textContent).toBe('git');
    await userEvent.click(bulk);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-mixed',
      decisions: [
        { kind: 'allowAlways', scope: 'agent', prefix: 'git' },
        { kind: 'allowOnce' },
      ],
    });
  });

  it('treats a whitespace-only prefix as no prefix at all', async () => {
    render(<ApprovalCard req={MIXED} />);

    const field = screen.getByDisplayValue('git');
    await userEvent.clear(field);
    await userEvent.type(field, '   ');

    // A whitespace-only prefix is no prefix: the part cannot be saved...
    expect(screen.queryByRole('button', { name: 'Always allow (this agent)' })).toBeNull();
    // ...so there is no bulk action either.
    expect(screen.queryByRole('button', { name: /^Allow (all|the rest)/ })).toBeNull();

    await userEvent.click(screen.getAllByRole('button', { name: 'Allow once' })[0]!);
    await userEvent.click(screen.getAllByRole('button', { name: 'Allow once' })[1]!);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-mixed',
      decisions: [{ kind: 'allowOnce' }, { kind: 'allowOnce' }],
    });
  });

  it('tells apart two parts with the same text', () => {
    render(<ApprovalCard req={SAME_TEXT_TWICE} />);

    const bulk = screen.getByRole('button', { name: 'Allow all (always where possible)' });
    const note = noteOf(bulk);
    expect(note).toHaveTextContent('Allowed once only: 2) git status, 3) git status');
  });

  it('never shows an empty part in the note', () => {
    render(<ApprovalCard req={PART_WITHOUT_TEXT} />);

    const bulk = screen.getByRole('button', { name: 'Allow all (always where possible)' });
    const note = noteOf(bulk);
    expect(note).toHaveTextContent('Allowed once only: 2) (empty), 3) ./deploy --now');
    for (const chip of note.querySelectorAll('code')) {
      expect(chip.textContent?.trim()).not.toBe('');
    }
  });

  it('saves the command text as the deny prefix when the part has no prefix', async () => {
    const bare = { ...MIXED, requestId: 'perm-deny-text', command: './run.sh', segments: [MIXED.segments[1]!] };
    render(<ApprovalCard req={bare} />);

    // Without a prefix there is nothing else to name the rule after, so
    // the deny rule is saved under the command text itself.
    await userEvent.click(screen.getByRole('button', { name: 'Always deny (this agent)' }));
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());
    await userEvent.click(screen.getByRole('button', { name: /Deny command|Deny and block always|Submit decisions/ }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-deny-text',
      decisions: [{ kind: 'denyAlways', scope: 'agent', prefix: './deploy --now' }],
    });
  });

  it('says that the click also saves the rule of a part the user already allowed', async () => {
    render(<ApprovalCard req={PIPELINE} />);

    // The user allows the first part always; the second is still pending.
    await userEvent.click(screen.getAllByRole('button', { name: 'Always allow (this agent)' })[0]!);

    const bulk = screen.getByRole('button', { name: 'Allow the rest (always)' });
    const note = noteOf(bulk);
    // The kept decision is a rule, and this click saves it: the note has
    // to say so, or it under-reports what is persisted.
    expect(note).toHaveTextContent(
      'Keeps your decision on 1) cd /srv/app (Always allow), saved as an allow rule for cd'
    );

    await userEvent.click(bulk);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [
        { kind: 'allowAlways', scope: 'agent', prefix: 'cd' },
        { kind: 'allowAlways', scope: 'agent', prefix: 'cargo test' },
      ],
    });
  });

  it('does not list the decided part under "This click saves"', async () => {
    render(<ApprovalCard req={PIPELINE} />);
    await userEvent.click(screen.getAllByRole('button', { name: 'Always allow (this agent)' })[0]!);

    const note = noteOf(screen.getByRole('button', { name: 'Allow the rest (always)' }));
    const saves = within(note).getByText(/This click saves:/);
    expect(saves).toHaveTextContent('2) cargo test');
    // The part the user already decided is not what this click saves.
    expect(saves).not.toHaveTextContent('cd');
  });

  it('does not promise a deny rule for a part with no text to name', () => {
    render(<ApprovalCard req={PART_WITHOUT_TEXT} />);

    const hints = screen.getAllByText(/No prefix to save for this part/);
    expect(hints).toHaveLength(2);
    // The part with no text has nothing to save a rule under: the card
    // must not promise a persistent deny for it.
    expect(hints[0]).toHaveTextContent('no rule can be saved for this part');
    expect(hints[0]).not.toHaveTextContent('Always deny still blocks');
    // It has no deny chip either; the other two parts have one.
    expect(screen.getAllByRole('button', { name: 'Always deny (this agent)' })).toHaveLength(2);
    // The other part without a prefix has text, so its deny rule is saved.
    expect(hints[1]).toHaveTextContent('Always deny still blocks commands starting with this text');
  });

  it('offers no bulk action in either card when nothing can be saved', () => {
    const plain = render(<ApprovalCard req={NOTHING_SAVABLE} />);
    expect(within(plain.container).queryByRole('button', { name: /^Allow (all|the rest)/ })).toBeNull();

    const spoken = render(<ApprovalCard req={NOTHING_SAVABLE} speaker={speaker} />);
    expect(within(spoken.container).queryByRole('button', { name: /^(All|Rest) / })).toBeNull();
  });

  it('drops the bulk action in both cards once the only savable part is decided', async () => {
    const plain = render(<ApprovalCard req={MIXED} />);
    await userEvent.click(within(plain.container).getAllByRole('button', { name: 'Always allow (this agent)' })[0]!);
    expect(within(plain.container).queryByRole('button', { name: /Allow (all|the rest)/ })).toBeNull();

    const spoken = render(<ApprovalCard req={MIXED} speaker={speaker} />);
    await userEvent.click(within(spoken.container).getAllByRole('button', { name: 'Always · git' })[0]!);
    expect(within(spoken.container).queryByRole('button', { name: /^(All|Rest) / })).toBeNull();
  });

  it('submits only known decision kinds and the prefix the backend suggested', async () => {
    render(<ApprovalCard req={MIXED} />);
    await userEvent.click(screen.getByRole('button', { name: 'Allow all (always where possible)' }));

    const call = mockInvoke.mock.calls.find(([cmd]) => cmd === 'submit_permission_decision');
    expect(call).toBeTruthy();
    const decisions = (call![1] as { decisions: Array<Record<string, unknown>> }).decisions;
    const knownKinds = ['allowOnce', 'denyOnce', 'allowAlways', 'denyAlways'];
    for (const decision of decisions) {
      expect(knownKinds).toContain(decision.kind);
      if (decision.kind === 'allowAlways' || decision.kind === 'denyAlways') {
        expect(decision.scope).toBe('agent');
        // The backend's own suggestion, never a prefix invented here.
        expect(['git', 'cargo test', 'cd']).toContain(decision.prefix);
      }
    }
  });

  it('says the same in the spoken card: the chip names what it saves', async () => {
    render(<ApprovalCard req={MIXED} speaker={speaker} />);

    const bulk = screen.getByRole('button', { name: 'All \u00b7 1 of 2 saved' });
    expect(screen.queryByRole('button', { name: 'All always' })).toBeNull();
    expect(bulk.getAttribute('title')).toMatch(/allowed once only/);

    const note = noteOf(bulk);
    expect(note).not.toHaveTextContent('git status');
    expect(note).toHaveTextContent('This click saves: 1) git');
    expect(note).toHaveTextContent('Allowed once only: 2) ./deploy --now');

    await userEvent.click(bulk);
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-mixed',
      decisions: [
        { kind: 'allowAlways', scope: 'agent', prefix: 'git' },
        { kind: 'allowOnce' },
      ],
    });
  });

  it('keeps a deny in the spoken card too', async () => {
    render(<ApprovalCard req={PIPELINE} speaker={speaker} />);

    await userEvent.click(screen.getAllByRole('button', { name: 'Deny' })[0]!);
    const bulk = screen.getByRole('button', { name: 'Rest always' });
    expect(noteOf(bulk)).toHaveTextContent('Keeps your decision on 1) cd /srv/app (Deny once)');

    await userEvent.click(bulk);
    expect(mockInvoke).not.toHaveBeenCalledWith('submit_permission_decision', expect.anything());
    await userEvent.click(screen.getByRole('button', { name: /Deny command|Deny and block always|Submit decisions/ }));
    expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
      requestId: 'perm-1',
      decisions: [
        { kind: 'denyOnce' },
        { kind: 'allowAlways', scope: 'agent', prefix: 'cargo test' },
      ],
    });
  });

  it('offers no bulk action in the spoken card when nothing can be saved', () => {
    render(<ApprovalCard req={NOTHING_SAVABLE} speaker={speaker} />);

    // The same rule as the workspace card: nothing pending can be saved,
    // so the parts are answered with their own chips.
    expect(screen.queryByRole('button', { name: /^(All|Rest) / })).toBeNull();
    expect(screen.getAllByText(/No prefix to save/)).toHaveLength(2);
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
