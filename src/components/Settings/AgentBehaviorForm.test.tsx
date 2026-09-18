import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';

const api = vi.hoisted(() => ({
  workspaceGetAgent: vi.fn(),
  workspaceCreateAgent: vi.fn(),
  workspaceUpdateAgent: vi.fn(),
  workspaceDeleteAgent: vi.fn(),
}));
vi.mock('../../api/client', () => api);

import { AgentBehaviorForm, type AgentDetail, type AgentFormDeps } from './AgentBehaviorForm';
import type { SectionHandle } from './sectionHandle';
import type { ProviderConnection } from '../../generated/bindings';

const connection = (id: string, name: string): ProviderConnection =>
  ({ id, name, enabled: true } as unknown as ProviderConnection);

const DEPS: AgentFormDeps = {
  mcpServers: [{ id: 'srv-a', name: 'Alpha' }],
  skills: [],
  providerConnections: [connection('conn-a', 'Anthropic'), connection('conn-b', 'OpenAI')],
  defaultExecution: null,
};

const HOME_GRANT = { path: '/home/u', access: 'read_only', origin: { kind: 'credentialsPreset' } };

const REVIEWER: AgentDetail = {
  id: 'agent-1',
  name: 'Reviewer',
  description: 'Reviews diffs',
  isDefault: false,
  enabled: true,
  selectedSkillIds: [],
  selectedMcpServerIds: ['srv-a'],
  providerConnectionIds: ['conn-a'],
  execution: {
    sandbox: { network: 'enabled', sessionBus: 'allow' },
    filesystem: { extraPaths: [HOME_GRANT] },
    shell: { mode: 'restricted', allowedCommandPrefixes: ['git status'], blockedCommandPrefixes: ['rm'] },
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
      snapshot={null}
      deps={DEPS}
      saving={false}
      onDirtyChange={onDirtyChange}
      {...props}
    />
  );
  return { ...utils, ref, onDirtyChange };
};

describe('AgentBehaviorForm', () => {
  beforeEach(() => {
    api.workspaceGetAgent.mockResolvedValue(REVIEWER);
    api.workspaceCreateAgent.mockResolvedValue({});
    api.workspaceUpdateAgent.mockResolvedValue({});
  });

  it('loads a workspace agent, tracks dirtiness and saves the full normalized payload', async () => {
    const { ref, onDirtyChange } = renderForm({ agentId: 'agent-1' });

    expect(await screen.findByDisplayValue('Reviewer')).toHaveAccessibleName(/^Name/);
    expect(api.workspaceGetAgent).toHaveBeenCalledWith('ws-1', 'agent-1');
    // Loaded state is the baseline: nothing is dirty yet.
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
    expect(screen.getByText('/home/u')).toBeInTheDocument();
    expect(screen.getByText('Preset')).toBeInTheDocument();

    await userEvent.type(screen.getByLabelText('Description'), ' carefully  ');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    // Reverting the edit reports clean again: the comparison is symmetric.
    await userEvent.clear(screen.getByLabelText('Description'));
    await userEvent.type(screen.getByLabelText('Description'), 'Reviews diffs');
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
    await userEvent.type(screen.getByLabelText('Description'), ' carefully  ');

    let result: { ok: boolean; error?: string } | undefined;
    await act(async () => { result = await ref.current!.submit(); });
    expect(result).toEqual({ ok: true });
    expect(api.workspaceUpdateAgent).toHaveBeenCalledWith({
      workspaceId: 'ws-1',
      agentId: 'agent-1',
      name: 'Reviewer',
      description: 'Reviews diffs carefully',
      selectedSkillIds: [],
      selectedMcpServerIds: ['srv-a'],
      providerConnectionIds: ['conn-a'],
      execution: {
        sandbox: { network: 'enabled', sessionBus: 'allow' },
        filesystem: { extraPaths: [{ ...HOME_GRANT, origin: HOME_GRANT.origin }] },
        shell: { mode: 'restricted', allowedCommandPrefixes: ['git status'], blockedCommandPrefixes: ['rm'] },
        web: { enabled: true },
      },
      enabled: true,
    });
    // The saved values become the new baseline — reported without the host
    // re-rendering the form (`saving` stays false throughout this test).
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
  });

  it('create flow waits for the backend defaults, validates, then sends the pre-populated grant', async () => {
    const { ref, rerender, onDirtyChange } = renderForm({ deps: { ...DEPS, defaultExecution: undefined } });
    expect(screen.getByText('Loading…')).toBeInTheDocument();

    rerender(
      <AgentBehaviorForm
        ref={ref}
        workspaceId="ws-1"
        agentId={null}
        snapshot={null}
        deps={{ ...DEPS, defaultExecution: { filesystem: { extraPaths: [HOME_GRANT] } } }}
        saving={false}
        onDirtyChange={onDirtyChange}
      />
    );
    expect(await screen.findByText('Set up the main agent')).toBeInTheDocument();
    expect(screen.getByText('/home/u')).toBeInTheDocument();

    let verdict: { ok: boolean; error?: string } | undefined;
    act(() => { verdict = ref.current!.validate(); });
    expect(verdict).toEqual({ ok: false, error: 'Agent name is required.' });
    await userEvent.type(screen.getByLabelText(/^Name/), 'Scout');
    act(() => { verdict = ref.current!.validate(); });
    expect(verdict).toEqual({ ok: false, error: 'Select at least one provider connection.' });
    expect(screen.getByText('Select at least one provider connection.')).toBeInTheDocument();

    await userEvent.selectOptions(screen.getByDisplayValue('Anthropic'), 'conn-b');
    await userEvent.click(screen.getAllByRole('button', { name: 'Add' })[0]!);
    expect(screen.getByText('OpenAI')).toBeInTheDocument();

    await act(async () => { await ref.current!.submit(); });
    expect(api.workspaceCreateAgent).toHaveBeenCalledTimes(1);
    const request = api.workspaceCreateAgent.mock.calls[0]![0] as {
      name: string;
      providerConnectionIds: string[];
      execution: { filesystem: { extraPaths: unknown[] }; shell: { mode: string; blockedCommandPrefixes: string[] } };
    };
    expect(request.name).toBe('Scout');
    expect(request.providerConnectionIds).toEqual(['conn-b']);
    expect(request.execution.filesystem.extraPaths).toEqual([HOME_GRANT]);
    expect(request.execution.shell.mode).toBe('off');
    // The destructive-command block list is the local default, not something the host supplies.
    expect(request.execution.shell.blockedCommandPrefixes).toEqual([
      'rm', 'sudo', 'chmod', 'chown', 'dd', 'mkfs', 'mount', 'umount', 'shutdown', 'reboot',
    ]);
    expect(api.workspaceUpdateAgent).not.toHaveBeenCalled();
    // A created agent is clean: a second Save after a sibling section failed
    // must not create it again.
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
  });

  it('disables every control while the host is saving', async () => {
    renderForm({ agentId: 'agent-1', saving: true });
    await screen.findByDisplayValue('Reviewer');
    for (const control of [
      screen.getByLabelText(/^Name/),
      screen.getByLabelText('Description'),
      screen.getByLabelText('Shell access'),
      screen.getByRole('button', { name: 'Delete agent' }),
      ...screen.getAllByRole('button', { name: 'Add' }),
      ...screen.getAllByRole('button', { name: /^Remove/ }),
      ...screen.getAllByRole('checkbox'),
    ]) {
      expect(control).toBeDisabled();
    }
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
    expect(onDeleted).not.toHaveBeenCalled();

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
    await act(async () => { await ref.current!.submit(); });

    expect(saveBehavior).toHaveBeenCalledWith(expect.objectContaining({ workspaceId: 'ws-1', name: 'Auditor', enabled: true }));
    expect(api.workspaceUpdateAgent).not.toHaveBeenCalled();
    expect(onDirtyChange).toHaveBeenLastCalledWith(false);
  });

  it('keeps the Main agent nameless, always enabled and undeletable', async () => {
    api.workspaceGetAgent.mockResolvedValue({ ...REVIEWER, id: 'main', name: 'Manager', isDefault: true, enabled: false });
    const { ref } = renderForm({ agentId: 'main' });

    expect(await screen.findByText('Main agent')).toBeInTheDocument();
    await screen.findByDisplayValue('Reviews diffs');
    expect(screen.queryByLabelText(/^Name/)).not.toBeInTheDocument();
    expect(screen.queryByText('Enabled')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Delete agent' })).not.toBeInTheDocument();

    await act(async () => { await ref.current!.submit(); });
    expect(api.workspaceUpdateAgent).toHaveBeenCalledWith(
      expect.objectContaining({ agentId: 'main', name: 'Manager', enabled: true })
    );
  });

  it('surfaces a backend save failure inline and reports it to the host', async () => {
    api.workspaceUpdateAgent.mockRejectedValue(new Error('agent name already taken'));
    const { ref } = renderForm({ agentId: 'agent-1' });
    await screen.findByDisplayValue('Reviewer');

    let result: { ok: boolean; error?: string } | undefined;
    await act(async () => { result = await ref.current!.submit(); });
    expect(result).toEqual({ ok: false, error: 'agent name already taken' });
    expect(screen.getByText('agent name already taken')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole('button', { name: 'Delete agent' })).toBeEnabled());
  });
});
