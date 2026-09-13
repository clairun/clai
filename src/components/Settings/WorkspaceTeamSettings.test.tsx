import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

import { AssignmentSection, TeamPolicySection } from './WorkspaceTeamSettings';

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
    const picker = await screen.findByLabelText('Shared agent');

    const options = Array.from(picker.querySelectorAll('option')).map((o) => o.textContent);
    expect(options).toContain('Writer');
    // Already on the team — a second assignment of the same definition would
    // create two identical rows the roster cannot tell apart.
    expect(options).not.toContain('Reviewer');
    // Archived definitions are kept for history, not for new work.
    expect(options).not.toContain('Retired');
  });

  it('adds the chosen agent to this workspace', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    render(<AssignmentSection workspaceId={WORKSPACE} onChanged={onChanged} />);
    const picker = await screen.findByLabelText('Shared agent');

    await user.selectOptions(picker, 'def-writer');
    await user.click(screen.getByRole('button', { name: 'Add' }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('workspace_assign_agent', {
        workspaceId: WORKSPACE,
        definitionId: 'def-writer',
      })
    );
    expect(onChanged).toHaveBeenCalled();
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

  it('names the callable id and says where shared behavior is edited', async () => {
    render(<AssignmentSection workspaceId={WORKSPACE} agentId="assign-1" />);
    await screen.findByText('Reviewer');

    expect(screen.getByText('assign-1')).toBeInTheDocument();
    expect(screen.getByText(/Settings → Agents/)).toBeInTheDocument();
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
