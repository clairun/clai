import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const api = vi.hoisted(() => ({
  listAgentDefinitions: vi.fn(),
  assignWorkspaceAgent: vi.fn(),
  workspaceDeleteAgent: vi.fn(),
}));
vi.mock('../../api/client', () => api);
const settings = vi.hoisted(() => ({ openGlobalSettings: vi.fn() }));
vi.mock('../../utils/globalSettings', () => settings);

import CrewList, { crewActivity, liveLine, sortCrew } from './CrewList';
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';

const agent = (over: Partial<WorkspaceAgentResponse>): WorkspaceAgentResponse => ({
  id: 'wa',
  workspaceId: 'ws-1',
  agentDefinitionId: 'def',
  displayName: 'Agent',
  role: 'member',
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
  ...over,
});

const task = (over: Partial<WorkspaceTaskResponse>): WorkspaceTaskResponse => ({
  id: 't',
  workspaceId: 'ws-1',
  createdByWorkspaceAgentId: 'wa-main',
  createdByDisplayName: 'Main',
  assignedToWorkspaceAgentId: 'wa-review',
  assignedAgentDefinitionId: 'def-review',
  assignedAgentDisplayName: 'Reviewer',
  title: 'Task',
  instructions: '',
  status: 'completed',
  resultSummary: null,
  error: null,
  sessionId: null,
  runId: null,
  createdAt: 1n,
  updatedAt: 1n,
  completedAt: null,
  attentionAcknowledgedAt: null,
  userResponse: null,
  userResponseAt: null,
  ...over,
});

const MAIN = agent({
  id: 'wa-main',
  agentDefinitionId: 'def-main',
  isDefault: true,
  role: 'manager',
});
const REVIEWER = agent({
  id: 'wa-review',
  agentDefinitionId: 'def-review',
  displayName: 'Reviewer',
  agentDescription: 'Reviews PRs',
});
const WRITER = agent({
  id: 'wa-writer',
  agentDefinitionId: 'def-writer',
  displayName: 'Writer',
  enabled: false,
});

const LIBRARY = [
  {
    id: 'def-review',
    name: 'Reviewer',
    archived: false,
    selectedSkillIds: [],
    selectedMcpServerIds: [],
    avatar: null,
  },
  {
    id: 'def-writer',
    name: 'Writer',
    archived: false,
    selectedSkillIds: [],
    selectedMcpServerIds: [],
    avatar: null,
  },
  {
    id: 'def-docs',
    name: 'Docs',
    archived: false,
    selectedSkillIds: [],
    selectedMcpServerIds: [],
    avatar: null,
  },
];

const baseProps = {
  workspaceId: 'ws-1',
  manageable: true,
  pickerOpen: false,
  onOpenEdit: vi.fn(),
  onRemove: vi.fn(),
  onChanged: vi.fn(),
};

beforeEach(() => {
  api.listAgentDefinitions.mockReset().mockResolvedValue(LIBRARY);
  api.assignWorkspaceAgent.mockReset().mockResolvedValue('wa-docs');
});

describe('crew rules', () => {
  it('pins the Main first whatever order the snapshot used', () => {
    expect(sortCrew([REVIEWER, MAIN, WRITER]).map((a) => a.id)).toEqual([
      'wa-main',
      'wa-review',
      'wa-writer',
    ]);
  });

  it('derives the ring from the tasks, with disabled winning', () => {
    const running = [task({ status: 'running' })];
    expect(crewActivity(REVIEWER, running)).toBe('running');
    expect(crewActivity({ ...REVIEWER, enabled: false }, running)).toBe('disabled');
    expect(crewActivity(REVIEWER, [])).toBe('idle');
  });

  it('writes the live line from running tasks first, then attention', () => {
    expect(
      liveLine('wa-review', [task({ status: 'running' }), task({ id: 't2', status: 'queued' })])
    ).toBe('2 tasks running');
    expect(liveLine('wa-review', [task({ status: 'blocked' })])).toBe('Needs your review');
    expect(
      liveLine('wa-review', [task({ status: 'blocked', attentionAcknowledgedAt: 3n })])
    ).toBeNull();
    expect(
      liveLine('wa-review', [task({ assignedToWorkspaceAgentId: 'wa-other', status: 'running' })])
    ).toBeNull();
  });
});

describe('CrewList', () => {
  it('renders a row per agent with its state, and no Remove for the Main', () => {
    render(
      <CrewList
        {...baseProps}
        agents={[REVIEWER, MAIN, WRITER]}
        tasks={[task({ status: 'running' })]}
      />
    );
    const rows = screen.getAllByRole('listitem');
    expect(rows.map((row) => within(row).getByText(/Main|Reviewer|Writer/).textContent)).toEqual([
      'Main',
      'Reviewer',
      'Writer',
    ]);
    const [main, reviewer, writer] = rows as [HTMLElement, HTMLElement, HTMLElement];
    expect(reviewer).toHaveAttribute('data-activity', 'running');
    expect(within(reviewer).getByText('1 task running')).toBeInTheDocument();
    expect(writer).toHaveAttribute('data-activity', 'disabled');
    expect(within(writer).getByText('Disabled in this workspace')).toBeInTheDocument();
    expect(within(main).queryByRole('button', { name: /Remove/ })).toBeNull();
    expect(
      within(reviewer).getByRole('button', { name: 'Remove Reviewer from the crew' })
    ).toBeInTheDocument();
    // The picker stays folded: the crew is not the Main alone.
    expect(screen.queryByTestId('crew-picker')).toBeNull();
    expect(api.listAgentDefinitions).not.toHaveBeenCalled();
  });

  it('opens the picker by itself when the Main works alone', async () => {
    render(<CrewList {...baseProps} agents={[MAIN]} tasks={[]} />);
    expect(
      await screen.findByText('Main works alone here. Add crew from the library, or create one.')
    ).toBeInTheDocument();
    expect(screen.getByRole('listitem', { name: 'Add Docs to the crew' })).toBeInTheDocument();
  });

  it('unfolds the picker on request, hides agents already on the crew, and reloads after an assign', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    render(
      <CrewList
        {...baseProps}
        onChanged={onChanged}
        pickerOpen
        agents={[MAIN, REVIEWER]}
        tasks={[]}
      />
    );

    expect(
      await screen.findByRole('listitem', { name: 'Add Docs to the crew' })
    ).toBeInTheDocument();
    expect(screen.getByRole('listitem', { name: 'Add Writer to the crew' })).toBeInTheDocument();
    expect(screen.queryByRole('listitem', { name: 'Add Reviewer to the crew' })).toBeNull();

    await user.click(screen.getByRole('listitem', { name: 'Add Docs to the crew' }));
    await waitFor(() => expect(api.assignWorkspaceAgent).toHaveBeenCalledWith('ws-1', 'def-docs'));
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(api.listAgentDefinitions).toHaveBeenCalledTimes(2));
  });

  it('deep-links "Create a new agent" into Settings → Agents', async () => {
    const user = userEvent.setup();
    render(<CrewList {...baseProps} pickerOpen agents={[MAIN]} tasks={[]} />);
    await user.click(await screen.findByRole('button', { name: 'Create a new agent →' }));
    expect(settings.openGlobalSettings).toHaveBeenCalledWith({ tab: 'agents' });
  });

  it('shows rows without actions or picker when the workspace is not manageable', () => {
    render(<CrewList {...baseProps} manageable={false} agents={[MAIN, REVIEWER]} tasks={[]} />);
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.queryByTestId('crew-picker')).toBeNull();
  });
});
