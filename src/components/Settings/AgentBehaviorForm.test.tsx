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
import hostEntryCases from './hostEntryCases.json';
import { resetSandboxStatusForTests } from '../../hooks/useSandboxStatus';
import { StaleAgentError } from '../../api/errors';
import formStyles from './SettingsForm.module.css';
import type {
  ExecutionSandboxConfig, FilesystemPathGrant, LocalPortGrant, NetworkPolicy, ProviderConnection, SandboxStatus,
} from '../../generated/bindings';

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

const LINUX: SandboxStatus = {
  available: true, backend: 'bubblewrap', allowlistAvailable: true, allowlistUnavailableReason: null,
  socketIsolationAvailable: true, socketIsolationUnavailableReason: null,
  sessionBusFilterAvailable: true, sessionBusFilterUnavailableReason: null,
};
const MACOS: SandboxStatus = {
  available: true, backend: 'seatbelt', allowlistAvailable: false, allowlistUnavailableReason: 'Not available on macOS yet',
  socketIsolationAvailable: false, socketIsolationUnavailableReason: 'Not available on macOS yet',
  sessionBusFilterAvailable: false, sessionBusFilterUnavailableReason: 'Not available on macOS yet',
};
const NO_SANDBOX: SandboxStatus = {
  available: false, backend: null, allowlistAvailable: false, allowlistUnavailableReason: 'Not available on this PC',
  socketIsolationAvailable: false, socketIsolationUnavailableReason: 'Not available on this PC',
  sessionBusFilterAvailable: false, sessionBusFilterUnavailableReason: 'Not available on this PC',
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
    sandbox: { networkAccess: NETWORK, sessionBusMode: 'full' },
    filesystem: { extraPaths: [HOME_GRANT] },
    shell: {
      mode: 'restricted',
      askFirstCommandPrefixes: ['git push'],
    },
    web: { enabled: true },
  },
  updatedAt: 5,
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
    sandbox: { sessionBusMode: string; networkAccess: NetworkPolicy };
  };
};

const model = () => screen.getByRole('combobox', { name: /Runs on/ });
const busGroup = () => screen.getByRole('group', { name: 'Session bus' });
const busButton = (name: string) => within(busGroup()).getByRole('button', { name });

// The Reviewer with another stored sandbox section, typed loosely so a
// section saved before the bus modes can be passed too.
const withSandbox = (sandbox: object): AgentDetail => ({
  ...REVIEWER,
  execution: { ...REVIEWER.execution, sandbox: sandbox as ExecutionSandboxConfig },
});

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
        sandbox: { networkAccess: NETWORK, sessionBusMode: 'full' },
        filesystem: { extraPaths: [HOME_GRANT] },
        shell: {
          mode: 'restricted',
          askFirstCommandPrefixes: ['git push'],
        },
        web: { enabled: true },
      },
      enabled: true,
      expectedUpdatedAt: 5,
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
    // The defaults carried no network policy, so the backend picks this PC's,
    // and no bus mode, so the agent gets no bus.
    expect(request.execution.sandbox).toEqual({ sessionBusMode: 'deny' });
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
      await userEvent.click(busButton('Off'));

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
      expect(execution.sandbox.sessionBusMode).toBe('deny');
    });

    describe('session bus', () => {
      const openAccess = async () => {
        await screen.findByDisplayValue('Reviewer');
        await userEvent.click(screen.getByRole('button', { name: 'Edit local access' }));
      };

      it('offers every bus mode with its hint and saves the one picked', async () => {
        const { ref } = renderForm({ agentId: 'agent-1' });
        await openAccess();
        expect(busButton('Full')).toHaveAttribute('aria-pressed', 'true');
        const modes = [
          ['Off', 'deny', /^No desktop services/],
          ['Keyring only', 'keyring_only', /^Only the system keyring/],
          ['Full', 'full', /^Every desktop service, unfiltered/],
        ] as const;
        for (const [label, mode, hint] of modes) {
          await userEvent.click(busButton(label));
          expect(busButton(label)).toHaveAttribute('aria-pressed', 'true');
          expect(busGroup()).toHaveAccessibleDescription(hint);
          await submit(ref);
          expect(sentUpdate().execution.sandbox.sessionBusMode).toBe(mode);
        }
        // Full reaches services that start programs outside the sandbox.
        expect(screen.getByText(/^Every desktop service/)).toHaveClass(formStyles.hintWarning!);
      });

      it('shows the backend reason as is where keyring only cannot be filtered, and still offers Full', async () => {
        const reason = 'Keyring-only agents get no session bus here: xdg-dbus-proxy is not installed. '
          + 'To give an agent the whole session bus, set its Session bus to Full in its Local access settings.';
        api.getSandboxStatus.mockResolvedValue({ ...LINUX, sessionBusFilterAvailable: false, sessionBusFilterUnavailableReason: reason });
        api.workspaceGetAgent.mockResolvedValue(
          withSandbox({ networkAccess: NETWORK, sessionBusMode: 'keyring_only' }),
        );
        const { ref } = renderForm({ agentId: 'agent-1' });
        expect(await screen.findByText('Bus off (keyring only unavailable here)')).toBeInTheDocument();
        await openAccess();

        const warning = await screen.findByText(reason);
        expect(warning).toHaveClass(formStyles.hintWarning!);
        expect(busGroup()).toHaveAccessibleDescription(expect.stringContaining(reason));
        expect(screen.queryByText(/gets the full session bus/)).toBeNull();

        await userEvent.click(busButton('Full'));
        expect(screen.queryByText(reason)).toBeNull();
        await submit(ref);
        expect(sentUpdate().execution.sandbox.sessionBusMode).toBe('full');
      });

      it('shows the bus only for the Linux sandbox, below Web access, and a save keeps it either way', async () => {
        for (const status of [MACOS, NO_SANDBOX]) {
          api.getSandboxStatus.mockResolvedValue(status);
          resetSandboxStatusForTests();
          const { ref, unmount } = renderForm({ agentId: 'agent-1' });
          await openAccess();
          await waitFor(() => expect(screen.queryByText(/Checking what this PC supports/)).toBeNull());
          expect(screen.queryByRole('group', { name: 'Session bus' })).toBeNull();
          expect(screen.getByRole('switch', { name: 'Web access' })).toBeInTheDocument();
          await submit(ref);
          expect(sentUpdate().execution.sandbox).toMatchObject({ sessionBusMode: 'full' });
          unmount();
        }

        api.getSandboxStatus.mockResolvedValue(LINUX);
        resetSandboxStatusForTests();
        renderForm({ agentId: 'agent-1' });
        await openAccess();
        const bus = await screen.findByRole('group', { name: 'Session bus' });
        // Web access sits above the bus, not pinned below it where it scrolls out of view.
        expect(screen.getByRole('switch', { name: 'Web access' }).compareDocumentPosition(bus))
          .toBe(Node.DOCUMENT_POSITION_FOLLOWING);
      });

      it('keeps the bus mode exactly across a save and a reload', async () => {
        const labels = { deny: 'Off', keyring_only: 'Keyring only', full: 'Full' } as const;
        for (const mode of ['deny', 'keyring_only', 'full'] as const) {
          const stored = withSandbox({ networkAccess: NETWORK, sessionBusMode: mode });
          api.workspaceGetAgent.mockResolvedValue(stored);
          const first = renderForm({ agentId: 'agent-1' });
          await screen.findByDisplayValue('Reviewer');
          await userEvent.type(screen.getByLabelText('Instructions'), ' again');
          await submit(first.ref);
          const saved = sentUpdate().execution.sandbox;
          expect(saved).toEqual({ networkAccess: NETWORK, sessionBusMode: mode });
          first.unmount();

          api.workspaceGetAgent.mockResolvedValue(withSandbox(saved));
          const second = renderForm({ agentId: 'agent-1' });
          await openAccess();
          expect(await within(await screen.findByRole('group', { name: 'Session bus' }))
            .findByRole('button', { name: labels[mode] })).toHaveAttribute('aria-pressed', 'true');
          expect(second.onDirtyChange).not.toHaveBeenCalledWith(true);
          second.unmount();
        }
      });

      it('reads an agent saved before the bus modes as the backend does, and saves that', async () => {
        // `allowedSockets` from an unreleased build is not sent back.
        const cases = [
          [{ networkAccess: NETWORK, sessionBus: 'allow', allowedSockets: ['/run/docker.sock'] }, 'keyring_only', 'Keyring only'],
          [{ networkAccess: NETWORK }, 'keyring_only', 'Keyring only'],
          [{ networkAccess: NETWORK, sessionBus: 'deny' }, 'deny', 'Off'],
        ] as const;
        for (const [sandbox, mode, label] of cases) {
          api.workspaceGetAgent.mockResolvedValue(withSandbox(sandbox));
          const { ref, unmount, onDirtyChange } = renderForm({ agentId: 'agent-1' });
          await openAccess();
          expect(await within(await screen.findByRole('group', { name: 'Session bus' }))
            .findByRole('button', { name: label })).toHaveAttribute('aria-pressed', 'true');
          expect(onDirtyChange).not.toHaveBeenCalledWith(true);
          await submit(ref);
          expect(sentUpdate().execution.sandbox).toEqual({ networkAccess: NETWORK, sessionBusMode: mode });
          unmount();
        }
      });
    });

    describe('socket grants', () => {
      const openAccess = async () => {
        await screen.findByDisplayValue('Reviewer');
        await userEvent.click(screen.getByRole('button', { name: 'Edit local access' }));
      };
      const warning = /^Granting a Docker, Podman or containerd socket gives this agent full control of this computer/;
      const grantItem = (path: string) => within(screen.getByRole('list', { name: 'Path grants' }))
        .getAllByRole('listitem').find((item) => item.textContent?.startsWith(path))!;
      const addPath = async (path: string) => {
        await userEvent.type(screen.getByRole('textbox', { name: 'New path for path grants' }), `${path}{Enter}`);
      };

      it('warns under a container engine socket in Paths and still saves it', async () => {
        const { ref } = renderForm({ agentId: 'agent-1' });
        await openAccess();
        const sockets = ['/var/run/docker.sock', '/run/user/1000/podman/podman.sock', '/run/containerd/containerd.sock'];
        for (const path of [...sockets, '/srv/data']) await addPath(path);

        for (const path of sockets) {
          expect(within(grantItem(path)).getByText(warning)).toHaveClass(formStyles.hintWarning!);
        }
        for (const path of ['/home/u', '/srv/data']) {
          expect(within(grantItem(path)).queryByText(warning)).toBeNull();
        }
        expect(screen.getAllByText(warning)).toHaveLength(3);

        expect(await submit(ref)).toEqual({ ok: true });
        expect(sentUpdate().execution.filesystem.extraPaths.map((grant) => grant.path))
          .toEqual(['/home/u', ...sockets, '/srv/data']);
      });

      it('says a socket needs its exact path, and when this kernel cannot isolate sockets', async () => {
        const help = /^A Unix socket, such as Docker’s, is reachable only when granted by its exact path/;
        const first = renderForm({ agentId: 'agent-1' });
        await openAccess();
        expect(await screen.findByText(help)).toBeInTheDocument();
        expect(screen.queryByText(/Socket isolation is unavailable/)).toBeNull();
        first.unmount();

        api.getSandboxStatus.mockResolvedValue({
          ...LINUX,
          socketIsolationAvailable: false,
          socketIsolationUnavailableReason: 'This Linux kernel cannot confine Unix sockets (it needs Landlock ABI 9)',
        });
        resetSandboxStatusForTests();
        const second = renderForm({ agentId: 'agent-1' });
        await openAccess();
        expect(await screen.findByText(
          'Socket isolation is unavailable here: This Linux kernel cannot confine Unix sockets (it needs Landlock ABI 9). '
            + 'A socket inside a granted folder can still be reachable.',
        )).toHaveClass(formStyles.hint!);
        second.unmount();

        // Only the Linux sandbox has the exact-path rule.
        api.getSandboxStatus.mockResolvedValue(MACOS);
        resetSandboxStatusForTests();
        renderForm({ agentId: 'agent-1' });
        await openAccess();
        await waitFor(() => expect(screen.queryByText(/Checking what this PC supports/)).toBeNull());
        expect(screen.queryByText(help)).toBeNull();
        expect(screen.queryByText(/Socket isolation is unavailable/)).toBeNull();
      });
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
      execution: { ...REVIEWER.execution, sandbox: { networkAccess, sessionBusMode: 'full' } },
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

      expect(await screen.findByText('Allowed hosts · 2')).toBeInTheDocument();
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

    it('shows saved names in Unicode and stores what is added in the backend form', async () => {
      api.workspaceGetAgent.mockResolvedValue(
        withPolicy({ mode: 'allowlist', allowDomains: ['xn--bcher-kva.example'], denyDomains: [] }),
      );
      const { ref } = renderForm({ agentId: 'agent-1' });
      await openAccess();
      const allowed = await screen.findByRole('list', { name: 'Allowed hosts' });
      expect(within(allowed).getByText('bücher.example')).toHaveAttribute('title', 'xn--bcher-kva.example');

      const input = screen.getByRole('textbox', { name: 'New entry for allowed hosts' });
      const add = within(input.parentElement!).getByRole('button', { name: 'Add' });
      await userEvent.type(input, 'Bücher.example');
      await userEvent.click(add);
      expect(screen.getByRole('alert')).toHaveTextContent('bücher.example is already in the list.');

      await userEvent.clear(input);
      await userEvent.type(input, 'münchen.example');
      await userEvent.click(add);
      expect(within(allowed).getByText('münchen.example')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Remove münchen.example' })).toBeInTheDocument();
      await submit(ref);
      expect(sentUpdate().execution.sandbox.networkAccess.allowDomains).toEqual([
        'xn--bcher-kva.example',
        'xn--mnchen-3ya.example',
      ]);
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

    it('does not call a stored Allowlist web tools only where it stops the shell', async () => {
      api.getSandboxStatus.mockResolvedValue(NO_SANDBOX);
      api.workspaceGetAgent.mockResolvedValue(withPolicy({ ...NETWORK, mode: 'allowlist' }));
      renderForm({ agentId: 'agent-1' });
      expect(await screen.findByText('Network: Allowlist (2 hosts)')).toBeInTheDocument();
      await openAccess();
      const reason = 'Allowlist: Not available on this PC. Shell commands will not run until you choose Off or Host network.';
      await waitFor(() => expect(screen.getByRole('group', { name: 'Network' })).toHaveAccessibleDescription(reason));
      expect(screen.queryByText(/shell commands are not restricted/)).toBeNull();
    });

    it('marks a stored host entry that is not valid', async () => {
      api.getSandboxStatus.mockResolvedValue(LINUX);
      api.workspaceGetAgent.mockResolvedValue(withPolicy({ ...NETWORK, mode: 'allowlist', denyDomains: ['x/y'] }));
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      const denied = await screen.findByRole('list', { name: 'Denied hosts' });
      expect(within(denied).getByText('Not valid: Enter the host only, without a path.')).toBeInTheDocument();
      const allowed = screen.getByRole('list', { name: 'Allowed hosts' });
      expect(within(allowed).queryByText(/Not valid/)).toBeNull();
    });

    describe('local ports', () => {
      const SSH_PORT: LocalPortGrant = { kind: 'program', port: 5432, program: '/usr/bin/ssh' };
      const POSTGRES_PORT: LocalPortGrant = { kind: 'user', port: 5433, uid: 120, user: 'postgres' };

      it('lists them in Allowlist only, and saves without one removed', async () => {
        api.getSandboxStatus.mockResolvedValue(LINUX);
        api.workspaceGetAgent.mockResolvedValue(
          withPolicy({ ...NETWORK, mode: 'allowlist', localPorts: [SSH_PORT, POSTGRES_PORT] }),
        );
        const { ref } = renderForm({ agentId: 'agent-1' });
        expect(await screen.findByText('Network: Allowlist (2 hosts, 2 local ports)')).toBeInTheDocument();
        await openAccess();
        const ports = await screen.findByRole('list', { name: 'Local ports' });
        expect(within(ports).getAllByRole('listitem')).toHaveLength(2);

        await userEvent.click(screen.getByRole('button', { name: 'Remove localhost:5432 (/usr/bin/ssh)' }));
        await submit(ref);
        expect(sentUpdate().execution.sandbox.networkAccess).toEqual({
          ...NETWORK, mode: 'allowlist', localPorts: [POSTGRES_PORT],
        });

        await userEvent.click(networkButton('Off'));
        expect(screen.queryByRole('list', { name: 'Local ports' })).toBeNull();
        expect(screen.queryByText(/^Local ports/)).toBeNull();
        await submit(ref);
        expect(sentUpdate().execution.sandbox.networkAccess).toEqual({ ...NETWORK, mode: 'off', localPorts: [POSTGRES_PORT] });
      });

      it('keeps this computer off the allowed hosts and notes a denied localhost', async () => {
        api.getSandboxStatus.mockResolvedValue(LINUX);
        api.workspaceGetAgent.mockResolvedValue(withPolicy({
          mode: 'allowlist', allowDomains: ['github.com', 'localhost'], denyDomains: [], localPorts: [SSH_PORT],
        }));
        const { ref } = renderForm({ agentId: 'agent-1' });
        await openAccess();
        const thisComputer = 'This computer is allowed one port at a time, as in localhost:5432, under Local ports.';
        const allowed = await screen.findByRole('list', { name: 'Allowed hosts' });
        expect(within(allowed).getByText(`Not valid: ${thisComputer}`)).toBeInTheDocument();

        const allowInput = screen.getByRole('textbox', { name: 'New entry for allowed hosts' });
        await userEvent.type(allowInput, '127.0.0.1{Enter}');
        expect(screen.getByRole('alert')).toHaveTextContent(thisComputer);
        await userEvent.clear(allowInput);
        await userEvent.type(allowInput, 'localhost:5432{Enter}');
        expect(screen.getByRole('alert'))
          .toHaveTextContent('Local ports go in the Local ports list, which checks the program listening.');
        await userEvent.clear(allowInput);

        expect(screen.queryByText(/Denied hosts include/)).toBeNull();
        const denyInput = screen.getByRole('textbox', { name: 'New entry for denied hosts' });
        await userEvent.type(denyInput, 'localhost:5432{Enter}');
        expect(screen.getByRole('alert'))
          .toHaveTextContent('To refuse a local port, remove it from Local ports; deny localhost to refuse them all.');
        await userEvent.clear(denyInput);
        await userEvent.type(denyInput, 'localhost{Enter}');
        expect(within(screen.getByRole('list', { name: 'Denied hosts' })).getByText('localhost')).toBeInTheDocument();
        expect(screen.getByText('Denied hosts include localhost, so these ports are refused.')).toBeInTheDocument();

        await submit(ref);
        expect(sentUpdate().execution.sandbox.networkAccess).toEqual({
          mode: 'allowlist', allowDomains: ['github.com', 'localhost'], denyDomains: ['localhost'], localPorts: [SSH_PORT],
        });
      });
    });

    it('says the mode covers only the web tools without a sandbox', async () => {
      api.getSandboxStatus.mockResolvedValue(NO_SANDBOX);
      renderForm({ agentId: 'agent-1' });
      expect(await screen.findByText('Network: Host network (web tools only)')).toBeInTheDocument();
    });

    it('marks the Host network hint as a warning and ties hints to their controls', async () => {
      api.getSandboxStatus.mockResolvedValue(MACOS);
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      await waitFor(() => expect(networkButton('Off')).toBeEnabled());
      const hint = screen.getByText(/including services on this computer/);
      expect(hint).toHaveClass(formStyles.hintWarning!);
      expect(screen.getByRole('group', { name: 'Network' })).toHaveAccessibleDescription(hint.textContent!);
      expect(networkButton('Allowlist')).toHaveAccessibleDescription('Allowlist: Not available on macOS yet.');
    });

    it('says it is checking, and offers a retry when the status cannot be read', async () => {
      api.getSandboxStatus.mockReturnValueOnce(new Promise(() => {}));
      const first = renderForm({ agentId: 'agent-1' });
      await openAccess();
      expect(screen.getByRole('status')).toHaveTextContent('Checking what this PC supports…');
      first.unmount();
      resetSandboxStatusForTests();

      api.getSandboxStatus.mockRejectedValue(new Error('ipc down'));
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      expect(await screen.findByRole('status')).toHaveTextContent(/Could not check what this PC supports/);
      expect(networkButton('Off')).toBeDisabled();

      api.getSandboxStatus.mockResolvedValue(LINUX);
      await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
      await waitFor(() => expect(networkButton('Off')).toBeEnabled());
      expect(screen.queryByText(/Could not check/)).toBeNull();
    });

    it('updates the overview too when Retry reads the status', async () => {
      api.getSandboxStatus.mockRejectedValueOnce(new Error('ipc down')).mockResolvedValue(NO_SANDBOX);
      renderForm({ agentId: 'agent-1' });
      await openAccess();
      await userEvent.click(await screen.findByRole('button', { name: 'Retry' }));
      await waitFor(() => expect(networkButton('Off')).toBeEnabled());
      await userEvent.click(screen.getByRole('button', { name: 'Back to Reviewer' }));
      expect(screen.getByText('Network: Host network (web tools only)')).toBeInTheDocument();
      expect(api.getSandboxStatus).toHaveBeenCalledTimes(2);
    });

    it('says this PC’s default applies when the defaults could not be loaded', async () => {
      renderForm();
      expect(await screen.findByText('Network: this PC’s default')).toBeInTheDocument();
      await userEvent.click(screen.getByRole('button', { name: 'Edit local access' }));
      expect(screen.getByText('This PC’s default applies when you save.')).toBeInTheDocument();
      expect(screen.queryByRole('group', { name: 'Network' })).toBeNull();
    });

    it('offers Off and Host network for the web tools without a sandbox', async () => {
      api.getSandboxStatus.mockResolvedValue(NO_SANDBOX);
      const { ref } = renderForm({ agentId: 'agent-1' });
      await openAccess();
      await waitFor(() => expect(networkButton('Off')).toBeEnabled());
      expect(screen.getByRole('group', { name: 'Network' }))
        .toHaveAccessibleDescription('On this PC it applies to web tools only; shell commands are not restricted.');
      expect(networkButton(/Host network/)).toBeEnabled();
      expect(networkButton('Allowlist')).toBeDisabled();
      expect(networkButton('Allowlist')).toHaveAttribute('title', 'Not available on this PC');

      await userEvent.click(networkButton('Off'));
      await submit(ref);
      expect(sentUpdate().execution.sandbox.networkAccess.mode).toBe('off');
    });

    // The cases `entries_are_parsed_by_one_rule` in network/matcher.rs runs.
    it('parses host entries as the backend matches them', () => {
      for (const { entry, matched } of hostEntryCases.accepted) {
        expect(parseHostEntry(entry), entry).toBe(matched);
      }
      for (const entry of hostEntryCases.rejected) {
        expect(parseHostEntry(entry), entry).toHaveProperty('error');
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
    // Only the failure: no hint to discard or reload.
    expect(screen.getByRole('alert')).toHaveTextContent(/^agent name already taken$/);
    expect(screen.queryByRole('button', { name: /Reload|Discard/ })).toBeNull();
  });

  it('bases each save on the agent it last loaded or saved', async () => {
    api.workspaceUpdateAgent.mockResolvedValue({ ...REVIEWER, updatedAt: 7 });
    const { ref } = renderForm({ agentId: 'agent-1' });
    await screen.findByDisplayValue('Reviewer');

    await submit(ref);
    expect(api.workspaceUpdateAgent).toHaveBeenLastCalledWith(expect.objectContaining({ expectedUpdatedAt: 5 }));
    await submit(ref);
    expect(api.workspaceUpdateAgent).toHaveBeenLastCalledWith(expect.objectContaining({ expectedUpdatedAt: 7 }));
  });

  // Recovering is the host's Discard, which mounts the form afresh (see
  // WorkspaceSettingsModal.stale.test.tsx).
  it('keeps the edits when a save is refused as stale, and says why', async () => {
    const stale = 'This agent was changed elsewhere after you opened it. Use Discard to load the latest settings.';
    api.workspaceUpdateAgent.mockRejectedValueOnce(new StaleAgentError(stale));
    const { ref, onDirtyChange } = renderForm({ agentId: 'agent-1' });
    await userEvent.type(await screen.findByLabelText('Instructions'), ' and tests');

    expect(await submit(ref)).toEqual({ ok: false, error: stale });
    expect(screen.getByRole('alert')).toHaveTextContent(stale);
    expect(screen.getByLabelText('Instructions')).toHaveValue('Reviews diffs and tests');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
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
