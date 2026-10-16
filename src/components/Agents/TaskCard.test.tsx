import { describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import TaskCard from './TaskCard';
import type { TaskCallCard } from '../AssistantChat/toolDisplay';
import type { WorkspaceAgentResponse } from '../../generated/bindings';

const agent = (over: Partial<WorkspaceAgentResponse>): WorkspaceAgentResponse => ({
  id: 'wa-review',
  workspaceId: 'ws-1',
  agentDefinitionId: 'def-review',
  displayName: 'Rust Reviewer',
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

const card = (over: Partial<TaskCallCard> = {}): TaskCallCard => ({
  kind: 'assign',
  variant: 'full',
  taskId: 'task-1',
  title: 'Round 3 review',
  status: 'queued',
  assignedToWorkspaceAgentId: 'wa-review',
  assignedAgentDefinitionId: 'def-review',
  assignedAgentDisplayName: '',
  error: '',
  ...over,
});

const roster = [agent({})];

describe('TaskCard', () => {
  it('shows who the task went to and its title', () => {
    render(<TaskCard card={card()} roster={roster} />);
    expect(screen.getByText('Rust Reviewer')).toBeInTheDocument();
    expect(screen.getByText('Delegated to')).toBeInTheDocument();
    expect(screen.getByText('Round 3 review')).toBeInTheDocument();
  });

  it('labels no status on a hand-off, whatever the row was stamped with', () => {
    // A delegation's status is the call racing the worker — `queued`, or
    // `completed` if the row was stamped first — so a label there would report
    // the race, frozen forever. The tasks drawer owns what is happening now.
    for (const [status, label] of [
      ['queued', 'Queued'],
      ['running', 'Running'],
      ['completed', 'Completed'],
      ['failed', 'Failed'],
    ]) {
      const { container } = render(<TaskCard card={card({ status })} roster={roster} />);
      expect(container.textContent).not.toContain(label);
      cleanup();
    }
  });

  it('keeps the status on a poll, which went and looked at the task', () => {
    render(<TaskCard card={card({ kind: 'poll', status: 'completed' })} roster={roster} />);
    expect(screen.getByText('Completed')).toBeInTheDocument();
  });

  it('names the workspace main "Main" whatever the definition calls it', () => {
    render(
      <TaskCard
        card={card()}
        roster={[agent({ isDefault: true, displayName: 'Manager Claude' })]}
      />
    );
    expect(screen.getByText('Main')).toBeInTheDocument();
  });

  it('still draws a card for an agent that has left the crew', () => {
    render(<TaskCard card={card()} roster={[]} />);
    expect(screen.getByText('Agent')).toBeInTheDocument();
    expect(screen.getByText('Round 3 review')).toBeInTheDocument();
  });

  it('names an agent that has left the crew as the call saw it', () => {
    render(<TaskCard card={card({ assignedAgentDisplayName: 'Old Reviewer' })} roster={[]} />);
    expect(screen.getByText('Old Reviewer')).toBeInTheDocument();
  });

  it('opens the task it describes, by id', async () => {
    const onOpen = vi.fn();
    render(<TaskCard card={card()} roster={roster} onOpen={onOpen} />);
    await userEvent.click(screen.getByRole('button', { name: /Round 3 review/ }));
    expect(onOpen).toHaveBeenCalledWith('task-1');
  });

  it('renders inert with no handler, so a read-only transcript offers no dead click', () => {
    render(<TaskCard card={card()} roster={roster} />);
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });

  it('keeps a slim card to one line, and still opens the task', async () => {
    const onOpen = vi.fn();
    render(
      <TaskCard
        card={card({ kind: 'poll', variant: 'slim', status: 'running' })}
        roster={roster}
        onOpen={onOpen}
      />
    );
    expect(screen.getByText('Round 3 review')).toBeInTheDocument();
    expect(screen.getByText('Running')).toBeInTheDocument();
    // The slim line drops the name and the error; the face and title carry it.
    expect(screen.queryByText('Rust Reviewer')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Round 3 review/ }));
    expect(onOpen).toHaveBeenCalledWith('task-1');
  });

  it('draws no spinning ring on a queued hand-off', () => {
    const { container } = render(<TaskCard card={card()} roster={roster} />);
    const face = container.querySelector('[data-activity]');
    expect(face?.getAttribute('data-activity')).toBe('none');
  });

  it('shows a failed task\'s error under its status', () => {
    render(
      <TaskCard
        card={card({ kind: 'poll', status: 'failed', error: 'Worker run record not found' })}
        roster={roster}
      />
    );
    expect(screen.getByText('Worker run record not found')).toBeInTheDocument();
    expect(screen.getByText('Failed')).toBeInTheDocument();
  });

  it('keeps the whole card readable as the button name', () => {
    // An aria-label here would hide the status and the error from a screen
    // reader behind a shorter sentence. A poll card carries both.
    render(
      <TaskCard card={card({ kind: 'poll', status: 'blocked' })} roster={roster} onOpen={vi.fn()} />
    );
    const button = screen.getByRole('button');
    expect(button).not.toHaveAttribute('aria-label');
    expect(button.textContent).toContain('Rust Reviewer');
    expect(button.textContent).toContain('Blocked');
  });
});
