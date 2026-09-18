import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import TaskCard, { cardActivity } from './TaskCard';
import type { TaskCallCard } from '../AssistantChat/toolDisplay';
import type { WorkspaceAgentResponse } from '../../generated/bindings';

const agent = (over: Partial<WorkspaceAgentResponse>): WorkspaceAgentResponse => ({
  id: 'wa-review',
  workspaceId: 'ws-1',
  agentDefinitionId: 'def-review',
  displayName: 'Rust Reviewer',
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

const card = (over: Partial<TaskCallCard> = {}): TaskCallCard => ({
  kind: 'assign',
  variant: 'full',
  taskId: 'task-1',
  title: 'Round 3 review',
  instructions: 'Review the branch end to end.',
  status: 'queued',
  assignedToWorkspaceAgentId: 'wa-review',
  assignedAgentDefinitionId: 'def-review',
  detail: '',
  detailIsError: false,
  ...over,
});

const roster = [agent({})];

describe('TaskCard', () => {
  it('shows who the task went to, what it is, and where it stood', () => {
    render(<TaskCard card={card()} roster={roster} />);
    expect(screen.getByText('Rust Reviewer')).toBeInTheDocument();
    expect(screen.getByText('Delegated to')).toBeInTheDocument();
    expect(screen.getByText('Round 3 review')).toBeInTheDocument();
    expect(screen.getByText('Queued')).toBeInTheDocument();
    expect(screen.getByText('Review the branch end to end.')).toBeInTheDocument();
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

  it('leads a poll with its answer rather than the instructions', () => {
    render(
      <TaskCard
        card={card({
          kind: 'poll',
          status: 'completed',
          detail: 'Found 3 issues.',
        })}
        roster={roster}
      />
    );
    expect(screen.getByText('Found 3 issues.')).toBeInTheDocument();
    expect(screen.queryByText('Delegated to')).not.toBeInTheDocument();
    expect(screen.queryByText('Review the branch end to end.')).not.toBeInTheDocument();
  });

  it('falls back to the instructions when a task answered with nothing', () => {
    render(<TaskCard card={card({ kind: 'poll', status: 'completed' })} roster={roster} />);
    expect(screen.getByText('Review the branch end to end.')).toBeInTheDocument();
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
    // The slim line drops the name and the body; the face and title carry it.
    expect(screen.queryByText('Review the branch end to end.')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Round 3 review/ }));
    expect(onOpen).toHaveBeenCalledWith('task-1');
  });

  it('rings a face by the status frozen on the card', () => {
    // No acknowledgement fields travel in a tool payload, so any stop that is
    // not a clean finish reads as needing attention.
    expect(cardActivity('queued')).toBe('running');
    expect(cardActivity('running')).toBe('running');
    expect(cardActivity('completed')).toBe('none');
    expect(cardActivity('failed')).toBe('attention');
    expect(cardActivity('blocked')).toBe('attention');
  });
});
