import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const api = vi.hoisted(() => ({ acknowledgeWorkspaceTask: vi.fn() }));
vi.mock('../../workspace/client', () => api);

import TaskList, { filterFaces, taskActivity } from './TaskList';
import AgentAvatar from './AgentAvatar';
import { identityFor, mainIdentity, taskIdentity, type AgentActivity } from './agentIdentity';
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
  createdByDisplayName: 'Manager',
  assignedToWorkspaceAgentId: 'wa-review',
  assignedAgentDefinitionId: 'def-review',
  // Recorded at hand-off; the roster's current name wins over it on screen.
  assignedAgentDisplayName: 'Reviewer (old name)',
  title: 'Task',
  instructions: 'Do it',
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

const MAIN = agent({ id: 'wa-main', agentDefinitionId: 'def-main', isDefault: true });
const REVIEWER = agent({
  id: 'wa-review',
  agentDefinitionId: 'def-review',
  displayName: 'Reviewer',
  avatar: { seed: 'picked', generatorVersion: 1 },
});
const ROSTER = [MAIN, REVIEWER];

const REVIEW_TASK = task({
  id: 't-review',
  title: 'Review PR',
  status: 'running',
  sessionId: 'sess-1',
});
const WRITE_TASK = task({
  id: 't-write',
  title: 'Write notes',
  status: 'blocked',
  assignedToWorkspaceAgentId: 'wa-gone',
  assignedAgentDefinitionId: 'def-gone',
  assignedAgentDisplayName: 'Writer (gone)',
});
const USER_TASK = task({
  id: 't-user',
  title: 'From the user',
  createdByWorkspaceAgentId: null,
  createdByDisplayName: null,
});

/** The SVG `AgentAvatar` draws for an identity at a size and activity, minus the wrapper. */
const svgOf = (
  identity: ReturnType<typeof identityFor>,
  size: number,
  activity: AgentActivity = 'none'
) =>
  render(
    <AgentAvatar identity={identity} size={size} activity={activity} />
  ).container.querySelector('span')?.innerHTML;

beforeEach(() => {
  api.acknowledgeWorkspaceTask.mockReset().mockResolvedValue(undefined);
});

describe('task rules', () => {
  it('rings a row by the task state, not the agent', () => {
    expect(taskActivity(task({ status: 'queued' }))).toBe('running');
    expect(taskActivity(task({ status: 'failed' }))).toBe('attention');
    expect(taskActivity(task({ status: 'failed', attentionAcknowledgedAt: 2n }))).toBe('none');
    expect(taskActivity(task({ status: 'completed' }))).toBe('none');
  });

  it('builds one filter face per assignee, roster names first, attention folded in', () => {
    const faces = filterFaces(
      [REVIEW_TASK, WRITE_TASK, task({ id: 't3', status: 'failed' })],
      ROSTER
    );
    expect(faces.map((face) => [face.agentId, face.name, face.attention])).toEqual([
      ['wa-review', 'Reviewer', true],
      ['wa-gone', 'Writer (gone)', true],
    ]);
    expect(faces[0]?.identity.seed).toBe('picked');
    expect(faces[1]?.identity.seed).toBe('def-gone');
  });
});

describe('TaskList', () => {
  const props = { workspaceId: 'ws-1', roster: ROSTER, onChanged: vi.fn() };

  it('leads each task with the assignee face and writes creator → assignee', () => {
    render(<TaskList {...props} tasks={[REVIEW_TASK, USER_TASK]} />);
    const [review, fromUser] = screen.getAllByRole('listitem') as [HTMLElement, HTMLElement];

    // The assignee's picked face, ringed as running, is the row's lead.
    const lead = review.querySelector('[data-activity="running"]') as HTMLElement;
    expect(lead.innerHTML).toBe(svgOf(identityFor(REVIEWER), 28, 'running'));
    // The roster names the creator: it is this workspace's Main, whatever the
    // display name the task was recorded with.
    expect(within(review).getByText('Main')).toBeInTheDocument();
    expect(within(review).getByText('→')).toBeInTheDocument();
    expect(within(review).getByText('Reviewer')).toBeInTheDocument();
    // The creator is the Main: its face is the fixed one.
    expect(review.innerHTML).toContain(svgOf(mainIdentity(), 14));

    // Created by the user: assignee only, no arrow.
    expect(within(fromUser).queryByText('→')).toBeNull();
    expect(within(fromUser).getByText('Reviewer')).toBeInTheDocument();
  });

  it('keeps the face of an agent that left the crew', () => {
    render(<TaskList {...props} tasks={[WRITE_TASK]} />);
    const row = screen.getByRole('listitem');
    expect(row.querySelector('[data-activity="attention"]')?.innerHTML).toBe(
      svgOf(taskIdentity(WRITE_TASK, []), 28, 'attention')
    );
    expect(within(row).getByText('Writer (gone)')).toBeInTheDocument();
  });

  it('filters by face, multi-select, and never hides everything behind a stale pick', async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <TaskList {...props} tasks={[REVIEW_TASK, WRITE_TASK, USER_TASK]} />
    );
    const filter = screen.getByRole('group', { name: 'Filter by agent' });
    expect(within(filter).getAllByRole('button')).toHaveLength(2);

    await user.click(within(filter).getByRole('button', { name: 'Writer (gone)' }));
    expect(
      screen
        .getAllByRole('listitem')
        .map((li) => within(li).getByText(/Review PR|Write notes|From the user/).textContent)
    ).toEqual(['Write notes']);

    await user.click(within(filter).getByRole('button', { name: 'Reviewer' }));
    expect(screen.getAllByRole('listitem')).toHaveLength(3);

    await user.click(screen.getByRole('button', { name: 'Show all' }));
    expect(within(filter).queryByRole('button', { pressed: true })).toBeNull();

    // Select the gone writer, then its task disappears: the list shows all again.
    await user.click(within(filter).getByRole('button', { name: 'Writer (gone)' }));
    rerender(<TaskList {...props} tasks={[REVIEW_TASK, USER_TASK]} />);
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
    expect(screen.queryByRole('group', { name: 'Filter by agent' })).toBeNull();

    // …and stays showing all when that agent's task comes back: the pick was forgotten.
    rerender(<TaskList {...props} tasks={[REVIEW_TASK, WRITE_TASK, USER_TASK]} />);
    expect(screen.getAllByRole('listitem')).toHaveLength(3);
    expect(screen.queryByRole('button', { pressed: true })).toBeNull();
  });

  it('hides the filter row with a single agent and explains an empty filtered list', () => {
    render(<TaskList {...props} tasks={[REVIEW_TASK]} />);
    expect(screen.queryByRole('group', { name: 'Filter by agent' })).toBeNull();
    render(<TaskList {...props} tasks={[]} />);
    expect(screen.getByText('No delegated tasks yet.')).toBeInTheDocument();
  });

  it('disables every Mark reviewed while one acknowledge is in flight', async () => {
    const user = userEvent.setup();
    let release: () => void = () => {};
    api.acknowledgeWorkspaceTask.mockImplementation(
      () => new Promise<void>((resolve) => (release = resolve))
    );
    const second = task({ id: 't-fail', title: 'Failed one', status: 'failed' });
    render(<TaskList {...props} tasks={[WRITE_TASK, second]} />);

    const [first, other] = screen.getAllByRole('button', { name: 'Mark reviewed' }) as [
      HTMLElement,
      HTMLElement,
    ];
    await user.click(first);
    expect(first).toBeDisabled();
    // A click here would otherwise be dropped silently by the busy guard.
    expect(other).toBeDisabled();
    release();
    await waitFor(() => expect(other).toBeEnabled());
  });

  it('acknowledges a blocked task once and reloads', async () => {
    const user = userEvent.setup();
    const onChanged = vi.fn();
    render(<TaskList {...props} onChanged={onChanged} tasks={[REVIEW_TASK, WRITE_TASK]} />);

    await user.click(screen.getByRole('button', { name: 'Mark reviewed' }));
    await waitFor(() =>
      expect(api.acknowledgeWorkspaceTask).toHaveBeenCalledWith('ws-1', 't-write')
    );
    expect(api.acknowledgeWorkspaceTask).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  });

  it('opens the log from the row itself, only where there is one to open', async () => {
    const user = userEvent.setup();
    const onViewTask = vi.fn();
    render(<TaskList {...props} onViewTask={onViewTask} tasks={[REVIEW_TASK, WRITE_TASK]} />);

    const [review, write] = screen.getAllByRole('listitem') as [HTMLElement, HTMLElement];
    // Keyboard's way in: the named control, activated on its own, opens once
    // — its click must not also count as a click on the row.
    await user.click(within(review).getByRole('button', { name: 'Open the log of "Review PR"' }));
    expect(onViewTask).toHaveBeenCalledTimes(1);
    expect(onViewTask).toHaveBeenCalledWith(REVIEW_TASK);

    // No session, no transcript: that row is not a click target at all.
    expect(within(write).queryByRole('button', { name: /Open the log/ })).toBeNull();
  });

  it('leaves the row inert when nobody is listening for the log', () => {
    render(<TaskList {...props} tasks={[REVIEW_TASK]} />);
    expect(screen.queryByRole('button', { name: /Open the log/ })).toBeNull();
  });

  it('does not open the row behind a Mark reviewed that is disabled', async () => {
    const user = userEvent.setup();
    const onViewTask = vi.fn();
    api.acknowledgeWorkspaceTask.mockImplementation(() => new Promise<void>(() => {}));
    const first = task({ id: 't-a', title: 'First', status: 'failed', sessionId: 'sess-a' });
    const second = task({ id: 't-b', title: 'Second', status: 'failed', sessionId: 'sess-b' });
    render(<TaskList {...props} onViewTask={onViewTask} tasks={[first, second]} />);

    const [one, two] = screen.getAllByRole('listitem') as [HTMLElement, HTMLElement];
    await user.click(within(one).getByRole('button', { name: 'Mark reviewed' }));
    const blocked = within(two).getByRole('button', { name: 'Mark reviewed' });
    expect(blocked).toBeDisabled();

    // A suppressed click on a disabled control is not a click on the row.
    await user.click(blocked);
    expect(onViewTask).not.toHaveBeenCalled();
  });

  it('opens the log from a click on the row itself, text included', async () => {
    const user = userEvent.setup();
    const onViewTask = vi.fn();
    render(<TaskList {...props} onViewTask={onViewTask} tasks={[REVIEW_TASK]} />);

    // The row's own text is the click target a user aims at.
    await user.click(screen.getByText('Review PR'));
    expect(onViewTask).toHaveBeenCalledWith(REVIEW_TASK);
  });

  it('treats a click that ended a text selection as a copy, not an open', async () => {
    const user = userEvent.setup();
    const onViewTask = vi.fn();
    const failed = task({
      id: 't-failed',
      title: 'Failed one',
      status: 'failed',
      error: 'panicked at src/lib.rs:12',
      sessionId: 'sess-3',
    });
    const selection = vi
      .spyOn(window, 'getSelection')
      .mockReturnValue({ toString: () => 'panicked at src/lib.rs:12' } as unknown as Selection);

    render(<TaskList {...props} onViewTask={onViewTask} tasks={[failed]} />);
    // Selecting the error is the whole point of the row being selectable:
    // the drag must not also open the log on mouse-up.
    await user.click(screen.getByText('panicked at src/lib.rs:12'));
    expect(onViewTask).not.toHaveBeenCalled();

    selection.mockRestore();
    await user.click(screen.getByText('panicked at src/lib.rs:12'));
    expect(onViewTask).toHaveBeenCalledWith(failed);
  });

  it('acknowledges from a row that opens, without also opening it', async () => {
    const user = userEvent.setup();
    const onViewTask = vi.fn();
    const both = task({ id: 't-both', title: 'Failed one', status: 'failed', sessionId: 'sess-2' });
    render(<TaskList {...props} onViewTask={onViewTask} tasks={[both]} />);

    const row = screen.getByRole('listitem');
    expect(
      within(row).getByRole('button', { name: 'Open the log of "Failed one"' })
    ).toBeInTheDocument();
    await user.click(within(row).getByRole('button', { name: 'Mark reviewed' }));
    await waitFor(() =>
      expect(api.acknowledgeWorkspaceTask).toHaveBeenCalledWith('ws-1', 't-both')
    );
    // The row's own action must not catch the acknowledge click.
    expect(onViewTask).not.toHaveBeenCalled();
  });
});
