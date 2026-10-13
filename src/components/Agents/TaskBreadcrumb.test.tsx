import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import TaskBreadcrumb from './TaskBreadcrumb';
import { useTaskStore } from '../../workspace/taskStore';
import type { WorkspaceTaskResponse } from '../../generated/bindings';

const task = (id: string, title: string, parentTaskId: string | null) =>
  ({ id, title, parentTaskId, workspaceId: 'ws-1', updatedAt: 1n }) as WorkspaceTaskResponse;

describe('TaskBreadcrumb', () => {
  beforeEach(() => useTaskStore.setState({ tasks: {}, seeded: {} }));

  it('breadcrumb opens ancestor', async () => {
    const root = task('t-root', 'Review PR #231', null);
    const middle = task('t-mid', 'Fix parse()', 't-root');
    const leaf = task('t-leaf', 'Add null test', 't-mid');
    useTaskStore.getState().seedTasks('ws-1', [root, middle, leaf]);
    const onOpenTask = vi.fn();
    render(<TaskBreadcrumb task={leaf} onOpenTask={onOpenTask} />);

    const trail = screen.getByRole('navigation', { name: 'Task ancestry' });
    expect(trail).toHaveTextContent('Review PR #231Fix parse()Add null test');
    await userEvent.click(screen.getByRole('button', { name: 'Review PR #231' }));
    expect(onOpenTask).toHaveBeenCalledWith('t-root');
    expect(screen.queryByRole('button', { name: 'Add null test' })).toBeNull();
  });

  it('marks an ancestor outside the store with …', () => {
    const leaf = task('t-leaf', 'Add null test', 't-gone');
    render(<TaskBreadcrumb task={leaf} onOpenTask={vi.fn()} />);
    expect(screen.getByRole('navigation', { name: 'Task ancestry' })).toHaveTextContent(
      '…Add null test'
    );
  });
});
