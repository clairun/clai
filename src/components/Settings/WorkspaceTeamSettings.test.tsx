import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

import { AssignmentSection, TeamPolicySection } from './WorkspaceTeamSettings';
import {
  OPEN_GLOBAL_SETTINGS_EVENT,
  type OpenGlobalSettingsDetail,
} from '../../utils/globalSettings';

const cleanups: Array<() => void> = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
  vi.restoreAllMocks();
});

/** Records the deep links the section asks the app to open. */
const recordGlobalSettingsOpens = (): OpenGlobalSettingsDetail[] => {
  const seen: OpenGlobalSettingsDetail[] = [];
  const listener = (event: Event) =>
    seen.push((event as CustomEvent<OpenGlobalSettingsDetail>).detail);
  window.addEventListener(OPEN_GLOBAL_SETTINGS_EVENT, listener);
  cleanups.push(() => window.removeEventListener(OPEN_GLOBAL_SETTINGS_EVENT, listener));
  return seen;
};

const WORKSPACE = 'ws-1';

const DEFINITIONS = [
  {
    id: 'def-review',
    revision: 2,
    archived: false,
    name: 'Reviewer',
    description: 'Reviews diffs',
    selectedSkillIds: [],
    selectedMcpServerIds: [],
    providerConnectionIds: [],
    execution: {},
    enabled: true,
    createdAt: 1,
    updatedAt: 1,
    assignedWorkspaces: [],
  },
  {
    id: 'def-writer',
    revision: 1,
    archived: false,
    name: 'Writer',
    description: '',
    selectedSkillIds: [],
    selectedMcpServerIds: [],
    providerConnectionIds: [],
    execution: {},
    enabled: true,
    createdAt: 1,
    updatedAt: 1,
    assignedWorkspaces: [],
  },
  {
    id: 'def-retired',
    revision: 1,
    archived: true,
    name: 'Retired',
    description: '',
    selectedSkillIds: [],
    selectedMcpServerIds: [],
    providerConnectionIds: [],
    execution: {},
    enabled: true,
    createdAt: 1,
    updatedAt: 1,
    assignedWorkspaces: [],
  },
];

const ASSIGNMENT = {
  id: 'assign-1',
  agentDefinitionId: 'def-review',
  enabled: true,
  context: 'API crate only',
  filesystemGrants: [{ path: '/srv/data', access: 'read_write' }],
  createdAt: 1,
  updatedAt: 1,
};

const policy = {
  context: 'House rules',
  filesystemGrants: [{ path: '/opt/tools', access: 'read_only' }],
  assignments: [ASSIGNMENT],
};

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'agent_definitions_list') return Promise.resolve(DEFINITIONS);
    if (cmd === 'workspace_team_policy') return Promise.resolve(policy);
    if (cmd === 'workspace_assign_agent') return Promise.resolve('assign-2');
    if (cmd === 'workspace_configure_assignment') return Promise.resolve(null);
    if (cmd === 'workspace_save_team_policy') return Promise.resolve(null);
    if (cmd === 'workspace_delete_agent') return Promise.resolve(null);
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  });
});

describe('AssignmentSection picker', () => {
  it('offers only shared agents this workspace can still add', async () => {
    render(<AssignmentSection workspaceId={WORKSPACE} />);
    expect(await screen.findByRole('listitem', { name: 'Add Writer to the crew' })).toBeInTheDocument();

    // Already on the team — a second assignment of the same definition would
    // create two identical rows the roster cannot tell apart.
    expect(screen.queryByRole('listitem', { name: /Reviewer/ })).toBeNull();
    // Archived definitions are kept for history, not for new work.
    expect(screen.queryByRole('listitem', { name: /Retired/ })).toBeNull();
  });

  it('adds the chosen agent to this workspace on one click', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    render(<AssignmentSection workspaceId={WORKSPACE} onChanged={onChanged} />);

    await user.click(await screen.findByRole('listitem', { name: 'Add Writer to the crew' }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('workspace_assign_agent', {
        workspaceId: WORKSPACE,
        definitionId: 'def-writer',
      })
    );
    expect(onChanged).toHaveBeenCalled();
    // The section reloads its own roster so the card does not linger.
    await waitFor(() =>
      expect(mockInvoke.mock.calls.filter(([cmd]) => cmd === 'workspace_team_policy').length).toBe(2)
    );
  });

  it('keeps the cards disabled until the roster has been re-read after an assign', async () => {
    const user = userEvent.setup();
    let releasePolicy: (value: unknown) => void = () => {};
    let policyCalls = 0;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'agent_definitions_list') return Promise.resolve(DEFINITIONS);
      if (cmd === 'workspace_team_policy') {
        policyCalls += 1;
        // The first read renders the picker; the second is the refetch after the assign.
        if (policyCalls === 1) return Promise.resolve(policy);
        return new Promise((resolve) => {
          releasePolicy = resolve;
        });
      }
      if (cmd === 'workspace_assign_agent') return Promise.resolve('assign-2');
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    });
    render(<AssignmentSection workspaceId={WORKSPACE} />);

    const writer = await screen.findByRole('listitem', { name: 'Add Writer to the crew' });
    await user.click(writer);
    await waitFor(() => expect(policyCalls).toBe(2));
    // A second click here would ask the backend to assign Writer twice.
    expect(screen.getByRole('listitem', { name: 'Add Writer to the crew' })).toBeDisabled();

    releasePolicy({
      ...policy,
      assignments: [ASSIGNMENT, { ...ASSIGNMENT, id: 'assign-2', agentDefinitionId: 'def-writer' }],
    });
    await waitFor(() => expect(screen.queryByRole('listitem', { name: 'Add Writer to the crew' })).toBeNull());
    expect(screen.getByRole('status')).toHaveTextContent('Writer joined the crew.');
  });

  it('offers a create link that opens the shared library, with nothing left to add', async () => {
    const user = userEvent.setup();
    const opened = recordGlobalSettingsOpens();
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'agent_definitions_list') return Promise.resolve(DEFINITIONS.filter((d) => d.id === 'def-review'));
      if (cmd === 'workspace_team_policy') return Promise.resolve(policy);
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    });
    render(<AssignmentSection workspaceId={WORKSPACE} />);
    expect(
      await screen.findByText('Every agent in the library is already on this crew.')
    ).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Create a new agent →' }));
    expect(opened).toEqual([{ tab: 'agents' }]);
  });
});

describe('AssignmentSection editor', () => {
  it('saves only the local overlays, keeping the shared identity', async () => {
    const user = userEvent.setup();
    render(<AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" />);
    await screen.findByText('Reviewer');

    await user.clear(screen.getByLabelText('Context for this workspace'));
    await user.type(screen.getByLabelText('Context for this workspace'), 'Docs only');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith('workspace_configure_assignment', {
      workspaceId: WORKSPACE,
      assignment: { ...ASSIGNMENT, context: 'Docs only' },
    }));
  });

  it('hands the modal back control after unassigning instead of sitting on a dead row', async () => {
    const user = userEvent.setup();
    const onUnassigned = vi.fn();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    render(
      <AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" onUnassigned={onUnassigned} />
    );
    await screen.findByText('Reviewer');

    await user.click(screen.getByRole('button', { name: 'Remove from crew' }));

    expect(confirm).toHaveBeenCalledWith(expect.stringContaining('Remove Reviewer from this crew?'));
    await waitFor(() => expect(onUnassigned).toHaveBeenCalledWith('assign-1'));
    expect(mockInvoke).toHaveBeenCalledWith('workspace_delete_agent', {
      workspaceId: WORKSPACE,
      agentId: 'assign-1',
    });
  });

  it('drops nothing when the remove is not confirmed', async () => {
    const user = userEvent.setup();
    const onUnassigned = vi.fn();
    vi.spyOn(window, 'confirm').mockReturnValue(false);
    render(
      <AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" onUnassigned={onUnassigned} />
    );
    await screen.findByText('Reviewer');

    await user.click(screen.getByRole('button', { name: 'Remove from crew' }));

    expect(mockInvoke).not.toHaveBeenCalledWith('workspace_delete_agent', expect.anything());
    expect(onUnassigned).not.toHaveBeenCalled();
  });

  // The two controls look adjacent but are not alternatives: parking keeps the
  // row (and with it the context, the grants and the callable id); removing
  // deletes it.
  it('parks the agent through the assignment row instead of deleting it', async () => {
    const user = userEvent.setup();
    const onUnassigned = vi.fn();
    render(
      <AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" onUnassigned={onUnassigned} />
    );
    await screen.findByText('Reviewer');

    await user.click(screen.getByRole('checkbox', { name: 'Enabled in this workspace' }));
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('workspace_configure_assignment', {
        workspaceId: WORKSPACE,
        assignment: { ...ASSIGNMENT, enabled: false },
      })
    );
    expect(mockInvoke).not.toHaveBeenCalledWith('workspace_delete_agent', expect.anything());
    expect(onUnassigned).not.toHaveBeenCalled();
  });

  it('names the callable id and opens the shared definition it belongs to', async () => {
    const user = userEvent.setup();
    const opened = recordGlobalSettingsOpens();
    render(<AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" />);
    await screen.findByText('Reviewer');

    expect(screen.getByText('assign-1')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Edit in Settings → Agents' }));

    // The definition id, not this workspace's callable id: the editor over
    // there edits the shared agent.
    expect(opened).toEqual([{ tab: 'agents', agentDefinitionId: 'def-review' }]);
  });

  it('asks before leaving an unsaved draft for the shared definition', async () => {
    const user = userEvent.setup();
    const opened = recordGlobalSettingsOpens();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    render(<AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" />);
    await screen.findByText('Reviewer');

    await user.type(screen.getByLabelText('Context for this workspace'), ' and docs');
    await user.click(screen.getByRole('button', { name: 'Edit in Settings → Agents' }));

    expect(confirm).toHaveBeenCalled();
    expect(opened).toEqual([]);
  });

  it('explains an assignment whose shared agent is gone instead of rendering a blank form', async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'agent_definitions_list') return Promise.resolve([]);
      if (cmd === 'workspace_team_policy') return Promise.resolve(policy);
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    });

    render(<AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" />);

    expect(await screen.findByText('Unavailable agent')).toBeInTheDocument();
  });
});

describe('TeamPolicySection', () => {
  it('saves the project context and the workspace-wide grants together', async () => {
    const user = userEvent.setup();
    render(<TeamPolicySection workspaceId={WORKSPACE} />);
    const context = await screen.findByLabelText('Instructions shared by every agent here');

    await user.clear(context);
    await user.type(context, 'Ship on Fridays');
    await user.click(screen.getByRole('button', { name: 'Save team settings' }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('workspace_save_team_policy', {
        workspaceId: WORKSPACE,
        context: 'Ship on Fridays',
        filesystemGrants: [{ path: '/opt/tools', access: 'read_only' }],
      })
    );
  });
});
