import { describe, expect, it } from 'vitest';
import { buildTaskTree, type TaskNode } from './taskTree';
import type { WorkspaceTaskResponse } from '../generated/bindings';

const task = (id: string, parentTaskId: string | null, createdAt = 1n) =>
  ({ id, parentTaskId, createdAt }) as WorkspaceTaskResponse;

const shape = (nodes: TaskNode[]): unknown =>
  nodes.map((node) => (node.children.length ? { [node.task.id]: shape(node.children) } : node.task.id));

describe('buildTaskTree', () => {
  it('buildTaskTree nests by parentTaskId', () => {
    const tree = buildTaskTree([
      task('b2', 'a', 3n),
      task('a', null),
      task('b1', 'a', 2n),
      task('c', 'b1'),
      task('z', null),
    ]);
    expect(shape(tree)).toEqual([{ a: [{ b1: ['c'] }, 'b2'] }, 'z']);
  });

  it('orphan becomes root', () => {
    expect(shape(buildTaskTree([task('child', 'outside-window'), task('x', null)]))).toEqual([
      'child',
      'x',
    ]);
  });

  it('cycle does not loop', () => {
    const tree = buildTaskTree([task('a', 'b'), task('b', 'a'), task('self', 'self')]);
    expect(shape(tree)).toEqual([{ a: ['b'] }, 'self']);
  });
});
