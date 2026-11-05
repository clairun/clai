import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';

const api = vi.hoisted(() => ({
  workspaceGetAgent: vi.fn(),
  workspaceCreateAgent: vi.fn(),
  workspaceUpdateAgent: vi.fn(),
  workspaceDeleteAgent: vi.fn(),
  getSandboxStatus: vi.fn(),
}));
vi.mock('../../api/client', () => api);

import { AgentBehaviorForm, type AgentDetail, type AgentFormDeps } from './AgentBehaviorForm';
import type { SectionHandle } from './sectionHandle';
import { parseHostEntry } from './AgentAccess';
import { resetSandboxStatusForTests } from '../../hooks/useSandboxStatus';
import type { FilesystemPathGrant, NetworkPolicy, ProviderConnection, SandboxStatus } from '../../generated/bindings';

const connection = (id: string, name: string, extra: Partial<ProviderConnection> = {}): ProviderConnection =>
  ({ id, name, enabled: true, modelId: '', ...extra } as unknown as ProviderConnection);

const DEPS: AgentFormDeps = {
  mcpServers: [{ id: 'srv-a', name: 'Alpha' }, { id: 'srv-b', name: 'Beta' }],
  skills: [
    { id: 'pdf', name: 'pdf', description: 'Read PDFs', sourceId: 'anthropic', sourceName: 'anthropic-skills' },
    { id: 'docx', name: 'docx', sourceId: 'anthropic', sourceName: 'anthropic-skills' },
    { id: 'review', name: 'code-review', description: 'Review a change', sourceId: 'clai', sourceName: 'clai-skills' },
  ],
  providerConnections: [
    connection('conn-a', 'Anthropic', { modelId: 'sonnet' }),
    connection('conn-b', 'OpenAI'),
    connection('conn-off', 'Old', { enabled: false }),
  ],
  defaultExecution: null,
};

const HOME_GRANT: FilesystemPathGrant = { path: '/home/u', access: 'read_only', origin: { kind: 'credentialsPreset' } };

// A stored policy the form must save back unchanged.
const NETWORK: NetworkPolicy = { mode: 'host_network', allowDomains: ['github.com', '*.example.org'], denyDomains: ['bad.example.org'] };

const LINUX: SandboxStatus = { available: true, backend: 'bubblewrap', allowlistAvailable: true, allowlistUnavailableReason: null };
const MACOS: SandboxStatus = {
  available: true, backend: 'seatbelt', allowlistAvailable: false, allowlistUnavailableReason: 'Not available on macOS yet',
};
const NO_SANDBOX: SandboxStatus = {
  available: false, backend: null, allowlistAvailable: false, allowlistUnavailableReason: 'Not available on this PC',
};

const REVIEWER: AgentDetail = {
  id: 'agent-1',
  name: 'Reviewer',
  description: 'Reviews diffs',
  isDefault: false,
  enabled: true,
  selectedSkillIds: ['review'],
  selectedMcpServerIds: ['srv-a'],
  providerConnectionIds: ['conn-a'],
  execution: {
    sandbox: { networkAccess: NETWORK, sessionBus: 'allow' },
    filesystem: { extraPaths: [HOME_GRANT] },
    shell: {
      mode: 'restricted',
      askFirstCommandPrefixes: ['git push'],
    },
    web: { enabled: true },
  },
};

const renderForm = (props: Partial<React.ComponentProps<typeof AgentBehaviorForm>> = {}) => {
  const ref = createRef<SectionHandle>();
  const onDirtyChange = vi.fn();
  const utils = render(
    <AgentBehaviorForm
      ref={ref}
      workspaceId="ws-1"
      agentId={null}
      deps={DEPS}
      saving={false}
      onDirtyChange={onDirtyChange}
      {...props}
    />
  );
  return { ...utils, ref, onDirtyChange };
};

const submit = async (ref: React.RefObject<SectionHandle | null>) => {
  let result: { ok: boolean; error?: string; createdId?: string } | undefined;
  await act(async () => { result = await ref.current!.submit(); });
  return result;
};

const sentUpdate = () => api.workspaceUpdateAgent.mock.calls.at(-1)![0] as {
  selectedSkillIds: string[];
  selectedMcpServerIds: string[];
  providerConnectionIds: string[];
  execution: {
    filesystem: { extraPaths: FilesystemPathGrant[] };
    shell: { mode: string; askFirstCommandPrefixes: string[] };
    web: { enabled: boolean };
    sandbox: { sessionBus: string; networkAccess: NetworkPolicy };
  };
};

const model = () => screen.getByRole('combobox', { name: /Runs on/ });

describe('AgentBehaviorForm', () => {
  afterEach(() => { vi.restoreAllMocks(); });

  beforeEach(() => {
    api.workspaceGetAgent.mockReset().mockResolvedValue(REVIEWER);
    api.workspaceCreateAgent.mockReset().mockResolvedValue({ id: 'agent-new' });
    api.workspaceUpdateAgent.mockReset().mockResolvedValue({});
    api.workspaceDeleteAgent.mockReset();
    api.getSandboxStatus.mockReset().mockResolvedValue(LINUX);
    resetSandboxStatusForTests();
  });

  it('loads an agent, tracks dirtiness both ways and saves the full normalized payload', async () => {
    const { ref, onDirtyChange } = renderForm({ agentId: 'agent-1' });

    expect(await screen.findByDisplayValue('Reviewer')).toHaveAccessibleName(/^Name/);
    expect(api.workspaceGetAgent).toHaveBeenCalledWith('ws-1', 'agent-1');
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);

    const instructions = screen.getByLabelText('Instructions');
    await userEvent.type(instructions, ' carefully  ');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    await userEvent.clear(instructions);
    await userEvent.type(instructions, 'Reviews diffs');
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
    await userEvent.type(instructions, ' carefully  ');

    expect(await submit(ref)).toEqual({ ok: true });
    expect(api.workspaceUpdateAgent).toHaveBeenCalledWith({
      workspaceId: 'ws-1',
      agentId: 'agent-1',
      name: 'Reviewer',
      description: 'Reviews diffs carefully',
      selectedSkillIds: ['review'],
      selectedMcpServerIds: ['srv-a'],
      providerConnectionIds: ['conn-a'],
      execution: {
        sandbox: { networkAccess: NETWORK, sessionBus: 'allow' },
        filesystem: { extraPaths: [HOME_GRANT] },
        shell: {
          mode: 'restricted',
          askFirstCommandPrefixes: ['git push'],
        },
        web: { enabled: true },
      },
      enabled: true,
    });
    // The saved values are the new baseline, without the host re-rendering.
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
  });

  it('create flow waits for the backend defaults, validates, then sends the pre-populated grant', async () => {
    const { ref, rerender, onDirtyChange } = renderForm({ deps: { ...DEPS, defaultExecution: undefined } });
    expect(screen.getByLabelText('Loading agent')).toBeInTheDocument();

    rerender(
      <AgentBehaviorForm
        ref={ref}
        workspaceId="ws-1"
        agentId={null}
        deps={{ ...DEPS, defaultExecution: { filesystem: { extraPaths: [HOME_GRANT] } } }}
        saving={false}
        onDirtyChange={onDirtyChange}
      />
    );
    expect(await screen.findByText('Shell: Off')).toBeInTheDocument();
    expect(screen.getByText('1 path')).toBeInTheDocument();

    let verdict: { ok: boolean; error?: string } | undefined;
    act(() => { verdict = ref.current!.validate(); });
    expect(verdict).toEqual({ ok: false, error: 'Agent name is required.' });
    await userEvent.type(screen.getByLabelText(/^Name/), 'Scout');
    act(() => { verdict = ref.current!.validate(); });
    expect(verdict).toEqual({ ok: false, error: 'Choose a model for this agent to run on.' });
    expect(screen.getByRole('alert')).toHaveTextContent('Choose a model for this agent to run on.');
    // Nothing is chosen for the user: the select says so.
    expect(model()).toHaveValue('');

    await userEvent.selectOptions(model(), 'conn-b');

    const result = await submit(ref);
    expect(api.workspaceCreateAgent).toHaveBeenCalledTimes(1);
    expect(result).toEqual({ ok: true, createdId: 'agent-new' });
    const request = api.workspaceCreateAgent.mock.calls[0]![0] as {
      name: string;
      providerConnectionIds: string[];
      execution: {
        filesystem: { extraPaths: unknown[] };
        shell: { mode: string; askFirstCommandPrefixes: string[] };
        sandbox: object;
      };
    };
    expect(request.name).toBe('Scout');
    // The defaults carried no network policy, so the backend picks this PC's.
    expect(request.execution.sandbox).toEqual({ sessionBus: 'allow' });
    expect(request.providerConnectionIds).toEqual(['conn-b']);
    expect(request.execution.filesystem.extraPaths).toEqual([HOME_GRANT]);
    expect(request.execution.shell.mode).toBe('off');
    expect(request.execution.shell.askFirstCommandPrefixes).toHaveLength(25);
    expect(request.execution.shell.askFirstCommandPrefixes.slice(0, 2)).toEqual(['git push', 'gh pr merge']);
    expect(request.execution.shell.askFirstCommandPrefixes.slice(15)).toEqual([
      'rm', 'sudo', 'chmod', 'chown', 'dd', 'mkfs', 'mount', 'umount', 'shutdown', 'reboot',
    ]);
    expect(api.workspaceUpdateAgent).not.toHaveBeenCalled();
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
  });

  describe('model', () => {
    it('is one choice: the current connection shows, and picking another saves only that one', async () => {
      api.workspaceGetAgent.mockResolvedValue({ ...REVIEWER, providerConnectionIds: ['conn-a', 'conn-b'] });
      const { ref, onDirtyChange } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      expect(model()).toHaveDisplayValue('Anthropic — sonnet');
      // Disabled connections are not offered.
      expect(within(model()).queryByRole('option', { name: 'Old' })).toBeNull();
      // A legacy list is left alone until the user picks.
      expect(screen.getByText(/1 more saved connection is never used/)).toBeInTheDocument();
      expect(onDirtyChange).toHaveBeenLastCalledWith(false);

      await userEvent.selectOptions(model(), 'conn-b');
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);
      await submit(ref);
      expect(sentUpdate().providerConnectionIds).toEqual(['conn-b']);
    });

    it('keeps a legacy connection list when saving any other edit', async () => {
      api.workspaceGetAgent.mockResolvedValue({ ...REVIEWER, providerConnectionIds: ['conn-a', 'conn-b'] });
      const { ref } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      await userEvent.type(screen.getByLabelText('Instructions'), ' carefully');
      await submit(ref);
      expect(sentUpdate().providerConnectionIds).toEqual(['conn-a', 'conn-b']);
    });

    it('shows no warnings while the connections are still loading', async () => {
      const { rerender, ref, onDirtyChange } = renderForm({ agentId: 'agent-1', deps: { ...DEPS, providerConnections: [], defaultExecution: undefined } });
      await waitFor(() => expect(api.workspaceGetAgent).toHaveBeenCalled());
      expect(screen.getByLabelText('Loading agent')).toBeInTheDocument();
      expect(screen.queryByText(/gone or turned off/)).toBeNull();

      rerender(<AgentBehaviorForm ref={ref} workspaceId="ws-1" agentId="agent-1" deps={DEPS} saving={false} onDirtyChange={onDirtyChange} />);
      expect(await screen.findByDisplayValue('Reviewer')).toBeInTheDocument();
      expect(model()).toHaveDisplayValue('Anthropic — sonnet');
    });

    it('says when the saved connection is gone or turned off', async () => {
      api.workspaceGetAgent.mockResolvedValue({ ...REVIEWER, providerConnectionIds: ['conn-off'] });
      renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      expect(model()).toHaveDisplayValue('Old (disabled)');
      expect(screen.getByText(/gone or turned off/)).toBeInTheDocument();
    });

    it('links to the provider settings when there is no connection at all', async () => {
      renderForm({ agentId: 'agent-1', deps: { ...DEPS, providerConnections: [] }, initialAgent: { ...REVIEWER, providerConnectionIds: [] } });
      await screen.findByDisplayValue('Reviewer');

      expect(screen.queryByRole('combobox', { name: /Runs on/ })).toBeNull();
      expect(screen.getByRole('button', { name: 'Add one in Settings → Providers' })).toBeInTheDocument();
    });
  });

  describe('MCP servers', () => {
    it('names what is on in the trigger and toggles servers from the dropdown', async () => {
      const { ref } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      const trigger = screen.getByRole('button', { name: 'MCP servers Alpha' });
      await userEvent.click(trigger);
      const popover = screen.getByRole('group', { name: 'MCP servers' });
      expect(within(popover).getByRole('checkbox', { name: 'Alpha' })).toBeChecked();
      expect(within(popover).getByRole('checkbox', { name: 'Alpha' })).toHaveFocus();
      await userEvent.click(within(popover).getByRole('checkbox', { name: 'Beta' }));
      expect(screen.getByRole('button', { name: 'MCP servers Alpha, Beta' })).toBeInTheDocument();

      await submit(ref);
      expect(sentUpdate().selectedMcpServerIds).toEqual(['srv-a', 'srv-b']);
    });
  });

  describe('skills', () => {
    it('lists sources first with their counts, and opens one to show only its skills', async () => {
      const { ref, onDirtyChange } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      expect(screen.getByText('1 of 3 selected')).toBeInTheDocument();
      const sources = screen.getByRole('list', { name: 'Skill sources' });
      expect(within(sources).getAllByRole('button').map((b) => b.getAttribute('aria-label'))).toEqual([
        'anthropic-skills, 0 of 2 selected',
        'clai-skills, 1 of 1 selected',
      ]);

      await userEvent.click(within(sources).getByRole('button', { name: /^anthropic-skills/ }));
      expect(screen.getByRole('heading', { name: 'anthropic-skills' })).toHaveFocus();
      const grid = screen.getByRole('list', { name: 'Skills in anthropic-skills' });
      expect(within(grid).getAllByRole('checkbox').map((c) => c.getAttribute('aria-label'))).toEqual(['docx', 'pdf']);
      expect(screen.queryByRole('checkbox', { name: 'code-review' })).toBeNull();

      await userEvent.click(within(grid).getByRole('checkbox', { name: 'pdf' }));
      expect(screen.getByText('1 of 2 selected')).toBeInTheDocument();
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);
      await userEvent.click(within(grid).getByRole('checkbox', { name: 'pdf' }));
      expect(onDirtyChange).toHaveBeenLastCalledWith(false);
      await userEvent.click(within(grid).getByRole('checkbox', { name: 'pdf' }));

      await userEvent.click(screen.getByRole('button', { name: 'Back to Reviewer' }));
      // Back on the overview, on the row that was opened.
      expect(screen.getByRole('button', { name: 'anthropic-skills, 1 of 2 selected' })).toHaveFocus();

      await submit(ref);
      expect(sentUpdate().selectedSkillIds).toEqual(['review', 'pdf']);
    });

    it('leaves a drill-in view through the handle, so the window can map Escape to it', async () => {
      const { ref } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');
      expect(ref.current!.back!()).toBe(false);

      await userEvent.click(screen.getByRole('button', { name: /^clai-skills/ }));
      let went = false;
      act(() => { went = ref.current!.back!(); });
      expect(went).toBe(true);
      expect(screen.getByRole('list', { name: 'Skill sources' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /^clai-skills/ })).toHaveFocus();
    });

    it('selects and clears a whole source, leaving other sources alone', async () => {
      const { ref } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');
      await userEvent.click(screen.getByRole('button', { name: /^anthropic-skills/ }));

      await userEvent.click(screen.getByRole('button', { name: 'Select all' }));
      expect(screen.getByText('2 of 2 selected')).toBeInTheDocument();
      await submit(ref);
      expect(sentUpdate().selectedSkillIds).toEqual(['review', 'docx', 'pdf']);

      await userEvent.click(screen.getByRole('button', { name: 'Clear all' }));
      await submit(ref);
      expect(sentUpdate().selectedSkillIds).toEqual(['review']);
    });

    it('Clear all on the overview keeps skill ids the library no longer lists', async () => {
      api.workspaceGetAgent.mockResolvedValue({ ...REVIEWER, selectedSkillIds: ['review', 'gone-id'] });
      const { ref } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      await userEvent.click(screen.getByRole('button', { name: 'Clear all' }));
      await submit(ref);
      expect(sentUpdate().selectedSkillIds).toEqual(['gone-id']);
    });

    it('selects only what the filter shows', async () => {
      const { ref } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');
      await userEvent.click(screen.getByRole('button', { name: /^anthropic-skills/ }));

      await userEvent.type(screen.getByRole('searchbox', { name: 'Filter anthropic-skills' }), 'pdf');
      await userEvent.click(screen.getByRole('button', { name: 'Select all' }));
      expect(screen.getByText('1 of 2 selected')).toBeInTheDocument();
      await submit(ref);
      expect(sentUpdate().selectedSkillIds).toEqual(['review', 'pdf']);
    });

    it('finds a skill across sources from the overview', async () => {
      renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');

      await userEvent.type(screen.getByRole('searchbox', { name: 'Search skills' }), 'review');
      const results = screen.getByRole('list', { name: 'Matching skills' });
      expect(within(results).getByRole('checkbox', { name: 'code-review' })).toBeChecked();
      expect(within(results).getByText('clai-skills')).toBeInTheDocument();
      expect(within(results).queryByRole('checkbox', { name: 'pdf' })).toBeNull();

      await userEvent.clear(screen.getByRole('searchbox', { name: 'Search skills' }));
      await userEvent.type(screen.getByRole('searchbox', { name: 'Search skills' }), 'zzz');
      expect(screen.getByRole('status')).toHaveTextContent('No skills match “zzz”.');
    });

    it('points at the skill sources when there are no skills', async () => {
      renderForm({ agentId: 'agent-1', deps: { ...DEPS, skills: [] } });
      await screen.findByDisplayValue('Reviewer');

      expect(screen.getByRole('button', { name: 'Add a skill source' })).toBeInTheDocument();
    });
  });

  describe('local access', () => {
    it('summarizes on the overview and edits in its own view', async () => {
      const { ref, onDirtyChange } = renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');
      expect(screen.getByText('Shell: Ask first')).toBeInTheDocument();
      expect(screen.getByText('Web on')).toBeInTheDocument();
      expect(screen.getByText('Network: Host network')).toBeInTheDocument();

      await userEvent.click(screen.getByRole('button', { name: 'Edit local access' }));
      expect(screen.getByRole('heading', { name: 'Local access' })).toHaveFocus();
      expect(screen.getByText('/home/u')).toBeInTheDocument();
      expect(screen.getByText('Preset')).toBeInTheDocument();
      expect(screen.getByRole('list', { name: 'Ask-first command prefixes' })).toHaveTextContent('git push');

      await userEvent.click(screen.getByRole('switch', { name: 'Web access' }));
      expect(onDirtyChange).toHaveBeenLastCalledWith(true);
      await userEvent.click(screen.getByRole('switch', { name: 'Web access' }));
      expect(onDirtyChange).toHaveBeenLastCalledWith(false);

      await userEvent.type(screen.getByRole('textbox', { name: 'New path for path grants' }), '/srv/data');
      await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Access for the new path' }), 'read_write');
      await userEvent.click(screen.getAllByRole('button', { name: 'Add' })[0]!);
      await userEvent.click(screen.getByRole('button', { name: "Don't ask" }));
      // Full reads no list, so none is shown.
      expect(screen.queryByRole('list', { name: 'Ask-first command prefixes' })).toBeNull();
      await userEvent.click(screen.getByRole('switch', { name: 'Web access' }));
      await userEvent.click(screen.getByRole('switch', { name: 'Session D-Bus' }));

      await userEvent.click(screen.getByRole('button', { name: 'Back to Reviewer' }));
      expect(screen.getByRole('button', { name: 'Edit local access' })).toHaveFocus();
      expect(screen.getByText("Shell: Don't ask")).toBeInTheDocument();

      await submit(ref);
      const { execution } = sentUpdate();
      expect(execution.filesystem.extraPaths).toEqual([HOME_GRANT, { path: '/srv/data', access: 'read_write', origin: null }]);
      expect(execution.shell).toEqual({
        mode: 'full',
        askFirstCommandPrefixes: ['git push'],
      });
      expect(execution.web.enabled).toBe(false);
      expect(execution.sandbox.sessionBus).toBe('deny');
    });

    it('hides the ask-first list when the shell is off', async () => {
      renderForm({ agentId: 'agent-1' });
      await screen.findByDisplayValue('Reviewer');
      await userEvent.click(screen.getByRole('button', { name: 'Edit local access' }));

      const shell = screen.getByRole('group', { name: 'Shell' });
      await userEvent.click(within(shell).getByRole('button', { name: 'Off' }));

      expect(within(shell).getByRole('button', { name: 'Off' })).toHaveAttribute('aria-pressed', 'true');
      expect(screen.queryByRole('list', { name: 'Ask-first command prefixes' })).toBeNull();
    });
  });

  describe('network', () => {
    const openAccess = async () => {
      await screen.findByDisplayValue('Reviewer');
      await userEvent.click(screen.getByRole('button', { name: 'Edit local access' }));
    };
    const networkButton = (name: string | RegExp) =>
      within(screen.getByRole('group', { name: 'Network' })).getByRole('button', { name });
    const withPolicy = (networkAccess: NetworkPolicy): AgentDetail => ({
      ...REVIEWER,
      execution: { ...REVIEWER.execution, sandbox: { networkAccess, sessionBus: 'allow' } },
    });

    it('saves each mode and keeps the lists across modes', async () => {
      const { ref } = renderForm({ agentId: 'agent-1' });
      await openAccess();
      await waitFor(() => expect(networkButton('Allowlist')).toBeEnabled());
      expect(networkButton(/Host network \(unfiltered\)/)).toHaveAttribute('aria-pressed', 'true');
      expect(screen.getByText(/including services on this computer and your local network/)).toBeInTheDocument();

      for (const [label, mode] of [['Off', 'off'], ['Allowlist', 'allowlist'], [/Host network/, 'host_network']] as const) {
        await userEvent.click(networkButton(label));
        expect(networkButton(label)).toHaveAttribute('aria-pressed', 'true');
        await submit(ref);
        expect(sentUpdate().execution.sandbox.networkAccess).toEqual({ ...NETWORK, mode });
      }
    });

    it('edits the host lists in Allowlist and rejects what is not a host', async () => {
      api.workspaceGetAgent.mockResolvedValue(withPolicy({ ...NETWORK, mode: 'allowlist' }));
      const { ref } = renderForm({ agentId: 'agent-1' });
      expect(await screen.findByText('Network: Allowlist (2 hosts)')).toBeInTheDocument();
      await openAccess();

      const input = await screen.findByRole('textbox', { name: 'New entry for allowed hosts' });
      const add = within(input.parentElement!).getByRole('button', { name: 'Add' });
      for (const [text, reason] of [
        ['https://x.org', /without http/],
        ['x.org/docs', /without a path/],
        ['x.org:443', /port/],
        ['x .org', /no spaces/],
        ['a*.org', /whole label/],
      ] as const) {
        await userEvent.clear(input);
        await userEvent.type(input, text);
        await userEvent.click(add);
        expect(screen.getByRole('alert')).toHaveTextContent(reason);
      }
      expect(screen.getByRole('list', { name: 'Allowed hosts' }).querySelectorAll('li')).toHaveLength(2);

      await userEvent.clear(input);
      await userEvent.type(input, 'Docs.Example.ORG.');
      expect(screen.queryByRole('alert')).toBeNull();
      await userEvent.click(add);
      await userEvent.click(screen.getByRole('button', { name: 'Remove bad.example.org' }));

      await submit(ref);
      expect(sentUpdate().execution.sandbox.networkAccess).toEqual({
        mode: 'allowlist',
        allowDomains: ['github.com', '*.example.org', 'docs.example.org'],
        denyDomains: [],
      });
    });

    it('offers Allowlist only where the sandbox can enforce it, with the reason', async () => {
      api.getSandboxStatus.mockResolvedValue(MACOS);
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      await waitFor(() => expect(networkButton('Off')).toBeEnabled());
      expect(networkButton(/Host network/)).toBeEnabled();
      expect(networkButton('Allowlist')).toBeDisabled();
      expect(networkButton('Allowlist')).toHaveAttribute('title', 'Not available on macOS yet');
      expect(screen.getByText('Allowlist: Not available on macOS yet.')).toBeInTheDocument();
    });

    it('warns that a stored Allowlist stops the shell where it cannot be enforced', async () => {
      api.getSandboxStatus.mockResolvedValue(MACOS);
      api.workspaceGetAgent.mockResolvedValue(withPolicy({ ...NETWORK, mode: 'allowlist' }));
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      expect(await screen.findByText(/Shell commands will not run until you choose Off or Host network/)).toBeInTheDocument();
    });

    it('disables the whole control without a sandbox', async () => {
      api.getSandboxStatus.mockResolvedValue(NO_SANDBOX);
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      expect(await screen.findByText('Not available on this PC.')).toBeInTheDocument();
      for (const name of ['Off', 'Allowlist', /Host network/]) expect(networkButton(name)).toBeDisabled();
    });

    // The same cases as `entries_are_parsed_by_one_rule` in network/matcher.rs.
    it('parses host entries as the backend matches them', () => {
      const accepted: [string, string][] = [
        ['github.com', 'github.com'],
        [' GitHub.COM. ', 'github.com'],
        ['*.github.com', '*.github.com'],
        ['s3.*.amazonaws.com', 's3.*.amazonaws.com'],
        ['*.com', '*.com'],
        ['_dmarc.example.com', '_dmarc.example.com'],
        ['bücher.example', 'bücher.example'],
        ['10.0.0.5', '10.0.0.5'],
        ['[2001:db8::1]', '2001:db8::1'],
        ['2001:0db8:0:0::1', '2001:db8::1'],
      ];
      for (const [raw, entry] of accepted) {
        expect(parseHostEntry(raw), raw).toBe(entry);
      }
      for (const bad of [
        '', '*', '*.*', '*.*.*',
        'github.com:443', 'ab:443', 'abc:def:1', '[foo]', '[::1]:443',
        'git@github.com', 'github.com?x', 'github.com#x', 'a|b.com', 'a^b.com', 'a!b.com', '%2a.github.com',
        '999.1.1.1', 'example.123', 'example.0x1', '2130706433', '010.0.0.5', '*.0.0.5',
        'https://github.com', 'github.com/x', 'git hub.com', 'a..com', 'gi*thub.com',
      ]) {
        expect(parseHostEntry(bad), bad).toHaveProperty('error');
      }
    });
  });

  it('freezes every control while a delete is in flight, not only while the host saves', async () => {
    let finishDelete: () => void = () => {};
    api.workspaceDeleteAgent.mockReturnValue(new Promise<void>((resolve) => { finishDelete = resolve; }));
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    renderForm({ agentId: 'agent-1' });
    await screen.findByDisplayValue('Reviewer');

    await userEvent.click(screen.getByRole('button', { name: 'Delete agent' }));
    expect(await screen.findByRole('button', { name: 'Deleting…' })).toBeDisabled();
    for (const control of [
      screen.getByLabelText(/^Name/),
      screen.getByLabelText('Instructions'),
      model(),
      screen.getByRole('button', { name: /^MCP servers/ }),
      screen.getByRole('switch', { name: 'Enabled' }),
      screen.getByRole('searchbox', { name: 'Search skills' }),
      screen.getByRole('button', { name: 'Clear all' }),
    ]) {
      expect(control).toBeDisabled();
    }

    finishDelete();
    await waitFor(() => expect(screen.getByRole('button', { name: 'Delete agent' })).toBeEnabled());
  });

  it('deletes only after the user confirms, then tells the host', async () => {
    const onDeleted = vi.fn();
    api.workspaceDeleteAgent.mockResolvedValue(undefined);
    const confirm = vi.spyOn(window, 'confirm');
    renderForm({ agentId: 'agent-1', onDeleted });
    await screen.findByDisplayValue('Reviewer');

    confirm.mockReturnValueOnce(false);
    await userEvent.click(screen.getByRole('button', { name: 'Delete agent' }));
    expect(api.workspaceDeleteAgent).not.toHaveBeenCalled();

    confirm.mockReturnValueOnce(true);
    await userEvent.click(screen.getByRole('button', { name: 'Delete agent' }));
    await waitFor(() => expect(onDeleted).toHaveBeenCalledTimes(1));
    expect(api.workspaceDeleteAgent).toHaveBeenCalledWith('ws-1', 'agent-1');
    expect(confirm).toHaveBeenCalledWith('Delete agent "Reviewer"? This cannot be undone.');
  });

  it('routes saves through saveBehavior when a host supplies one and hides Delete', async () => {
    const saveBehavior = vi.fn().mockResolvedValue(undefined);
    const { ref, onDirtyChange } = renderForm({ agentId: 'def-1', initialAgent: REVIEWER, saveBehavior });

    await screen.findByDisplayValue('Reviewer');
    expect(api.workspaceGetAgent).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: 'Delete agent' })).not.toBeInTheDocument();

    await userEvent.clear(screen.getByLabelText(/^Name/));
    await userEvent.type(screen.getByLabelText(/^Name/), 'Auditor');
    await submit(ref);

    expect(saveBehavior).toHaveBeenCalledWith(expect.objectContaining({ workspaceId: 'ws-1', name: 'Auditor', enabled: true }));
    expect(api.workspaceUpdateAgent).not.toHaveBeenCalled();
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
  });

  it('keeps the Main agent nameless, always enabled and undeletable', async () => {
    api.workspaceGetAgent.mockResolvedValue({ ...REVIEWER, id: 'main', name: 'Manager', isDefault: true, enabled: false });
    const { ref } = renderForm({ agentId: 'main' });

    await screen.findByDisplayValue('Reviews diffs');
    expect(screen.queryByLabelText(/^Name/)).not.toBeInTheDocument();
    expect(screen.queryByRole('switch', { name: 'Enabled' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Delete agent' })).not.toBeInTheDocument();

    await submit(ref);
    expect(api.workspaceUpdateAgent).toHaveBeenCalledWith(
      expect.objectContaining({ agentId: 'main', name: 'Manager', enabled: true })
    );
  });

  it('surfaces a backend save failure inline and reports it to the host', async () => {
    api.workspaceUpdateAgent.mockRejectedValue(new Error('agent name already taken'));
    const { ref } = renderForm({ agentId: 'agent-1' });
    await screen.findByDisplayValue('Reviewer');

    expect(await submit(ref)).toEqual({ ok: false, error: 'agent name already taken' });
    expect(screen.getByRole('alert')).toHaveTextContent('agent name already taken');
  });

  it('shows the load failure instead of a form the edit flow cannot save', async () => {
    api.workspaceGetAgent.mockRejectedValue(new Error('agent not found'));
    renderForm({ agentId: 'agent-1' });

    expect(await screen.findByRole('alert')).toHaveTextContent('agent not found');
    expect(screen.queryByLabelText(/^Name/)).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Instructions')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Delete agent' })).not.toBeInTheDocument();
  });

  it('says the agent is gone when the backend finds no such agent', async () => {
    api.workspaceGetAgent.mockResolvedValue(null);
    renderForm({ agentId: 'agent-1' });

    expect(await screen.findByRole('alert')).toHaveTextContent('Agent not found.');
    expect(screen.queryByLabelText(/^Name/)).not.toBeInTheDocument();
  });
});
