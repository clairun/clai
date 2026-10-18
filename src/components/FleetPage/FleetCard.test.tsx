import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { emptyCard, type FleetLive } from '../../fleet/fleetModel';
import type { WorkspaceTaskResponse } from '../../generated/bindings';
import { LiveFaces } from './FleetCard';

const task = (n: number): WorkspaceTaskResponse =>
  ({
    id: `t${n}`,
    workspaceId: 'ws',
    title: `Task ${n}`,
    status: 'running',
    runId: null,
    parentTaskId: null,
    assignedAgentDisplayName: `Agent ${n}`,
    assignedAgentDefinitionId: `def-${n}`,
    assignedToWorkspaceAgentId: `wa-${n}`,
    updatedAt: BigInt(n),
  }) as unknown as WorkspaceTaskResponse;

const liveWith = (count: number): FleetLive => ({
  runs: {},
  tasks: { ws: Object.fromEntries(Array.from({ length: count }, (_, i) => [`t${i}`, task(i)])) },
});

describe('LiveFaces', () => {
  it('shows four faces and "+1" for a fifth agent', () => {
    render(<LiveFaces card={emptyCard('ws')} live={liveWith(5)} faces={{}} />);
    expect(screen.getByLabelText("Agent 0: running 'Task 0'")).toBeTruthy();
    expect(screen.queryByLabelText("Agent 4: running 'Task 4'")).toBeNull();
    expect(screen.getByText('+1')).toBeTruthy();
  });

  it('renders nothing when nothing is live', () => {
    const { container } = render(<LiveFaces card={emptyCard('ws')} live={liveWith(0)} faces={{}} />);
    expect(container.innerHTML).toBe('');
  });
});
