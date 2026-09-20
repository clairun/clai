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

import CrewList, { crewStatus, sortCrew } from './CrewList';
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
const DOCS = agent({ id: 'wa-docs', agentDefinitionId: 'def-docs', displayName: 'Docs' });

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
  onOpenPicker: vi.fn(),
  onOpenEdit: vi.fn(),
  onRemove: vi.fn(),
  onChanged: vi.fn(),
};

/** The crew rows only — picker cards are list items too. */
const rows = () => screen.getAllByRole('listitem').filter((el) => el.tagName === 'LI');

beforeEach(() => {
  api.listAgentDefinitions.mockReset().mockResolvedValue(LIBRARY);
  api.assignWorkspaceAgent.mockReset().mockResolvedValue('wa-docs');
});

describe('crew rules', () => {
  it('pins the Main first whatever order the details used', () => {
    expect(sortCrew([REVIEWER, MAIN, WRITER]).map((a) => a.id)).toEqual([
      'wa-main',
      'wa-review',
      'wa-writer',
    ]);
  });

  it('derives ring and line from the tasks, with disabled winning and running before attention', () => {
    expect(crewStatus(REVIEWER, [])).toEqual({ activity: 'idle', line: null });
    expect(
      crewStatus(REVIEWER, [task({ status: 'blocked' }), task({ id: 't2', status: 'queued' })])
    ).toEqual({
      activity: 'running',
      line: '1 task running',
    });
    expect(
      crewStatus(REVIEWER, [task({ status: 'running' }), task({ id: 't2', status: 'queued' })]).line
    ).toBe('2 tasks running');
    expect(crewStatus(REVIEWER, [task({ status: 'failed' })])).toEqual({
      activity: 'attention',
      line: 'Needs your review',
    });
    expect(crewStatus({ ...REVIEWER, enabled: false }, [task({ status: 'running' })])).toEqual({
      activity: 'disabled',
      line: 'Disabled in this workspace',
    });
  });

  it('stops asking for review once the task was acknowledged or answered, or belongs to someone else', () => {
    expect(
      crewStatus(REVIEWER, [task({ status: 'blocked', attentionAcknowledgedAt: 3n })]).line
    ).toBeNull();
    expect(crewStatus(REVIEWER, [task({ status: 'blocked', userResponseAt: 3n })]).line).toBeNull();
    expect(
      crewStatus(REVIEWER, [task({ assignedToWorkspaceAgentId: 'wa-other', status: 'running' })])
    ).toEqual({
      activity: 'idle',
      line: null,
    });
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
    const [main, reviewer, writer] = rows() as [HTMLElement, HTMLElement, HTMLElement];
    expect(
      [main, reviewer, writer].map(
        (row) => within(row).getByText(/Main|Reviewer|Writer/).textContent
      )
    ).toEqual(['Main', 'Reviewer', 'Writer']);
    expect(reviewer).toHaveAttribute('data-activity', 'running');
    expect(within(reviewer).getByText('1 task running')).toBeInTheDocument();
    expect(writer).toHaveAttribute('data-activity', 'disabled');
    expect(within(writer).getByText('Disabled in this workspace')).toBeInTheDocument();
    expect(within(main).queryByRole('button', { name: /Remove/ })).toBeNull();
    expect(
      within(reviewer).getByRole('button', { name: 'Remove Reviewer from the crew' })
    ).toBeInTheDocument();
    // Folded: neither the picker nor the invitation, and no library fetch.
    expect(screen.queryByTestId('crew-picker')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Add to crew' })).toBeNull();
    expect(api.listAgentDefinitions).not.toHaveBeenCalled();
  });

  it('invites the user to add crew when the Main works alone, and asks the parent to open the picker', async () => {
    const user = userEvent.setup();
    const onOpenPicker = vi.fn();
    render(<CrewList {...baseProps} onOpenPicker={onOpenPicker} agents={[MAIN]} tasks={[]} />);
    expect(
      screen.getByText('Main works alone here. Add an agent from the library, or create one.')
    ).toBeInTheDocument();
    expect(screen.queryByTestId('crew-picker')).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Add to crew' }));
    expect(onOpenPicker).toHaveBeenCalledTimes(1);
  });

  it('invites the user to add agents to an empty crew too, and hides the invitation once the picker is open', () => {
    const { rerender } = render(<CrewList {...baseProps} agents={[]} tasks={[]} />);
    expect(
      screen.getByText('No agents here yet. Add one from the library, or create one.')
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Add to crew' })).toBeInTheDocument();
    rerender(<CrewList {...baseProps} pickerOpen agents={[MAIN]} tasks={[]} />);
    expect(screen.queryByText(/works alone|No agents here/)).toBeNull();
    expect(screen.getByTestId('crew-picker')).toBeInTheDocument();
  });

  it('keeps the picker — and its Undo — mounted after the first assign on a Main-only crew', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    const { rerender } = render(
      <CrewList {...baseProps} onChanged={onChanged} pickerOpen agents={[MAIN]} tasks={[]} />
    );

    await user.click(await screen.findByRole('listitem', { name: 'Add Docs to the crew' }));
    await waitFor(() => expect(api.assignWorkspaceAgent).toHaveBeenCalledWith('ws-1', 'def-docs'));
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));

    // The parent reloads and the crew is no longer the Main alone.
    rerender(
      <CrewList {...baseProps} onChanged={onChanged} pickerOpen agents={[MAIN, DOCS]} tasks={[]} />
    );
    expect(screen.getByTestId('crew-picker')).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Docs joined the crew.');
    expect(screen.queryByRole('listitem', { name: 'Add Docs to the crew' })).toBeNull();
    // And the library was re-read after the crew changed.
    await waitFor(() => expect(api.listAgentDefinitions).toHaveBeenCalledTimes(2));
  });

  it('unfolds the picker on request, hiding agents already on the crew', async () => {
    render(<CrewList {...baseProps} pickerOpen agents={[MAIN, REVIEWER]} tasks={[]} />);
    expect(
      await screen.findByRole('listitem', { name: 'Add Docs to the crew' })
    ).toBeInTheDocument();
    expect(screen.getByRole('listitem', { name: 'Add Writer to the crew' })).toBeInTheDocument();
    expect(screen.queryByRole('listitem', { name: 'Add Reviewer to the crew' })).toBeNull();
    // Rows and cards are both list items; the rows are the <li>s.
    expect(rows()).toHaveLength(2);
    // The rows already say who is on the crew; the picker does not repeat them.
    expect(screen.queryByText('Already on this crew')).toBeNull();
  });

  it('lays the library cards out one per row, to fit the narrow drawer', async () => {
    render(<CrewList {...baseProps} pickerOpen agents={[MAIN]} tasks={[]} />);
    const card = await screen.findByRole('listitem', { name: 'Add Docs to the crew' });
    // The crew <ul> is a list too, so reach the card container through the card.
    expect(card.parentElement?.className).toMatch(/cardsRows/);
    expect(card.className).toMatch(/cardRow/);
    expect(card.querySelector('svg')?.getAttribute('width')).toBe('36');
  });

  it('deep-links "Create a new agent" into Settings → Agents', async () => {
    const user = userEvent.setup();
    render(<CrewList {...baseProps} pickerOpen agents={[MAIN]} tasks={[]} />);
    await user.click(await screen.findByRole('button', { name: 'Create a new agent →' }));
    expect(settings.openGlobalSettings).toHaveBeenCalledWith({ tab: 'agents' });
  });

  it('shows rows without actions, invitation or picker when the workspace is not manageable', () => {
    render(<CrewList {...baseProps} manageable={false} pickerOpen agents={[MAIN]} tasks={[]} />);
    expect(rows()).toHaveLength(1);
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.queryByTestId('crew-picker')).toBeNull();
  });
});
