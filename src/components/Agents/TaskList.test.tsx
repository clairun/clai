import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const api = vi.hoisted(() => ({ acknowledgeWorkspaceTask: vi.fn(), stopWorkspaceTask: vi.fn() }));
vi.mock('../../workspace/client', () => api);

import TaskList, { filterFaces, taskActivity } from './TaskList';
import { useTaskStore } from '../../workspace/taskStore';
import AgentAvatar from './AgentAvatar';
import { identityFor, mainIdentity, taskIdentity, type AgentActivity } from './agentIdentity';
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';

const agent = (over: Partial<WorkspaceAgentResponse>): WorkspaceAgentResponse => ({
  id: 'wa',
  workspaceId: 'ws-1',
  agentDefinitionId: 'def',
  displayName: 'Agent',
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
  runId: 'run-1',
  createdAt: 1n,
  updatedAt: 1n,
  completedAt: null,
  attentionAcknowledgedAt: null,
  failureReason: null,
  parentSessionId: null,
  parentRunId: null,
  parentTaskId: null,
  depth: 0n,
  cycleNo: 0n,
  stopRequestedAt: null,
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
  status: 'failed',
  failureReason: 'blocked',
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
    expect(taskActivity(task({ status: 'running' }))).toBe('running');
    expect(taskActivity(task({ status: 'running', runId: null }))).toBe('idle');
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

describe('task status display', () => {
  const props = { workspaceId: 'ws-1', roster: ROSTER, onChanged: vi.fn() };

  it('failed+blocked shows Blocked pill and Mark reviewed', () => {
    render(<TaskList {...props} tasks={[WRITE_TASK]} />);
    expect(screen.getByText('Blocked')).toBeInTheDocument();
    expect(screen.queryByText('Failed')).toBeNull();
    expect(screen.getByRole('button', { name: 'Mark reviewed' })).toBeInTheDocument();
  });

  it('failed+restart reads Failed · interrupted', () => {
    render(
      <TaskList
        {...props}
        tasks={[task({ id: 't-r', status: 'failed', failureReason: 'restart' })]}
      />
    );
    expect(screen.getByText('Failed · interrupted')).toBeInTheDocument();
  });

  it('running with no run reads Waiting for subtasks and can still be stopped', () => {
    render(
      <TaskList
        {...props}
        tasks={[task({ id: 't-w', title: 'Parent', status: 'running', runId: null })]}
      />
    );
    expect(screen.getByText('Waiting for subtasks')).toBeInTheDocument();
    expect(screen.queryByText('Running')).toBeNull();
    expect(screen.getByRole('button', { name: 'Stop Parent' })).toBeEnabled();
  });
});

describe('task tree', () => {
  it('nests subtasks under their parent and collapses them', async () => {
    const parent = task({ id: 't-p', title: 'Parent' });
    const child = task({ id: 't-c', title: 'Child', parentTaskId: 't-p' });
    render(
      <TaskList workspaceId="ws-1" roster={ROSTER} onChanged={vi.fn()} tasks={[child, parent]} />
    );
    const titles = () => screen.getAllByText(/^(Parent|Child)$/).map((el) => el.textContent);
    expect(titles()).toEqual(['Parent', 'Child']);

    const toggle = screen.getByRole('button', { name: '1 subtask' });
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(titles()).toEqual(['Parent']);
  });
});

describe('task stop', () => {
  const props = { workspaceId: 'ws-1', roster: ROSTER, onChanged: vi.fn() };
  beforeEach(() => {
    api.stopWorkspaceTask.mockReset();
    useTaskStore.setState({ tasks: {}, seeded: {} });
  });

  it('stop visible for queued/running', () => {
    const statuses = [
      'queued',
      'running',
      'completed',
      'failed',
      'stopped',
    ] as const;
    render(
      <TaskList
        {...props}
        tasks={statuses.map((status) => task({ id: status, title: status, status }))}
      />
    );
    const stops = screen
      .getAllByRole('button', { name: /^Stop / })
      .map((b) => b.getAttribute('aria-label'));
    expect(stops).toEqual(['Stop queued', 'Stop running']);
  });

  it('stop disables while stopping and survives reload via status', async () => {
    const running = task({ id: 't-s', title: 'Review', status: 'running' });
    const stopping = {
      ...running,
      status: 'stopping' as const,
      updatedAt: 2n,
      stopRequestedAt: 2n,
    };
    let resolve: (t: WorkspaceTaskResponse) => void = () => {};
    api.stopWorkspaceTask.mockReturnValue(new Promise((r) => (resolve = r)));
    const { unmount } = render(<TaskList {...props} tasks={[running]} />);
    const stop = screen.getByRole('button', { name: 'Stop Review' });
    await userEvent.click(stop);
    expect(api.stopWorkspaceTask).toHaveBeenCalledWith('ws-1', 't-s');
    expect(stop).toBeDisabled();
    resolve(stopping);
    await waitFor(() =>
      expect(useTaskStore.getState().tasks['ws-1']?.['t-s']?.status).toBe('stopping')
    );
    unmount();

    // A fresh mount has no click latch; the durable status alone disables it.
    render(<TaskList {...props} tasks={[stopping]} />);
    expect(screen.getByText('Stopping…')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop Review' })).toBeDisabled();
  });

  it('stopped rows name a restart and leave the resume hint to the composer', () => {
    render(
      <TaskList
        {...props}
        tasks={[
          task({ id: 'a', status: 'stopped' }),
          task({ id: 'b', status: 'stopped', error: 'task interrupted by app restart' }),
        ]}
      />
    );
    expect(screen.getByText('Interrupted by restart')).toBeInTheDocument();
    expect(screen.queryByText(/send a message to resume/i)).toBeNull();
    expect(screen.queryByText('task interrupted by app restart')).toBeNull();
  });

  it('stop is enabled again once a stopped task resumes', async () => {
    const running = task({ id: 't-s', title: 'Review', status: 'running' });
    api.stopWorkspaceTask.mockResolvedValue({ ...running, status: 'stopping', updatedAt: 2n });
    const { rerender } = render(<TaskList {...props} tasks={[running]} />);
    await userEvent.click(screen.getByRole('button', { name: 'Stop Review' }));
    await waitFor(() =>
      expect(useTaskStore.getState().tasks['ws-1']?.['t-s']?.status).toBe('stopping')
    );
    rerender(<TaskList {...props} tasks={[{ ...running, status: 'stopping', updatedAt: 2n }]} />);
    expect(screen.getByRole('button', { name: 'Stop Review' })).toBeDisabled();
    rerender(<TaskList {...props} tasks={[{ ...running, status: 'stopped', updatedAt: 3n }]} />);
    rerender(
      <TaskList
        {...props}
        tasks={[{ ...running, cycleNo: 2n, status: 'running', updatedAt: 4n }]}
      />
    );
    expect(screen.getByRole('button', { name: 'Stop Review' })).toBeEnabled();
  });

  it('already_terminal shows no error', async () => {
    const running = task({ id: 't-d', title: 'Done', status: 'running' });
    api.stopWorkspaceTask.mockResolvedValue({ ...running, status: 'completed', updatedAt: 2n });
    render(<TaskList {...props} tasks={[running]} />);
    await userEvent.click(screen.getByRole('button', { name: 'Stop Done' }));
    await waitFor(() =>
      expect(useTaskStore.getState().tasks['ws-1']?.['t-d']?.status).toBe('completed')
    );
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('a rejected stop re-enables the button and shows the error', async () => {
    api.stopWorkspaceTask.mockRejectedValue('task not found');
    render(<TaskList {...props} tasks={[task({ id: 't-x', title: 'Gone', status: 'running' })]} />);
    const stop = screen.getByRole('button', { name: 'Stop Gone' });
    await userEvent.click(stop);
    expect(await screen.findByRole('alert')).toHaveTextContent('task not found');
    expect(stop).toBeEnabled();
  });

  it('a stop error does not outlive its cycle', async () => {
    const running = task({ id: 't-x', title: 'Gone', status: 'running' });
    api.stopWorkspaceTask.mockRejectedValue('transport closed');
    const { rerender } = render(<TaskList {...props} tasks={[running]} />);
    await userEvent.click(screen.getByRole('button', { name: 'Stop Gone' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('transport closed');
    rerender(<TaskList {...props} tasks={[{ ...running, cycleNo: 2n, updatedAt: 2n }]} />);
    expect(screen.queryByRole('alert')).toBeNull();
  });
});
