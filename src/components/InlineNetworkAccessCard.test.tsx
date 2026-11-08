import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { LocalListenerView, NetworkAccessRequest, WorkspaceAgentResponse } from '../generated/bindings';
import { rosterAgent } from './AssistantChat/messageSender';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
const listenHandlers = vi.hoisted(() => ({}) as Record<string, (event: { payload: unknown }) => void>);
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listenHandlers[name] = handler;
    return Promise.resolve(() => {});
  }),
}));

import InlineNetworkAccessCard, { NetworkAccessCard } from './InlineNetworkAccessCard';

const request = (overrides: Partial<NetworkAccessRequest> = {}): NetworkAccessRequest => ({
  requestId: 'net-1',
  workspaceId: 'ws-1',
  agentId: 'agent-1',
  agentName: null,
  host: 'mirror.example.org',
  port: 443,
  reason: 'Download a test fixture.',
  runHosts: [
    { host: 'github.com', port: 443, allowed: true },
    { host: 'mirror.example.org', port: 443, allowed: false },
  ],
  suggestedWildcard: '*.example.org',
  persistsToSharedAgent: false,
  alsoAffectsWorkspaces: [],
  ...overrides,
});

const requester = rosterAgent('wa-rust', [
  {
    id: 'wa-rust',
    workspaceId: 'ws-1',
    agentDefinitionId: 'def-rust',
    displayName: 'Rust Architect',
    enabled: true,
    isDefault: false,
    agentName: null,
    agentDescription: null,
    providerConnectionIds: [],
    skillIds: [],
    selectedMcpServerIds: [],
    execution: {},
    avatar: null,
    createdAt: 1n,
    updatedAt: 1n,
  } as unknown as WorkspaceAgentResponse,
]);

afterEach(() => {
  mockInvoke.mockReset();
});

const submitted = () =>
  mockInvoke.mock.calls.filter(([cmd]) => cmd === 'submit_network_access_decision').map(([, args]) => args);

describe('NetworkAccessCard', () => {
  it('shows the host, the reason and what this run reached', () => {
    render(<NetworkAccessCard req={request()} />);
    expect(screen.getByText('Network access requested')).toBeInTheDocument();
    expect(screen.getByText('mirror.example.org:443')).toBeInTheDocument();
    expect(screen.getByText('not in allowed hosts')).toBeInTheDocument();
    expect(screen.getByText('Download a test fixture.')).toBeInTheDocument();
    const runLine = screen.getByText(/^This run:/);
    expect(runLine).toHaveTextContent('This run: github.com ✓ · mirror.example.org ✕');
    expect(within(runLine).getByRole('img', { name: 'allowed' })).toBeInTheDocument();
    expect(within(runLine).getByRole('img', { name: 'refused' })).toBeInTheDocument();
  });

  it('reads an IDN host in Unicode, shows its ASCII form, and sends the ASCII form', async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    const req = request({
      host: 'mirror.xn--bcher-kva.example',
      suggestedWildcard: '*.xn--bcher-kva.example',
      runHosts: [{ host: 'xn--mnchen-3ya.de', port: 443, allowed: true }],
    });
    render(<NetworkAccessCard req={req} requester={requester} />);
    expect(screen.getByText('mirror.bücher.example:443').tagName).toBe('CODE');
    expect(screen.getByText('mirror.xn--bcher-kva.example')).toBeInTheDocument();
    expect(screen.getByText(/^This run:/)).toHaveTextContent('This run: münchen.de ✓');
    expect(screen.getByText(/add their entry/)).toHaveTextContent(/^“Always” and “Allow \*\.bücher\.example” add their entry/);
    expect(screen.getByRole('button', { name: 'Always for Rust Architect' })).toHaveAttribute(
      'title',
      "Adds mirror.bücher.example to this agent's allowed hosts.",
    );

    await user.click(screen.getByRole('button', { name: 'Allow *.bücher.example' }));
    expect(submitted().at(-1)).toEqual({
      requestId: 'net-1',
      decision: { kind: 'allowAlways', entry: '*.xn--bcher-kva.example' },
    });
  });

  it('brackets an IPv6 host so its port reads apart from it', () => {
    render(<NetworkAccessCard req={request({ host: '2001:db8::1', port: 443, suggestedWildcard: null })} />);
    expect(screen.getByText('[2001:db8::1]:443')).toBeInTheDocument();
  });

  it('names a port of this computer as a port, in the run line and when spoken', () => {
    const req = request({
      host: 'localhost',
      port: 5432,
      suggestedWildcard: null,
      runHosts: [
        { host: 'localhost', port: 5432, allowed: false },
        { host: 'localhost', port: 6379, allowed: true },
        { host: 'github.com', port: 443, allowed: true },
      ],
    });
    const { unmount } = render(<NetworkAccessCard req={req} />);
    expect(screen.getByText(/^This run:/)).toHaveTextContent(
      'This run: port 5432 on this computer ✕ · port 6379 on this computer ✓ · github.com ✓',
    );
    unmount();

    render(<NetworkAccessCard req={req} speaker={(prompt: ReactNode) => <div>{prompt}</div>} />);
    expect(screen.getByText('May I reach port 5432 on this computer?')).toBeInTheDocument();
  });

  it('names the first eight hosts of the run and counts the rest', () => {
    const runHosts = Array.from({ length: 11 }, (_, i) => ({ host: `h${i}.example.org`, port: 443, allowed: i % 2 === 0 }));
    render(<NetworkAccessCard req={request({ runHosts })} />);
    const runLine = screen.getByText(/^This run:/);
    expect(runLine).toHaveTextContent(/^This run: h0\.example\.org ✓ · .* · h7\.example\.org ✕ and 3 more$/);
    expect(runLine).not.toHaveTextContent('h8.example.org');
    expect(within(runLine).getAllByRole('img')).toHaveLength(8);
  });

  it('says the entry goes to the allowed hosts, as the settings call the list', () => {
    render(<NetworkAccessCard req={request()} requester={requester} />);
    expect(screen.getByRole('button', { name: 'Always for Rust Architect' })).toHaveAttribute(
      'title',
      "Adds mirror.example.org to this agent's allowed hosts.",
    );
    expect(screen.getByRole('button', { name: 'Allow *.example.org' })).toHaveAttribute(
      'title',
      "Adds *.example.org to this agent's allowed hosts: every host under it.",
    );
  });

  it('sends each choice with its entry', async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    for (const [label, decision] of [
      ['Allow this run', { kind: 'allowRun' }],
      ['Always for Rust Architect', { kind: 'allowAlways', entry: 'mirror.example.org' }],
      ['Allow *.example.org', { kind: 'allowAlways', entry: '*.example.org' }],
    ] as const) {
      const { unmount } = render(<NetworkAccessCard req={request()} requester={requester} />);
      // A reason is for a denial only; the allows never carry it.
      await user.type(screen.getByLabelText('Reason for denying (optional)'), 'typed anyway');
      await user.click(screen.getByRole('button', { name: label }));
      expect(submitted().at(-1)).toEqual({ requestId: 'net-1', decision });
      unmount();
    }
  });

  it('offers no wildcard when the host has no parent domain to widen to', () => {
    render(<NetworkAccessCard req={request({ host: 'example.org', suggestedWildcard: null })} />);
    expect(screen.queryByRole('button', { name: /^Allow \*\./ })).not.toBeInTheDocument();
    expect(screen.getByText(/adds/)).toHaveTextContent(/Always. adds example\.org to this agent.s allowed hosts in this workspace only\./);
  });

  it('shows the reason field from the start, without taking focus', () => {
    render(<NetworkAccessCard req={request()} />);
    const reason = screen.getByLabelText('Reason for denying (optional)');
    expect(reason).toBeVisible();
    expect(reason).not.toHaveFocus();
    expect(screen.getByRole('button', { name: 'Deny' })).toHaveAccessibleDescription('Reason for denying (optional)');
  });

  it('denies in one click with the reason the user typed, trimmed, or none', async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    const { unmount } = render(<NetworkAccessCard req={request()} />);
    await user.type(screen.getByLabelText('Reason for denying (optional)'), '  use the internal mirror ');
    await user.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([{ requestId: 'net-1', decision: { kind: 'deny', reason: 'use the internal mirror' } }]);
    unmount();

    render(<NetworkAccessCard req={request()} />);
    await user.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted().at(-1)).toEqual({ requestId: 'net-1', decision: { kind: 'deny' } });
    expect(submitted()).toHaveLength(2);
  });

  it('says where Always saves', () => {
    const { unmount } = render(<NetworkAccessCard req={request()} workspaceName="clai" />);
    expect(screen.getByText(/add their entry/)).toHaveTextContent(/to this agent.s allowed hosts in .clai. only\.$/);
    unmount();

    render(
      <NetworkAccessCard
        req={request({ persistsToSharedAgent: true, alsoAffectsWorkspaces: ['docs-site'] })}
        workspaceName="clai"
      />,
    );
    expect(screen.getByText(/add their entry/)).toHaveTextContent(
      /shared agent.s allowed hosts in every workspace that has it, now and later\. Today that is .clai. and .docs-site.\./,
    );
  });

  it.each([
    ['agent', { agentId: null }],
    ['workspace', { workspaceId: null }],
  ])('offers this run only when there is no %s to save an entry on', (_, missing) => {
    render(<NetworkAccessCard req={request(missing)} requester={requester} />);
    expect(screen.getAllByRole('button').map((button) => button.textContent)).toEqual(['▾', 'Allow this run', 'Deny']);
    expect(screen.getByText(/no agent to save/)).toHaveTextContent(
      /^There is no agent to save an entry on, so the host can be allowed for this run only\.$/,
    );
  });

  it('keeps a refused decision on the card so the user can answer again', async () => {
    mockInvoke.mockRejectedValueOnce('not a narrow entry').mockResolvedValue(undefined);
    const onDecided = vi.fn();
    const user = userEvent.setup();
    render(<NetworkAccessCard req={request()} onDecided={onDecided} />);
    await user.click(screen.getByRole('button', { name: 'Allow *.example.org' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('not a narrow entry');
    expect(onDecided).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Allow this run' }));
    expect(onDecided).toHaveBeenCalledWith('net-1');
  });

  it('shows a request answered elsewhere as resolved, with no live decisions', async () => {
    mockInvoke.mockRejectedValue('No pending network access request `net-1` (already resolved or cleared)');
    const onDecided = vi.fn();
    const user = userEvent.setup();
    render(<NetworkAccessCard req={request()} onDecided={onDecided} />);
    await user.click(screen.getByRole('button', { name: 'Deny' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Already answered in another view, or its run has ended.');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Allow|^Deny/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect(onDecided).toHaveBeenCalledWith('net-1');
  });

  it('speaks on the Fleet with short chips, and denies there in one click with no reason field', async () => {
    mockInvoke.mockResolvedValue(undefined);
    render(<NetworkAccessCard req={request()} speaker={(prompt: ReactNode) => <div>{prompt}</div>} />);
    expect(screen.queryByText('Network access requested')).not.toBeInTheDocument();
    expect(screen.getByText('mirror.example.org:443')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'This run' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Always' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: '*.example.org' })).toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(submitted()).toEqual([{ requestId: 'net-1', decision: { kind: 'deny' } }]);
  });
});

const fire = (name: string, payload: unknown) => act(() => listenHandlers[name]!({ payload }));

describe('InlineNetworkAccessCard', () => {
  it('shows a request from network-access://request and drops it on network-access://resolved', async () => {
    mockInvoke.mockResolvedValue([]);
    render(<InlineNetworkAccessCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['network-access://resolved']).toBeDefined());
    fire('network-access://request', request());
    expect(screen.getByRole('article', { name: 'Network access requested' })).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('An agent wants network access to mirror.example.org:443.');
    fire('network-access://resolved', { requestId: 'net-1' });
    expect(screen.queryByRole('article')).not.toBeInTheDocument();
  });

  it('announces a request for a port on this computer by its port', async () => {
    mockInvoke.mockResolvedValue([]);
    render(<InlineNetworkAccessCard workspaceId="ws-1" />);
    await waitFor(() => expect(listenHandlers['network-access://request']).toBeDefined());
    fire(
      'network-access://request',
      request({
        host: 'localhost',
        port: 5432,
        suggestedWildcard: null,
        runHosts: [],
        localListener: { kind: 'runOnly', reason: 'unseen', holders: [] },
      }),
    );
    expect(screen.getByRole('status')).toHaveTextContent(
      /^An agent wants network access to port 5432 on this computer\.$/,
    );
  });

  it('drops the card once its decision is accepted', async () => {
    mockInvoke.mockImplementation((cmd: string) =>
      Promise.resolve(cmd === 'list_pending_network_access_requests' ? [request()] : undefined),
    );
    render(<InlineNetworkAccessCard workspaceId="ws-1" />);
    await userEvent.click(await screen.findByRole('button', { name: 'Allow this run' }));
    await waitFor(() => expect(screen.queryByRole('article')).not.toBeInTheDocument());
  });

  it('shows the pending requests of its workspace', async () => {
    mockInvoke.mockImplementation((cmd: string) =>
      Promise.resolve(
        cmd === 'list_pending_network_access_requests'
          ? [request(), request({ requestId: 'net-2', host: 'cdn.example.com' })]
          : undefined,
      ),
    );
    render(<InlineNetworkAccessCard workspaceId="ws-1" />);
    expect(await screen.findAllByRole('article', { name: 'Network access requested' })).toHaveLength(2);
    expect(mockInvoke).toHaveBeenCalledWith('list_pending_network_access_requests', { workspaceId: 'ws-1' });
  });
});

describe('NetworkAccessCard for a port on this computer', () => {
  const ssh = {
    kind: 'program',
    pid: 4321,
    exe: '/usr/bin/ssh',
    commandLine: 'ssh -N -L 5432:db.internal:5432 bastion',
    interpreter: false,
  } as const;
  const postgres = { kind: 'otherUser', uid: 120, userName: 'postgres' } as const;
  const holder = (pid: number, exe: string) => ({ pid, exe, commandLine: `${exe} --serve` });
  const localRequest = (localListener: LocalListenerView, overrides: Partial<NetworkAccessRequest> = {}) =>
    request({ host: 'localhost', port: 5432, suggestedWildcard: null, runHosts: [], localListener, ...overrides });
  const buttons = () => screen.getAllByRole('button').map((button) => button.textContent);
  const always = () => screen.getByRole('button', { name: 'Always for Rust Architect' });

  it('shows the program, its pid and its command line, and where Always saves the port', () => {
    render(<NetworkAccessCard req={localRequest(ssh)} requester={requester} />);
    expect(screen.getByText('localhost:5432').tagName).toBe('CODE');
    expect(screen.getByText('not in allowed local ports')).toBeInTheDocument();
    expect(screen.getByText(/^Listening:/)).toHaveTextContent('Listening: /usr/bin/ssh (pid 4321)');
    const command = screen.getByText(ssh.commandLine);
    expect(command.tagName).toBe('CODE');
    expect(command).not.toHaveAttribute('data-collapsed');
    expect(screen.queryByRole('button', { name: 'Show all' })).not.toBeInTheDocument();
    expect(screen.queryByText(/runs many programs/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^Started/)).not.toBeInTheDocument();
    expect(screen.getByText(/adds port/)).toHaveTextContent(
      /^“Always” adds port 5432 for \/usr\/bin\/ssh to this agent.s allowed local ports in this workspace only\.$/,
    );
  });

  it('names the port on this computer when collapsed', async () => {
    const user = userEvent.setup();
    render(<NetworkAccessCard req={localRequest({ ...ssh, startedBy: { kind: 'terminal' } })} requester={requester} />);
    await user.click(screen.getByRole('button', { name: 'Collapse request' }));
    expect(screen.getByText('port 5432 on this computer')).toBeInTheDocument();
    expect(screen.queryByText(/localhost/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^Started/)).not.toBeInTheDocument();
  });

  it.each([
    [{ kind: 'terminal' }, 'Started in a CLAI terminal.'],
    [{ kind: 'app', name: 'code' }, 'Started by code, opened from CLAI.'],
    [{ kind: 'agent', name: 'Rust Architect' }, 'Started by a command from agent Rust Architect.'],
    [{ kind: 'agent' }, 'Started by an agent command.'],
    [{ kind: 'mcpServer', name: 'netdata' }, 'Started by MCP server netdata.'],
  ] as const)('says who in CLAI started the program, without changing the choices (%o)', (startedBy, line) => {
    render(<NetworkAccessCard req={localRequest({ ...ssh, startedBy })} requester={requester} />);
    expect(screen.getByText((_, el) => el?.tagName === 'P' && el.textContent === line)).toBeInTheDocument();
    expect(buttons()).toEqual(['▾', 'Allow this run', 'Always for Rust Architect', 'Deny']);
  });

  it('says who in CLAI started a run-only listener', () => {
    const startedBy = { kind: 'mcpServer', name: 'netdata' } as const;
    render(
      <NetworkAccessCard
        req={localRequest({ kind: 'runOnly', reason: 'unseen', holders: [], startedBy })}
        requester={requester}
      />,
    );
    expect(screen.getByText('Started by MCP server netdata.').tagName).toBe('P');
    expect(buttons()).toEqual(['▾', 'Allow this run', 'Deny']);
  });

  it('shows the app that started the program as code, and markup in its name as text', () => {
    const name = '<img src=x onerror=alert(1)>';
    render(<NetworkAccessCard req={localRequest({ ...ssh, startedBy: { kind: 'app', name } })} />);
    const app = screen.getByText(name);
    expect(app.tagName).toBe('CODE');
    expect(app.parentElement).toHaveTextContent(`Started by ${name}, opened from CLAI.`);
    expect(app.parentElement!.querySelector('img')).toBeNull();
  });

  it('renders markup in a command line as text', () => {
    const commandLine = 'node <img src=x onerror=alert(1)> <b>bold</b>';
    render(<NetworkAccessCard req={localRequest({ ...ssh, commandLine })} />);
    const command = screen.getByText(commandLine);
    expect(command.tagName).toBe('CODE');
    expect(command.querySelector('img, b')).toBeNull();
  });

  it('collapses a long command line to three lines until Show all', async () => {
    const user = userEvent.setup();
    const commandLine = `python3 -m http.server ${'--flag '.repeat(60)}`.trim();
    render(<NetworkAccessCard req={localRequest({ ...ssh, commandLine })} />);
    const command = screen.getByText(commandLine);
    const toggle = screen.getByRole('button', { name: 'Show all' });
    expect(command).toHaveAttribute('data-collapsed');
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveAttribute('aria-controls', command.id);
    await user.click(toggle);
    expect(command).not.toHaveAttribute('data-collapsed');
    expect(screen.getByRole('button', { name: 'Show less' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('names each toggle by its pid when several programs are listed', async () => {
    const user = userEvent.setup();
    const long = (pid: number, exe: string) => ({ pid, exe, commandLine: `${exe} ${'--flag '.repeat(60)}`.trim() });
    const holders = [long(1, '/usr/bin/a'), long(2, '/usr/bin/b')];
    render(<NetworkAccessCard req={localRequest({ kind: 'runOnly', reason: 'severalPrograms', holders })} />);
    const first = screen.getByRole('button', { name: 'Show all of the command line of pid 1' });
    expect(screen.getByRole('button', { name: 'Show all of the command line of pid 2' })).toBeInTheDocument();
    await user.click(first);
    expect(screen.getByRole('button', { name: 'Show less of the command line of pid 1' })).toHaveAttribute(
      'aria-expanded',
      'true',
    );
  });

  it('collapses a short command line of more than three lines', () => {
    const { container } = render(<NetworkAccessCard req={localRequest({ ...ssh, commandLine: 'a\nb\nc\nd' })} />);
    expect(container.querySelector('code[data-collapsed]')).toHaveTextContent(/^a\s*b\s*c\s*d$/);
    expect(screen.getByRole('button', { name: 'Show all' })).toBeInTheDocument();
  });

  it('says an interpreter allows whatever it runs', () => {
    render(<NetworkAccessCard req={localRequest({ ...ssh, exe: '/usr/bin/python3.12', interpreter: true })} />);
    expect(screen.getByText(/runs many programs/)).toHaveTextContent(
      '/usr/bin/python3.12 runs many programs, so allowing it allows any of them on port 5432.',
    );
  });

  it('sends Always as the port, never the program, and offers no wildcard', async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(<NetworkAccessCard req={localRequest(ssh)} requester={requester} />);
    expect(buttons()).toEqual(['▾', 'Allow this run', 'Always for Rust Architect', 'Deny']);
    expect(screen.getByRole('button', { name: 'Allow this run' })).toHaveAttribute(
      'title',
      'Allows /usr/bin/ssh on port 5432 until this run ends.',
    );
    expect(always()).toHaveAttribute('title', "Adds port 5432, served by /usr/bin/ssh, to this agent's allowed local ports.");
    await user.click(always());
    expect(submitted()).toEqual([{ requestId: 'net-1', decision: { kind: 'allowAlways', entry: 'localhost:5432' } }]);
  });

  it("names another user's service for both choices, and saves the port for that user", async () => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(<NetworkAccessCard req={localRequest(postgres)} requester={requester} workspaceName="clai" />);
    expect(screen.getByText(/^Listening:/)).toHaveTextContent(
      'Listening: a program of user postgres (uid 120). CLAI cannot see which program, because it runs as another user.',
    );
    expect(screen.getByText(/^Any program/)).toHaveTextContent(
      'Any program of user postgres on port 5432 will be allowed, for this run or always.',
    );
    expect(screen.getByRole('button', { name: 'Allow this run' })).toHaveAttribute(
      'title',
      'Allows any program of user postgres on port 5432 until this run ends.',
    );
    expect(always()).toHaveAttribute(
      'title',
      "Adds port 5432, served by user postgres (uid 120), to this agent's allowed local ports.",
    );
    expect(screen.getByText(/adds port/)).toHaveTextContent(
      /^“Always” adds port 5432 for user postgres to this agent.s allowed local ports in .clai. only\.$/,
    );
    await user.click(always());
    expect(submitted()).toEqual([{ requestId: 'net-1', decision: { kind: 'allowAlways', entry: 'localhost:5432' } }]);
  });

  it.each([
    ['unseen', [], 'CLAI cannot see every program listening on port 5432.'],
    [
      'otherNamespace',
      [holder(77, '/usr/bin/conmon')],
      'The program on port 5432 runs in a container or another sandbox, so CLAI cannot name it.',
    ],
    ['severalPrograms', [holder(1, '/usr/bin/a'), holder(2, '/usr/bin/b')], 'Different programs listen on port 5432:'],
    ['replaced', [holder(3, '/opt/app/server')], 'The program on port 5432 was replaced or deleted on disk since it started.'],
    ['pathNotUtf8', [holder(4, '/opt/\ufffd/server')], 'The path of the program on port 5432 is not valid UTF-8, so CLAI cannot save it.'],
  ] as const)('offers this run only when CLAI cannot name the program (%s)', async (reason, holders, why) => {
    mockInvoke.mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(
      <NetworkAccessCard req={localRequest({ kind: 'runOnly', reason, holders: [...holders] })} requester={requester} />,
    );
    expect(buttons()).toEqual(['▾', 'Allow this run', 'Deny']);
    expect(screen.getByText(why)).toBeInTheDocument();
    expect(screen.queryAllByText(/^Listening:/)).toHaveLength(holders.length);
    for (const { exe, commandLine } of holders) {
      expect(screen.getByText(exe)).toBeInTheDocument();
      expect(screen.getByText(commandLine)).toBeInTheDocument();
    }
    expect(screen.getByText(/this run only/)).toHaveTextContent(
      /^CLAI cannot save a rule for this listener, so the port can be allowed for this run only, until what listens there changes\.$/,
    );
    const allowRun = screen.getByRole('button', { name: 'Allow this run' });
    expect(allowRun).toHaveAttribute('title', 'Allows what listens on port 5432 now, until this run ends.');
    await user.click(allowRun);
    expect(submitted()).toEqual([{ requestId: 'net-1', decision: { kind: 'allowRun' } }]);
  });

  it('says there is no agent to save on, for a run-only listener too', () => {
    render(<NetworkAccessCard req={localRequest({ kind: 'runOnly', reason: 'unseen', holders: [] }, { agentId: null })} />);
    expect(buttons()).toEqual(['▾', 'Allow this run', 'Deny']);
    expect(screen.getByText(/this run only/)).toHaveTextContent(
      /^There is no agent to save an entry on, so the port can be allowed for this run only\.$/,
    );
  });

  it('says the port can be allowed for this run only when there is no agent to save on', () => {
    render(<NetworkAccessCard req={localRequest(ssh, { agentId: null })} requester={requester} />);
    expect(buttons()).toEqual(['▾', 'Allow this run', 'Deny']);
    expect(screen.getByText(/no agent to save/)).toHaveTextContent(
      /^There is no agent to save an entry on, so the port can be allowed for this run only\.$/,
    );
  });

  it.each([
    [ssh, 'It is served by /usr/bin/ssh.', ['This run', 'Always', 'Deny']],
    [postgres, 'It is served by user postgres.', ['This run', 'Always', 'Deny']],
    [{ ...ssh, startedBy: { kind: 'terminal' } }, 'It is served by /usr/bin/ssh. Started in a CLAI terminal.', ['This run', 'Always', 'Deny']],
    [
      { kind: 'runOnly', reason: 'unseen', holders: [], startedBy: { kind: 'agent', name: 'Rust Architect' } },
      'Started by a command from agent Rust Architect.',
      ['This run', 'Deny'],
    ],
  ] as const)('speaks on the Fleet with the listener and short chips (%o)', (listener, line, chips) => {
    render(
      <NetworkAccessCard
        req={localRequest(listener as LocalListenerView)}
        speaker={(prompt: ReactNode) => <div>{prompt}</div>}
      />,
    );
    // The question and the listener are separate sentences of one bubble.
    const spoken = `May I reach port 5432 on this computer? ${line}`;
    const bubble = screen.getByText('May I reach port 5432 on this computer?').textContent!;
    expect(bubble.slice(0, spoken.length)).toBe(spoken);
    expect(screen.getByText((_, el) => el?.tagName === 'SPAN' && el.textContent === line)).toBeInTheDocument();
    expect(buttons()).toEqual(chips);
    expect(screen.queryByText(/^Listening:/)).not.toBeInTheDocument();
  });

  it('says this run only once on the Fleet for a run-only listener', () => {
    render(
      <NetworkAccessCard
        req={localRequest({ kind: 'runOnly', reason: 'unseen', holders: [] })}
        speaker={(prompt: ReactNode) => <div>{prompt}</div>}
      />,
    );
    expect(screen.getByText('May I reach port 5432 on this computer?')).toBeInTheDocument();
    expect(screen.queryByText(/cannot name/)).not.toBeInTheDocument();
    expect(screen.getByText(/this run only/)).toHaveTextContent(
      /^CLAI cannot save a rule for this listener, so the port can be allowed for this run only, until what listens there changes\.$/,
    );
  });

  it('titles the spoken This run chip with the user it allows', () => {
    render(<NetworkAccessCard req={localRequest(postgres)} speaker={(prompt: ReactNode) => <div>{prompt}</div>} />);
    expect(screen.getByRole('button', { name: 'This run' })).toHaveAttribute(
      'title',
      'Allow this run. Allows any program of user postgres on port 5432 until this run ends.',
    );
  });
});
